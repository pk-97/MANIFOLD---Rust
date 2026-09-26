//! Capture the shipped Water Basin CPU FLIP scene for offline/preview timing.
//!
//! The frame CSVs keep render CPU, command-buffer wait, whole-buffer GPU, and
//! native solver metrics separate. Offline is a complete-step replay at 60 Hz;
//! preview records the latest completed native snapshot, so repeated scalar
//! values are expected when the worker is behind. Each stepped offline frame
//! contributes one raw RGBA frame to `offline.rgba`; readback and file writes
//! happen after that frame's timer and are reported as `capture_ms`.
//!
//! Build with:
//!
//! `cargo build --profile test --features gpu-proofs --example fluid_capture`
//!
//! Run the resulting example with exactly one output directory argument.

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
use manifold_renderer::headless_readback::readback_tonemapped_rgba8;
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde::Serialize;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const RESOLUTION: u32 = 24;
const DOMAIN_SIZE: f64 = 4.0;
const CELL_SIZE: f64 = DOMAIN_SIZE / RESOLUTION as f64;
const SURFACE_DETAIL: u32 = 0;
const FIXED_HZ: f64 = 60.0;
const DT: f64 = 1.0 / FIXED_HZ;
const OFFLINE_FRAMES: u32 = 480;
const PREVIEW_SECONDS: f64 = 8.0;
const PREVIEW_MAX_SECONDS: f64 = 10.0;
const OVERALL_MAX_SECONDS: f64 = 180.0;
const CSV_HEADER: &str = "frame,authored_time,simulation_time,lag_seconds,render_cpu_ms,submit_wait_ms,gpu_ms,frame_ms,simulation_ms,meshing_ms,particle_count,vertex_count,capture_ms,presentation_interval_ms";
const METRIC_NAMES: [&str; 6] = [
    "simulation_time",
    "lag_seconds",
    "simulation_ms",
    "meshing_ms",
    "particle_count",
    "vertex_count",
];

type CaptureResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug)]
struct FluidMetrics {
    simulation_time: f64,
    lag_seconds: f64,
    simulation_ms: f64,
    meshing_ms: f64,
    particle_count: f64,
    vertex_count: f64,
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
    initial_frame: InitialFrameTimings,
    start_simulation_time: f64,
    end_simulation_time: f64,
    capture_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct PreviewMetadata {
    elapsed_s: f64,
    actual_frames: u32,
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
    gpu_device_name: String,
    width: u32,
    height: u32,
    grid: [u32; 3],
    cell_size: f64,
    surface_detail: u32,
    native_fixed_hz: f64,
    offline: PassMetadata,
    preview: PreviewMetadata,
}

fn water_basin_json() -> CaptureResult<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("generator-presets")
        .join("WaterBasin.json");
    fs::read_to_string(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read WaterBasin preset {}: {error}", path.display()),
        )
        .into()
    })
}

fn build_runtime(json: &str, device: &Arc<GpuDevice>) -> CaptureResult<PresetRuntime> {
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
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .map_err(|error| io::Error::other(format!("build Water Basin runtime: {error}")))?;
    let fluid_node = NodeId::from("fluid_surface");
    runtime.set_preview_node(Some(&fluid_node));
    Ok(runtime)
}

fn context(frame: u32, authored_time: f64, dt: f64) -> PresetContext {
    PresetContext {
        time: authored_time,
        beat: authored_time * 2.0,
        dt: dt as f32,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
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
    })
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    device: &GpuDevice,
    frame: u32,
    authored_time: f64,
    dt: f64,
) -> CaptureResult<(FrameTimings, FluidMetrics)> {
    let frame_started = Instant::now();
    let mut encoder = device.create_encoder("fluid-capture");
    let render_started = Instant::now();
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context(frame, authored_time, dt),
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
            "{},{:.9},{:.9},{:.9},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.3},{:.3},{:.6},{:.6}",
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
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn ensure_overall_limit(started: Instant, label: &str) -> CaptureResult<()> {
    let elapsed = started.elapsed().as_secs_f64();
    if elapsed > OVERALL_MAX_SECONDS {
        return Err(io::Error::other(format!(
            "{label} exceeded overall wall limit of {OVERALL_MAX_SECONDS:.0}s ({elapsed:.3}s)"
        ))
        .into());
    }
    Ok(())
}

fn run(output_dir: &Path) -> CaptureResult<()> {
    fs::create_dir_all(output_dir)?;
    let overall_started = Instant::now();
    let json = water_basin_json()?;
    let device = Arc::new(GpuDevice::new());
    let offline_target = RenderTarget::new(
        &device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "fluid-capture-offline",
    );
    let offline_scope = PhysicsStepScope::for_render(true);
    let mut offline_runtime = build_runtime(&json, &device)?;
    let (initial_timings, initial_fluid) =
        render_frame(&mut offline_runtime, &offline_target, &device, 0, 0.0, DT)?;
    if initial_fluid.simulation_time.abs() > 1e-4 {
        return Err(io::Error::other(format!(
            "offline initial simulation time is not zero: {}",
            initial_fluid.simulation_time
        ))
        .into());
    }

    let mut offline_rows = Vec::with_capacity(OFFLINE_FRAMES as usize);
    let mut offline_rgba_writer = BufWriter::new(File::create(output_dir.join("offline.rgba"))?);
    let mut capture_ms = 0.0;
    for frame in 1..=OFFLINE_FRAMES {
        ensure_overall_limit(overall_started, "offline pass")?;
        let authored_time = f64::from(frame) * DT;
        let (timings, fluid) = render_frame(
            &mut offline_runtime,
            &offline_target,
            &device,
            frame,
            authored_time,
            DT,
        )?;
        if fluid.vertex_count < 3.0 {
            return Err(io::Error::other(format!(
                "offline frame {frame} produced an empty fluid mesh"
            ))
            .into());
        }
        if (fluid.simulation_time - authored_time).abs() > 1e-4 {
            return Err(io::Error::other(format!(
                "offline frame {frame} simulation time {:.9} does not reach authored time {:.9}",
                fluid.simulation_time, authored_time
            ))
            .into());
        }
        let mut row = metric_row(frame, authored_time, timings, fluid, 0.0)?;
        let capture_started = Instant::now();
        let rgba = readback_tonemapped_rgba8(&device, &offline_target.texture, WIDTH, HEIGHT);
        let expected_rgba_len = (WIDTH as usize)
            .checked_mul(HEIGHT as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .expect("offline RGBA size fits usize");
        if rgba.len() != expected_rgba_len {
            return Err(io::Error::other(format!(
                "offline frame {frame} readback has {} bytes, expected {expected_rgba_len}",
                rgba.len()
            ))
            .into());
        }
        offline_rgba_writer.write_all(&rgba)?;
        row.capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
        if !row.capture_ms.is_finite() || row.capture_ms < 0.0 {
            return Err(io::Error::other(format!(
                "offline frame {frame} capture timing is invalid: {}",
                row.capture_ms
            ))
            .into());
        }
        capture_ms += row.capture_ms;
        offline_rows.push(row);
    }
    ensure_overall_limit(overall_started, "offline pass")?;
    offline_rgba_writer.flush()?;
    let offline_end_simulation_time = offline_rows
        .last()
        .expect("offline rows contain all stepped frames")
        .fluid
        .simulation_time;
    drop(offline_target);
    drop(offline_runtime);
    drop(offline_scope);

    let preview_target = RenderTarget::new(
        &device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "fluid-capture-preview",
    );
    let mut preview_runtime = build_runtime(&json, &device)?;
    let preview_initial_timings;
    {
        let preview_warmup_scope = PhysicsStepScope::for_render(true);
        let (warmup_timings, warmup_fluid) =
            render_frame(&mut preview_runtime, &preview_target, &device, 0, 0.0, DT)?;
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
    let mut preview_rows = Vec::with_capacity(OFFLINE_FRAMES as usize);
    let mut next_deadline = 0.0f64;
    let mut previous_authored_time = 0.0f64;
    let mut previous_frame_begin: Option<Instant> = None;
    let mut last_simulation_time = 0.0f64;
    let mut accepted_fluid_frames = 0u32;
    let mut total_sim_ticks_advanced = 0u64;

    while preview_started.elapsed().as_secs_f64() < PREVIEW_SECONDS
        && preview_rows.len() < OFFLINE_FRAMES as usize
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
                total_sim_ticks_advanced = total_sim_ticks_advanced.saturating_add(advanced as u64);
            }
            last_simulation_time = fluid.simulation_time;
        }
        previous_authored_time = authored_time;
        previous_frame_begin = Some(frame_begin);
        let elapsed = preview_started.elapsed().as_secs_f64();
        next_deadline += DT;
        if next_deadline < elapsed {
            next_deadline = (elapsed / DT).floor() * DT + DT;
        }
        if elapsed > PREVIEW_MAX_SECONDS {
            return Err(io::Error::other(format!(
                "preview pass exceeded wall limit of {PREVIEW_MAX_SECONDS:.0}s ({elapsed:.3}s)"
            ))
            .into());
        }
    }
    let preview_elapsed_s = preview_started.elapsed().as_secs_f64();
    if preview_elapsed_s > PREVIEW_MAX_SECONDS {
        return Err(io::Error::other(format!(
            "preview pass exceeded wall limit of {PREVIEW_MAX_SECONDS:.0}s ({preview_elapsed_s:.3}s)"
        ))
        .into());
    }
    let preview_final = preview_rows
        .last()
        .ok_or_else(|| io::Error::other("preview pass delivered no frames before its deadline"))?;
    let preview_metadata = PreviewMetadata {
        elapsed_s: preview_elapsed_s,
        actual_frames: preview_rows.len() as u32,
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
    ensure_overall_limit(overall_started, "capture")?;

    write_csv(&output_dir.join("offline.csv"), &offline_rows)?;
    write_csv(&output_dir.join("preview.csv"), &preview_rows)?;
    let metadata = Metadata {
        gpu_device_name: device.device_name(),
        width: WIDTH,
        height: HEIGHT,
        grid: [RESOLUTION; 3],
        cell_size: CELL_SIZE,
        surface_detail: SURFACE_DETAIL,
        native_fixed_hz: FIXED_HZ,
        offline: PassMetadata {
            frames: OFFLINE_FRAMES,
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
    let mut metadata_writer = BufWriter::new(File::create(output_dir.join("metadata.json"))?);
    serde_json::to_writer_pretty(&mut metadata_writer, &metadata)?;
    writeln!(metadata_writer)?;
    metadata_writer.flush()?;
    ensure_overall_limit(overall_started, "capture")?;
    Ok(())
}

fn main() -> CaptureResult<()> {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_else(|| "fluid_capture".into());
    let Some(output_dir) = args.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("usage: {} OUTPUT_DIR", PathBuf::from(program).display()),
        )
        .into());
    };
    if args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fluid_capture takes exactly one output directory argument",
        )
        .into());
    }
    run(&PathBuf::from(output_dir))
}
