//! Capture a CPU FLIP preset for bounded offline/preview timing.
//!
//! The frame CSVs keep render CPU, command-buffer wait, whole-buffer GPU, and
//! native solver metrics separate. Offline is a complete-step replay using the
//! native fixed 60 Hz simulation; output frames may use a lower capture FPS.
//! preview records the latest completed native snapshot, so repeated scalar
//! values are expected when the worker is behind. Each stepped offline frame
//! contributes one raw RGBA frame to `offline.rgba`; readback and file writes
//! happen after that frame's timer and are reported as `capture_ms`.
//!
//! Build with:
//!
//! `cargo build --profile test --features gpu-proofs --example fluid_capture`
//!
//! Run with one output directory and optional `--preset`, `--width`, `--height`,
//! `--frames`, `--fps`, `--offline-only`, `--max-seconds`, `--stills-every`,
//! and `--linear` flags. Defaults preserve the shipped Water Basin workflow.

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use manifold_core::NodeId;
use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, readback_srgb_rgba8, readback_tonemapped_rgba8,
};
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde::Serialize;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_FRAMES: u32 = 480;
const DEFAULT_FPS: u32 = 60;
const DEFAULT_MAX_SECONDS: f64 = 180.0;
const FIXED_HZ: f64 = 60.0;
const PREVIEW_SECONDS: f64 = 8.0;
const PREVIEW_MAX_SECONDS: f64 = 10.0;
const MAX_WIDTH: u32 = 3840;
const MAX_HEIGHT: u32 = 2160;
const MAX_FRAMES: u32 = 600;
const MAX_FPS: u32 = 60;
const MAX_SECONDS: f64 = 900.0;
const CSV_HEADER: &str = "frame,authored_time,simulation_time,lag_seconds,render_cpu_ms,submit_wait_ms,gpu_ms,frame_ms,simulation_ms,meshing_ms,particle_count,vertex_count,capture_ms,presentation_interval_ms,foam_count,bubble_count,spray_count";
const METRIC_NAMES: [&str; 9] = [
    "simulation_time",
    "lag_seconds",
    "simulation_ms",
    "meshing_ms",
    "particle_count",
    "vertex_count",
    "foam_count",
    "bubble_count",
    "spray_count",
];

type CaptureResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Debug)]
struct CaptureOptions {
    output_dir: PathBuf,
    preset_path: PathBuf,
    width: u32,
    height: u32,
    frames: u32,
    fps: u32,
    offline_only: bool,
    max_seconds: f64,
    stills_every: Option<u32>,
    linear: bool,
}

#[derive(Clone, Copy, Debug)]
struct PresetSettings {
    resolution: u32,
    domain_size: f64,
    surface_detail: u32,
}

#[derive(Clone, Copy, Debug)]
struct FluidMetrics {
    simulation_time: f64,
    lag_seconds: f64,
    simulation_ms: f64,
    meshing_ms: f64,
    particle_count: f64,
    vertex_count: f64,
    foam_count: f64,
    bubble_count: f64,
    spray_count: f64,
}

#[derive(Clone, Copy, Debug)]
struct FrameTimings {
    render_cpu_ms: f64,
    submit_wait_ms: f64,
    gpu_ms: f64,
    frame_ms: f64,
}

#[derive(Clone, Copy, Debug)]
struct MetricRow {
    frame: u32,
    authored_time: f64,
    fluid: FluidMetrics,
    timings: FrameTimings,
    capture_ms: f64,
    presentation_interval_ms: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct InitialFrameTimings {
    render_cpu_ms: f64,
    submit_wait_ms: f64,
    gpu_ms: f64,
    frame_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct PassMetadata {
    frames: u32,
    fps: u32,
    initial_frame: InitialFrameTimings,
    start_simulation_time: f64,
    end_simulation_time: f64,
    capture_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct PreviewMetadata {
    elapsed_s: f64,
    actual_frames: u32,
    fps: u32,
    initial_frame: InitialFrameTimings,
    accepted_fluid_frames: u32,
    total_sim_ticks_advanced: u64,
    start_simulation_time: f64,
    end_simulation_time: f64,
    final_authored_time: f64,
    final_lag_seconds: f64,
}

#[derive(Clone, Debug, Serialize)]
struct Metadata {
    preset_path: String,
    gpu_device_name: String,
    width: u32,
    height: u32,
    grid: [u32; 3],
    cell_size: f64,
    surface_detail: u32,
    native_fixed_hz: f64,
    capture_fps: u32,
    display_transform: &'static str,
    offline_only: bool,
    stills_every: Option<u32>,
    offline: PassMetadata,
    preview: Option<PreviewMetadata>,
}

fn default_preset_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("generator-presets")
        .join("WaterBasin.json")
}

fn read_preset(path: &Path) -> CaptureResult<String> {
    fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read preset {}: {error}", path.display()),
        )
        .into()
    })
}

fn preset_number(node: &serde_json::Value, name: &str) -> CaptureResult<f64> {
    node["params"][name]["value"].as_f64().ok_or_else(|| {
        io::Error::other(format!(
            "fluid_surface parameter `{name}` is missing or non-numeric"
        ))
        .into()
    })
}

fn preset_settings(json: &str) -> CaptureResult<PresetSettings> {
    let document: serde_json::Value = serde_json::from_str(json)?;
    let fluid = document["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["nodeId"] == "fluid_surface"))
        .ok_or_else(|| io::Error::other("preset has no fluid_surface node"))?;
    let resolution = preset_number(fluid, "resolution")?;
    let domain_size = preset_number(fluid, "domain_size")?;
    let surface_detail = preset_number(fluid, "surface_subdivisions")?;
    if !resolution.is_finite()
        || !domain_size.is_finite()
        || !surface_detail.is_finite()
        || resolution < 1.0
        || domain_size <= 0.0
        || surface_detail < 0.0
    {
        return Err(io::Error::other("preset fluid settings are invalid").into());
    }
    Ok(PresetSettings {
        resolution: resolution.round() as u32,
        domain_size,
        surface_detail: surface_detail.round() as u32,
    })
}

fn build_runtime(
    json: &str,
    device: &Arc<GpuDevice>,
    width: u32,
    height: u32,
) -> CaptureResult<PresetRuntime> {
    // The compiler omits unwired scalar outputs. Diagnostic consumers retain
    // their bindings on the live fluid node; these orphan math nodes are not
    // executed. The shipped preset, simulation and rendering stay unchanged.
    let mut instrumented: serde_json::Value = serde_json::from_str(json)?;
    let nodes = instrumented["nodes"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("Water Basin nodes must be an array"))?;
    let fluid_id = nodes
        .iter()
        .find(|node| node["nodeId"] == "fluid_surface")
        .and_then(|node| node["id"].as_u64())
        .ok_or_else(|| io::Error::other("Water Basin fluid_surface node is missing"))?;
    let first_probe_id = nodes
        .iter()
        .filter_map(|node| node["id"].as_u64())
        .max()
        .unwrap_or(0)
        + 1;
    for (index, name) in METRIC_NAMES.iter().enumerate() {
        nodes.push(serde_json::json!({
            "id": first_probe_id + index as u64,
            "nodeId": format!("capture_metric_{name}"),
            "typeId": "node.math",
            "handle": format!("Capture {name}"),
        }));
    }
    let wires = instrumented["wires"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("Water Basin wires must be an array"))?;
    for (index, name) in METRIC_NAMES.iter().enumerate() {
        wires.push(serde_json::json!({
            "fromNode": fluid_id, "fromPort": name,
            "toNode": first_probe_id + index as u64, "toPort": "a",
        }));
    }
    let instrumented = serde_json::to_string(&instrumented)?;
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &instrumented,
        &registry,
        Arc::clone(device),
        width,
        height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .map_err(|error| io::Error::other(format!("build Water Basin runtime: {error}")))?;
    let fluid_node = NodeId::from("fluid_surface");
    runtime.set_preview_node(Some(&fluid_node));
    Ok(runtime)
}

fn context(frame: u32, authored_time: f64, dt: f64, width: u32, height: u32) -> PresetContext {
    PresetContext {
        time: authored_time,
        beat: authored_time * 2.0,
        dt: dt as f32,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: i64::from(frame),
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn read_fluid_metrics(runtime: &PresetRuntime) -> CaptureResult<FluidMetrics> {
    let (_, outputs) = runtime.preview_scalar_io();
    let mut values = [0.0; METRIC_NAMES.len()];
    for (index, name) in METRIC_NAMES.iter().enumerate() {
        let value = outputs
            .iter()
            .find_map(|(port, value)| (port == name).then_some(*value))
            .ok_or_else(|| io::Error::other(format!("fluid_surface output `{name}` is missing")))?;
        if !value.is_finite() {
            return Err(io::Error::other(format!(
                "fluid_surface output `{name}` is non-finite: {value}"
            ))
            .into());
        }
        values[index] = f64::from(value);
    }
    Ok(FluidMetrics {
        simulation_time: values[0],
        lag_seconds: values[1],
        simulation_ms: values[2],
        meshing_ms: values[3],
        particle_count: values[4],
        vertex_count: values[5],
        foam_count: values[6],
        bubble_count: values[7],
        spray_count: values[8],
    })
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    frame: u32,
    authored_time: f64,
    dt: f64,
    options: &CaptureOptions,
) -> CaptureResult<(FrameTimings, FluidMetrics)> {
    let frame_started = Instant::now();
    let mut encoder = device.create_encoder("fluid-capture");
    let render_started = Instant::now();
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context(frame, authored_time, dt, options.width, options.height),
            &ParamManifest::default(),
        );
        gpu.frame_status()
    };
    let render_cpu_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    if status != FrameRenderStatus::Complete {
        return Err(io::Error::other(format!(
            "Water Basin frame {frame} failed with status {status:?}"
        ))
        .into());
    }
    let submit_started = Instant::now();
    let profile = encoder.commit_and_wait_profiled(device);
    let submit_wait_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
    if profile.failed_command_buffers != 0 {
        return Err(io::Error::other(format!(
            "Water Basin frame {frame} had {} failed command buffers",
            profile.failed_command_buffers
        ))
        .into());
    }
    let timings = FrameTimings {
        render_cpu_ms,
        submit_wait_ms,
        gpu_ms: profile.total_ms,
        frame_ms: frame_started.elapsed().as_secs_f64() * 1000.0,
    };
    for (label, value) in [
        ("render_cpu_ms", timings.render_cpu_ms),
        ("submit_wait_ms", timings.submit_wait_ms),
        ("gpu_ms", timings.gpu_ms),
        ("frame_ms", timings.frame_ms),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(io::Error::other(format!(
                "Water Basin frame {frame} has invalid {label}: {value}"
            ))
            .into());
        }
    }
    // Keep scalar-output copying outside the frame timer. The executor's
    // internal preview capture remains part of runtime.render above.
    let fluid = read_fluid_metrics(runtime)?;
    Ok((timings, fluid))
}

fn metric_row(
    frame: u32,
    authored_time: f64,
    timings: FrameTimings,
    fluid: FluidMetrics,
    presentation_interval_ms: f64,
) -> CaptureResult<MetricRow> {
    let row = MetricRow {
        frame,
        authored_time,
        fluid,
        timings,
        capture_ms: 0.0,
        presentation_interval_ms,
    };
    if !authored_time.is_finite() || !presentation_interval_ms.is_finite() {
        return Err(io::Error::other(format!(
            "frame {frame} has invalid authored or presentation time"
        ))
        .into());
    }
    Ok(row)
}

fn write_csv(path: &Path, rows: &[MetricRow]) -> CaptureResult<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, "{CSV_HEADER}")?;
    for row in rows {
        writeln!(
            writer,
            "{},{:.9},{:.9},{:.9},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.3},{:.3},{:.6},{:.6},{:.0},{:.0},{:.0}",
            row.frame,
            row.authored_time,
            row.fluid.simulation_time,
            row.fluid.lag_seconds,
            row.timings.render_cpu_ms,
            row.timings.submit_wait_ms,
            row.timings.gpu_ms,
            row.timings.frame_ms,
            row.fluid.simulation_ms,
            row.fluid.meshing_ms,
            row.fluid.particle_count,
            row.fluid.vertex_count,
            row.capture_ms,
            row.presentation_interval_ms,
            row.fluid.foam_count,
            row.fluid.bubble_count,
            row.fluid.spray_count,
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn ensure_wall_limit(started: Instant, label: &str, max_seconds: f64) -> CaptureResult<()> {
    let elapsed = started.elapsed().as_secs_f64();
    if elapsed > max_seconds {
        return Err(io::Error::other(format!(
            "{label} exceeded wall limit of {max_seconds:.0}s ({elapsed:.3}s)"
        ))
        .into());
    }
    Ok(())
}

fn parse_u32(value: &str, flag: &str) -> CaptureResult<u32> {
    value
        .parse()
        .map_err(|_| io::Error::other(format!("{flag} expects an unsigned integer")).into())
}

fn parse_options() -> CaptureResult<CaptureOptions> {
    let mut args = std::env::args().skip(1);
    let mut output_dir = None;
    let mut options = CaptureOptions {
        output_dir: PathBuf::new(),
        preset_path: default_preset_path(),
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
        frames: DEFAULT_FRAMES,
        fps: DEFAULT_FPS,
        offline_only: false,
        max_seconds: DEFAULT_MAX_SECONDS,
        stills_every: None,
        linear: false,
    };
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| -> CaptureResult<String> {
            args.next()
                .ok_or_else(|| io::Error::other(format!("{flag} requires a value")).into())
        };
        match arg.as_str() {
            "--preset" => options.preset_path = PathBuf::from(value("--preset")?),
            "--width" => options.width = parse_u32(&value("--width")?, "--width")?,
            "--height" => options.height = parse_u32(&value("--height")?, "--height")?,
            "--frames" => options.frames = parse_u32(&value("--frames")?, "--frames")?,
            "--fps" => options.fps = parse_u32(&value("--fps")?, "--fps")?,
            "--max-seconds" => {
                let raw = value("--max-seconds")?;
                options.max_seconds = raw
                    .parse()
                    .map_err(|_| io::Error::other("--max-seconds expects a number"))?;
            }
            "--stills-every" => {
                let every = parse_u32(&value("--stills-every")?, "--stills-every")?;
                options.stills_every = Some(every);
            }
            "--offline-only" => options.offline_only = true,
            "--linear" => options.linear = true,
            flag if flag.starts_with('-') => {
                return Err(io::Error::other(format!("unsupported argument `{flag}`")).into());
            }
            path if output_dir.is_none() => output_dir = Some(PathBuf::from(path)),
            path => {
                return Err(
                    io::Error::other(format!("unexpected positional argument `{path}`")).into(),
                );
            }
        }
    }
    options.output_dir = output_dir.ok_or_else(|| {
        io::Error::other(
            "usage: fluid_capture OUTPUT_DIR [--preset PATH] [--width N] [--height N] [--frames N] [--fps N] [--offline-only] [--max-seconds N] [--stills-every N] [--linear]",
        )
    })?;
    if options.width == 0
        || options.width > MAX_WIDTH
        || options.height == 0
        || options.height > MAX_HEIGHT
    {
        return Err(io::Error::other(format!(
            "dimensions must be positive and at most {MAX_WIDTH}x{MAX_HEIGHT}"
        ))
        .into());
    }
    if options.frames == 0 || options.frames > MAX_FRAMES {
        return Err(io::Error::other(format!("frames must be in 1..={MAX_FRAMES}")).into());
    }
    if options.fps == 0 || options.fps > MAX_FPS {
        return Err(io::Error::other(format!("fps must be in 1..={MAX_FPS}")).into());
    }
    if !options.max_seconds.is_finite()
        || options.max_seconds <= 0.0
        || options.max_seconds > MAX_SECONDS
    {
        return Err(
            io::Error::other(format!("max-seconds must be in (0,{MAX_SECONDS:.0}]")).into(),
        );
    }
    if options.stills_every == Some(0) {
        return Err(io::Error::other("stills-every must be positive").into());
    }
    Ok(options)
}

fn readback_rgba(
    device: &GpuDevice,
    target: &RenderTarget,
    width: u32,
    height: u32,
    linear: bool,
) -> Vec<u8> {
    if linear {
        readback_srgb_rgba8(device, &target.texture, width, height)
    } else {
        readback_tonemapped_rgba8(device, &target.texture, width, height)
    }
}

fn warmup_assets(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    options: &CaptureOptions,
    overall_started: Instant,
) -> CaptureResult<()> {
    let started = Instant::now();
    while runtime.warmup_pending() {
        ensure_wall_limit(started, "asset warmup", 30.0)?;
        ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
        thread::sleep(Duration::from_millis(10));
        // Asset IO must settle without advancing the fluid or authored clock.
        render_frame(runtime, target, device, 0, 0.0, 0.0, options)?;
    }
    Ok(())
}

fn run(options: &CaptureOptions) -> CaptureResult<()> {
    fs::create_dir_all(&options.output_dir)?;
    let overall_started = Instant::now();
    let json = read_preset(&options.preset_path)?;
    fs::write(options.output_dir.join("preset.json"), &json)?;
    let preset = preset_settings(&json)?;
    let frame_dt = 1.0 / f64::from(options.fps);
    let device = Arc::new(GpuDevice::new());
    let offline_target = RenderTarget::new(
        &device,
        options.width,
        options.height,
        GpuTextureFormat::Rgba16Float,
        "fluid-capture-offline",
    );
    let offline_scope = PhysicsStepScope::for_render(true);
    let mut offline_runtime = build_runtime(&json, &device, options.width, options.height)?;
    let (initial_timings, initial_fluid) = render_frame(
        &mut offline_runtime,
        &offline_target,
        &device,
        0,
        0.0,
        frame_dt,
        options,
    )?;
    warmup_assets(
        &mut offline_runtime,
        &offline_target,
        &device,
        options,
        overall_started,
    )?;
    if initial_fluid.simulation_time.abs() > 1e-4 {
        return Err(io::Error::other(format!(
            "offline initial simulation time is not zero: {}",
            initial_fluid.simulation_time
        ))
        .into());
    }

    let mut offline_rows = Vec::with_capacity(options.frames as usize);
    let mut offline_rgba_writer = match options.stills_every {
        None => Some(BufWriter::new(File::create(
            options.output_dir.join("offline.rgba"),
        )?)),
        Some(_) => None,
    };
    if options.stills_every.is_some() {
        fs::create_dir_all(options.output_dir.join("stills"))?;
    }
    let mut capture_ms = 0.0;
    for frame in 1..=options.frames {
        ensure_wall_limit(overall_started, "offline pass", options.max_seconds)?;
        let authored_time = f64::from(frame) * frame_dt;
        let (timings, fluid) = render_frame(
            &mut offline_runtime,
            &offline_target,
            &device,
            frame,
            authored_time,
            frame_dt,
            options,
        )?;
        if fluid.vertex_count < 3.0 {
            return Err(io::Error::other(format!(
                "offline frame {frame} produced an empty fluid mesh"
            ))
            .into());
        }
        let expected_time = (authored_time * FIXED_HZ + 1e-8).floor() / FIXED_HZ;
        if (fluid.simulation_time - expected_time).abs() > 1e-4 {
            return Err(io::Error::other(format!(
                "offline frame {frame} simulation time {:.9} does not reach fixed-tick time {:.9}",
                fluid.simulation_time, expected_time
            ))
            .into());
        }
        let mut row = metric_row(frame, authored_time, timings, fluid, 0.0)?;
        let should_capture = options
            .stills_every
            .is_none_or(|every| frame % every == 0 || frame == options.frames || frame == 1);
        if should_capture {
            let capture_started = Instant::now();
            let rgba = readback_rgba(
                &device,
                &offline_target,
                options.width,
                options.height,
                options.linear,
            );
            let expected_rgba_len = (options.width as usize)
                .checked_mul(options.height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .expect("offline RGBA size fits usize");
            if rgba.len() != expected_rgba_len {
                return Err(io::Error::other(format!(
                    "offline frame {frame} readback has {} bytes, expected {expected_rgba_len}",
                    rgba.len()
                ))
                .into());
            }
            if let Some(writer) = offline_rgba_writer.as_mut() {
                writer.write_all(&rgba)?;
            } else {
                let png = encode_rgba8_png(&rgba, options.width, options.height);
                let path = options
                    .output_dir
                    .join("stills")
                    .join(format!("frame_{frame:06}.png"));
                fs::write(path, png)?;
            }
            row.capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
            if !row.capture_ms.is_finite() || row.capture_ms < 0.0 {
                return Err(io::Error::other(format!(
                    "offline frame {frame} capture timing is invalid: {}",
                    row.capture_ms
                ))
                .into());
            }
            capture_ms += row.capture_ms;
        }
        offline_rows.push(row);
    }
    ensure_wall_limit(overall_started, "offline pass", options.max_seconds)?;
    if let Some(mut writer) = offline_rgba_writer {
        writer.flush()?;
    }
    let offline_end_simulation_time = offline_rows
        .last()
        .expect("offline rows contain all stepped frames")
        .fluid
        .simulation_time;
    drop(offline_target);
    drop(offline_runtime);
    drop(offline_scope);

    let preview_metadata = if options.offline_only {
        None
    } else {
        let preview_target = RenderTarget::new(
            &device,
            options.width,
            options.height,
            GpuTextureFormat::Rgba16Float,
            "fluid-capture-preview",
        );
        let mut preview_runtime = build_runtime(&json, &device, options.width, options.height)?;
        let preview_initial_timings;
        {
            let preview_warmup_scope = PhysicsStepScope::for_render(true);
            let (warmup_timings, warmup_fluid) = render_frame(
                &mut preview_runtime,
                &preview_target,
                &device,
                0,
                0.0,
                frame_dt,
                options,
            )?;
            warmup_assets(
                &mut preview_runtime,
                &preview_target,
                &device,
                options,
                overall_started,
            )?;
            if warmup_fluid.simulation_time.abs() > 1e-4 {
                return Err(io::Error::other(format!(
                    "preview warmup simulation time is not zero: {}",
                    warmup_fluid.simulation_time
                ))
                .into());
            }
            preview_initial_timings = InitialFrameTimings {
                render_cpu_ms: warmup_timings.render_cpu_ms,
                submit_wait_ms: warmup_timings.submit_wait_ms,
                gpu_ms: warmup_timings.gpu_ms,
                frame_ms: warmup_timings.frame_ms,
            };
            drop(preview_warmup_scope);
        }

        let preview_scope = PhysicsStepScope::for_render(false);
        let preview_started = Instant::now();
        let mut preview_rows = Vec::with_capacity(options.frames as usize);
        let mut next_deadline = 0.0f64;
        let mut previous_authored_time = 0.0f64;
        let mut previous_frame_begin: Option<Instant> = None;
        let mut last_simulation_time = 0.0f64;
        let mut accepted_fluid_frames = 0u32;
        let mut total_sim_ticks_advanced = 0u64;

        while preview_started.elapsed().as_secs_f64() < PREVIEW_SECONDS
            && preview_rows.len() < options.frames as usize
        {
            let now = preview_started.elapsed().as_secs_f64();
            if now < next_deadline {
                thread::sleep(Duration::from_secs_f64(next_deadline - now));
            }
            let frame_begin = Instant::now();
            let authored_time = preview_started.elapsed().as_secs_f64();
            let dt = authored_time - previous_authored_time;
            let presentation_interval_ms = previous_frame_begin
                .map(|previous| frame_begin.duration_since(previous).as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            let frame = (preview_rows.len() + 1) as u32;
            let (timings, fluid) = render_frame(
                &mut preview_runtime,
                &preview_target,
                &device,
                frame,
                authored_time,
                dt,
                options,
            )?;
            preview_rows.push(metric_row(
                frame,
                authored_time,
                timings,
                fluid,
                presentation_interval_ms,
            )?);
            if fluid.simulation_time > last_simulation_time + 1e-9 {
                accepted_fluid_frames += 1;
                let advanced = ((fluid.simulation_time - last_simulation_time) * FIXED_HZ).round();
                if advanced.is_finite() && advanced > 0.0 {
                    total_sim_ticks_advanced =
                        total_sim_ticks_advanced.saturating_add(advanced as u64);
                }
                last_simulation_time = fluid.simulation_time;
            }
            previous_authored_time = authored_time;
            previous_frame_begin = Some(frame_begin);
            let elapsed = preview_started.elapsed().as_secs_f64();
            next_deadline += frame_dt;
            if next_deadline < elapsed {
                next_deadline = (elapsed / frame_dt).floor() * frame_dt + frame_dt;
            }
            let preview_max_seconds = PREVIEW_MAX_SECONDS.min(options.max_seconds);
            if elapsed > preview_max_seconds {
                return Err(io::Error::other(format!(
                    "preview pass exceeded wall limit of {preview_max_seconds:.0}s ({elapsed:.3}s)"
                ))
                .into());
            }
        }
        let preview_elapsed_s = preview_started.elapsed().as_secs_f64();
        let preview_max_seconds = PREVIEW_MAX_SECONDS.min(options.max_seconds);
        if preview_elapsed_s > preview_max_seconds {
            return Err(io::Error::other(format!(
            "preview pass exceeded wall limit of {preview_max_seconds:.0}s ({preview_elapsed_s:.3}s)"
        ))
        .into());
        }
        let preview_final = preview_rows.last().ok_or_else(|| {
            io::Error::other("preview pass delivered no frames before its deadline")
        })?;
        let preview_metadata = PreviewMetadata {
            elapsed_s: preview_elapsed_s,
            actual_frames: preview_rows.len() as u32,
            fps: options.fps,
            initial_frame: preview_initial_timings,
            accepted_fluid_frames,
            total_sim_ticks_advanced,
            start_simulation_time: 0.0,
            end_simulation_time: preview_final.fluid.simulation_time,
            final_authored_time: preview_final.authored_time,
            final_lag_seconds: preview_final.fluid.lag_seconds,
        };
        drop(preview_scope);
        drop(preview_target);
        drop(preview_runtime);
        ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
        write_csv(&options.output_dir.join("preview.csv"), &preview_rows)?;
        Some(preview_metadata)
    };

    write_csv(&options.output_dir.join("offline.csv"), &offline_rows)?;
    if options.offline_only {
        write_csv(&options.output_dir.join("preview.csv"), &[])?;
    }
    let metadata = Metadata {
        preset_path: options.preset_path.display().to_string(),
        gpu_device_name: device.device_name(),
        width: options.width,
        height: options.height,
        grid: [preset.resolution; 3],
        cell_size: preset.domain_size / f64::from(preset.resolution),
        surface_detail: preset.surface_detail,
        native_fixed_hz: FIXED_HZ,
        capture_fps: options.fps,
        display_transform: if options.linear {
            "linear_srgb"
        } else {
            "reinhard"
        },
        offline_only: options.offline_only,
        stills_every: options.stills_every,
        offline: PassMetadata {
            frames: options.frames,
            fps: options.fps,
            initial_frame: InitialFrameTimings {
                render_cpu_ms: initial_timings.render_cpu_ms,
                submit_wait_ms: initial_timings.submit_wait_ms,
                gpu_ms: initial_timings.gpu_ms,
                frame_ms: initial_timings.frame_ms,
            },
            start_simulation_time: 0.0,
            end_simulation_time: offline_end_simulation_time,
            capture_ms,
        },
        preview: preview_metadata,
    };
    let mut metadata_writer =
        BufWriter::new(File::create(options.output_dir.join("metadata.json"))?);
    serde_json::to_writer_pretty(&mut metadata_writer, &metadata)?;
    writeln!(metadata_writer)?;
    metadata_writer.flush()?;
    ensure_wall_limit(overall_started, "capture", options.max_seconds)?;
    Ok(())
}

fn main() -> CaptureResult<()> {
    let options = parse_options()?;
    run(&options)
}
