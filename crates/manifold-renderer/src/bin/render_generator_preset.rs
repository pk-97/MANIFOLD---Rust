//! `render-generator-preset` — headless look-dev render of ONE generator
//! preset at an arbitrary resolution, warm-up length, and outer-card param
//! overrides. The factory-thumbnail bin (`generate_preset_thumbnails.rs`)
//! renders every preset square at 256px with defaults — right for the
//! browser, useless for judging a look. This bin is the iteration loop for
//! shader work inside a preset: edit JSON → render → Read the PNG.
//!
//! Run:
//!   cargo run -p manifold-renderer --bin render-generator-preset -- \
//!       BlackHole --size 1280x720 --frames 90 --out /tmp/bh.png \
//!       --param cam_dist=31.75 --param tilt=15
//!
//! Sequence mode (--sequence-dir DIR) renders exactly --frames frames, one
//! blocking PNG readback each, named frame_000000.png onward — artifact
//! generation for the Live Water capture (WATER_IMPLEMENTATION_PLAN S8), no
//! convergence polling. --schedule FILE.json applies param events during
//! sequence rendering (requires --sequence-dir); the complete schedule is
//! validated before frame 0 and any violation aborts with nothing written.
//!
//! Params are outer-card ids from the preset's `presetMetadata.params`;
//! anything not overridden renders at its declared default. Output is the
//! same Reinhard-tonemapped, straight-alpha-over-black PNG convention the
//! save-time thumbnail path uses (linear HDR graph output → viewable PNG).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::Seconds;
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{readback_raw_halves, readback_to_srgb_png};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const DT: f32 = 1.0 / 60.0;

struct Args {
    preset: String,
    preset_file: Option<PathBuf>,
    width: u32,
    height: u32,
    frames: u32,
    out: PathBuf,
    overrides: Vec<(String, f32)>,
    /// Fire a clip trigger every N frames (0 = never) — trigger_count
    /// advances as frame/N so trigger-responsive presets can be exercised
    /// headlessly.
    trigger_every: u32,
    /// BUG-117: hard cap on frames rendered while WAITING for convergence
    /// past `--frames`, so a preset that never settles (a genuinely
    /// per-frame-varying look, or a stuck async load) can't hang the tool
    /// forever — it prints a warning and writes whatever it has instead.
    max_frames: u32,
    /// Sequence mode: render exactly `--frames` frames into DIR, one PNG
    /// per frame, no convergence polling. The final `--out` write still
    /// happens after the sequence.
    sequence_dir: Option<PathBuf>,
    /// Parameter schedule for sequence mode (see module doc).
    schedule: Option<PathBuf>,
    /// Capture every Nth sequence frame while still stepping every frame.
    capture_stride: u32,
    /// Optional PNG contact sheet assembled from captured sequence frames.
    contact_sheet: Option<PathBuf>,
    timing_output: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let preset = argv.next().ok_or("usage: render-generator-preset <PresetId> [--size WxH] [--frames N] [--out PATH] [--param id=value ...]")?;
    let mut args = Args {
        preset,
        preset_file: None,
        width: 1280,
        height: 720,
        frames: 90,
        out: PathBuf::from("/tmp/preset-render.png"),
        overrides: Vec::new(),
        trigger_every: 0,
        max_frames: 300,
        sequence_dir: None,
        schedule: None,
        capture_stride: 1,
        contact_sheet: None,
        timing_output: None,
    };
    while let Some(flag) = argv.next() {
        let value = argv
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--size" => {
                let (w, h) = value
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants WxH, got {value}"))?;
                args.width = w.parse().map_err(|e| format!("bad width: {e}"))?;
                args.height = h.parse().map_err(|e| format!("bad height: {e}"))?;
            }
            "--preset-file" => args.preset_file = Some(PathBuf::from(value)),
            "--frames" => {
                args.frames = value.parse().map_err(|e| format!("bad frames: {e}"))?;
            }
            "--out" => args.out = PathBuf::from(value),
            "--triggers" => {
                args.trigger_every = value.parse().map_err(|e| format!("bad triggers: {e}"))?;
            }
            "--max-frames" => {
                args.max_frames = value.parse().map_err(|e| format!("bad max-frames: {e}"))?;
            }
            "--param" => {
                let (id, v) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--param wants id=value, got {value}"))?;
                let v: f32 = v.parse().map_err(|e| format!("bad value for {id}: {e}"))?;
                args.overrides.push((id.to_string(), v));
            }
            "--sequence-dir" => args.sequence_dir = Some(PathBuf::from(value)),
            "--schedule" => args.schedule = Some(PathBuf::from(value)),
            "--capture-stride" => {
                args.capture_stride = value.parse().map_err(|e| format!("bad capture-stride: {e}"))?;
                if args.capture_stride == 0 {
                    return Err("--capture-stride must be greater than zero".to_string());
                }
            }
            "--contact-sheet" => args.contact_sheet = Some(PathBuf::from(value)),
            "--timing-output" => args.timing_output = Some(PathBuf::from(value)),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

struct ScheduleEvent {
    frame: u32,
    params: Vec<(String, f32)>,
}

/// Full up-front validation of a schedule file. Returns every violation
/// found, not just the first, so a bad fixture lists all its mistakes in
/// one run. `allowed` is the preset's exposed outer-card param id set — the
/// same set `--param` validates against.
fn load_schedule(
    path: &Path,
    total_frames: u32,
    allowed: &HashSet<&str>,
) -> Result<Vec<ScheduleEvent>, Vec<String>> {
    let mut violations = Vec::new();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return Err(vec![format!("read {}: {e}", path.display())]),
    };
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("parse {}: {e}", path.display())]),
    };
    match root.get("version").and_then(|v| v.as_u64()) {
        Some(1) => {}
        other => violations.push(format!(
            "version must be 1, got {}",
            other.map_or("missing or non-integer".to_string(), |v| v.to_string())
        )),
    }
    let events = match root.get("events").and_then(|v| v.as_array()) {
        Some(e) => e,
        None => {
            violations.push("events must be an array".to_string());
            return Err(violations);
        }
    };
    let mut prev_frame: Option<u64> = None;
    for (i, ev) in events.iter().enumerate() {
        let frame_raw = ev.get("frame").and_then(|v| v.as_u64());
        let frame = match frame_raw {
            Some(f) if f < total_frames as u64 => Some(f),
            Some(f) => {
                violations.push(format!("events[{i}].frame {f} out of range (must be < --frames={total_frames})"));
                None
            }
            None => {
                violations.push(format!("events[{i}].frame missing or not a non-negative integer"));
                None
            }
        };
        if let (Some(f), Some(prev)) = (frame, prev_frame)
            && f <= prev
        {
            violations.push(format!(
                "events[{i}].frame {f} not strictly ascending (previous event at frame {prev})"
            ));
        }
        if let Some(f) = frame {
            prev_frame = Some(f);
        }
        match ev.get("params").and_then(|v| v.as_object()) {
            Some(params) => {
                for (key, value) in params {
                    if !allowed.contains(key.as_str()) {
                        violations.push(format!("events[{i}]: unknown param '{key}'"));
                    }
                    match value.as_f64() {
                        Some(v) if v.is_finite() && (v as f32).is_finite() => {}
                        _ => violations.push(format!(
                            "events[{i}].params.{key} must be a finite number, got {value}"
                        )),
                    }
                }
            }
            None => violations.push(format!("events[{i}].params must be an object")),
        }
    }
    if !violations.is_empty() {
        return Err(violations);
    }
    Ok(events
        .iter()
        .map(|ev| ScheduleEvent {
            frame: ev.get("frame").and_then(|v| v.as_u64()).unwrap() as u32,
            params: ev
                .get("params")
                .and_then(|v| v.as_object())
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_f64().unwrap() as f32))
                .collect(),
        })
        .collect())
}

/// The S3 clock contract: one frame through the production PresetRuntime
/// render path with an explicit advancing SimulationFrame (fixed 60 Hz,
/// epoch 0) — shared verbatim by the single-shot convergence loop and
/// sequence mode, so both exercise the same code the live path runs.
fn render_frame(
    device: &std::sync::Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    manifest: &ParamManifest,
    frame: u32,
    warmup_frames: u32,
    width: u32,
    height: u32,
    trigger_every: u32,
) {
    let time = frame as f64 * DT as f64;
    let ctx = PresetContext {
        time,
        beat: time * 2.0, // 120 bpm
        dt: DT,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: (frame as f32 / warmup_frames as f32).min(1.0),
        trigger_count: if trigger_every > 0 {
            frame / trigger_every
        } else {
            0
        },
    };
    let mut enc = device.create_encoder("look-dev-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        // WATER_SIMULATION_DESIGN section 6 warmup parity: this headless
        // context has no host transport, so it installs an explicit
        // advancing frame (fixed 60 Hz, epoch 0) — a graph with substep
        // regions never runs without a SimulationFrame.
        runtime.set_simulation_frame(
            manifold_renderer::node_graph::substeps::SimulationFrame {
                frame_id: frame as u64 + 1,
                delta: Seconds(1.0 / 60.0),
                epoch: 0,
                advancing: true,
                exporting: false,
            },
        );
        runtime.render(&mut gpu, &target.texture, &ctx, manifest);
    }
    enc.commit_and_wait_completed();
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    if args.schedule.is_some() && args.sequence_dir.is_none() {
        eprintln!("error: --schedule requires --sequence-dir");
        std::process::exit(2);
    }
    if args.frames == 0 {
        eprintln!("error: --frames must be greater than zero");
        std::process::exit(2);
    }
    if (args.capture_stride != 1 || args.contact_sheet.is_some() || args.timing_output.is_some())
        && args.sequence_dir.is_none()
    {
        eprintln!("error: capture stride, contact sheet, and timing output require --sequence-dir");
        std::process::exit(2);
    }
    if args.timing_output.is_some() && args.frames < 90 {
        eprintln!("error: --timing-output requires at least 90 frames");
        std::process::exit(2);
    }

    let json_path = args.preset_file.clone().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/generator-presets")
            .join(format!("{}.json", args.preset))
    });
    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse preset JSON");

    // Manifest seeded from the preset's own outer-card specs so overrides
    // route through the same bindings the inspector card uses.
    let mut params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    for (id, v) in &args.overrides {
        match params.iter_mut().find(|p| p.id() == id) {
            Some(p) => p.value = *v,
            None => {
                eprintln!("error: preset has no outer param '{id}'");
                std::process::exit(2);
            }
        }
    }
    let mut manifest = ParamManifest::from_params(params.clone());

    // Validate the complete schedule before any GPU work so a bad fixture
    // exits non-zero with nothing written.
    let schedule = match &args.schedule {
        Some(path) => {
            let allowed: HashSet<&str> = params.iter().map(|p| p.id()).collect();
            match load_schedule(path, args.frames, &allowed) {
                Ok(events) => events,
                Err(violations) => {
                    for v in violations {
                        eprintln!("schedule error: {v}");
                    }
                    std::process::exit(2);
                }
            }
        }
        None => Vec::new(),
    };

    let device = std::sync::Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&device),
        args.width,
        args.height,
        format,
        None,
    )
    .expect("generator build failed");

    let target = RenderTarget::new(
        &device,
        args.width,
        args.height,
        format,
        "look-dev-target",
    );

    let warmup_frames = args.frames.max(1);

    if let Some(dir) = &args.sequence_dir {
        // Sequence mode: exactly --frames frames, each rendered once and
        // read back to its own PNG — no convergence polling. Blocking
        // readback here is artifact generation, not a timing measurement.
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        let mut captured: Vec<(u32, image::RgbaImage)> = Vec::new();
        let mut mapping: Vec<serde_json::Value> = Vec::new();
        let mut timing_samples: Vec<f64> = Vec::with_capacity(args.frames.saturating_sub(60) as usize);
        let mut next_event = 0usize;
        for frame in 0..args.frames {
            if next_event < schedule.len() && schedule[next_event].frame == frame {
                let event = &schedule[next_event];
                for (id, v) in &event.params {
                    let p = params
                        .iter_mut()
                        .find(|p| p.id() == id)
                        .expect("schedule keys validated before rendering");
                    p.value = *v;
                }
                manifest = ParamManifest::from_params(params.clone());
                println!(
                    "schedule: frame {} applied {} param(s)",
                    event.frame,
                    event.params.len()
                );
                next_event += 1;
            }
            let render_start = Instant::now();
            render_frame(
                &device,
                &mut runtime,
                &target,
                &manifest,
                frame,
                warmup_frames,
                args.width,
                args.height,
                args.trigger_every,
            );
            if args.timing_output.is_some() && frame >= 60 {
                timing_samples.push(render_start.elapsed().as_secs_f64() * 1000.0);
            }
            if let Some(err) = runtime.runtime_fatal_error() {
                panic!("render-generator-preset frame {frame}: {err}");
            }
            if frame % args.capture_stride == 0 || frame + 1 == args.frames {
                let png = readback_to_srgb_png(&device, &target.texture, args.width, args.height);
                let path = dir.join(format!("frame_{frame:06}.png"));
                std::fs::write(&path, &png)
                    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
                if args.contact_sheet.is_some() {
                    let image = image::load_from_memory(&png)
                        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
                        .resize(320, 180, image::imageops::FilterType::Triangle)
                        .to_rgba8();
                    captured.push((frame, image));
                    mapping.push(serde_json::json!({
                        "frame": frame,
                        "timeSeconds": frame as f64 * DT as f64,
                        "path": path.display().to_string(),
                    }));
                }
            }
        }
        if let Some(path) = &args.contact_sheet {
            let columns = (captured.len().max(1) as f32).sqrt().ceil() as u32;
            let rows = (captured.len() as u32).div_ceil(columns);
            let tile_width = 320;
            let tile_height = 180;
            let sheet_width = columns * tile_width;
            let sheet_height = rows * tile_height;
            let mut sheet = image::RgbaImage::new(sheet_width, sheet_height);
            for (index, (frame, image)) in captured.iter().enumerate() {
                let x = (index as u32 % columns) * tile_width;
                let y = (index as u32 / columns) * tile_height;
                image::imageops::overlay(&mut sheet, image, i64::from(x), i64::from(y));
                println!("contact sheet tile frame={frame} x={x} y={y}");
            }
            sheet.save(path).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            println!("contact sheet: {} ({} frames, {}x{})", path.display(), captured.len(), sheet_width, sheet_height);
            let sidecar = path.with_extension("json");
            let sidecar_text = serde_json::to_string_pretty(&mapping)
                .expect("serialize contact-sheet mapping");
            std::fs::write(&sidecar, sidecar_text)
                .unwrap_or_else(|e| panic!("write {}: {e}", sidecar.display()));
            println!("contact sheet mapping: {}", sidecar.display());
        }
        if let Some(path) = &args.timing_output {
            timing_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let percentile = |pct: usize| timing_samples[((timing_samples.len() * pct) / 100).min(timing_samples.len() - 1)];
            let mean = timing_samples.iter().sum::<f64>() / timing_samples.len() as f64;
            let timing = serde_json::json!({
                "measurement": "wall frame cost: encode + submit + GPU wait; excludes PNG readback, encoding, and file writes",
                "nativeAppFpsClaim": false,
                "width": args.width,
                "height": args.height,
                "totalFrames": args.frames,
                "sampleFrames": format!("60..{}", args.frames - 1),
                "samples": timing_samples,
                "medianMs": percentile(50),
                "p95Ms": percentile(95),
                "meanMs": mean,
            });
            std::fs::write(path, serde_json::to_string_pretty(&timing).unwrap())
                .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            println!("timing: {} (wall frame cost, frames 60..{})", path.display(), args.frames - 1);
        }
    } else {
        // BUG-117: async-loading primitives (large glTF, image_folder, DNN
        // plugins) leave the pre-bound output untouched until their background
        // job lands, so a fixed `--frames` count can write a PNG mid-load with
        // no warning — the same class BUG-100 hit for the azalea-import test
        // harness. Same fix, ported here: after the requested warm-up, keep
        // rendering and comparing consecutive RAW readbacks until `STABLE_STREAK`
        // of them are byte-identical, capped at `--max-frames` so a genuinely
        // per-frame-varying preset (or a stuck load) can't hang the tool forever.
        const STABLE_STREAK: u32 = 3;
        let max_frames = args.max_frames.max(warmup_frames);
        let mut prev_raw: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        let mut converged = false;
        for frame in 0..max_frames {
            render_frame(
                &device,
                &mut runtime,
                &target,
                &manifest,
                frame,
                warmup_frames,
                args.width,
                args.height,
                args.trigger_every,
            );

            // Convergence tracking only kicks in once the requested warm-up has
            // run — the caller asked for at least that many frames regardless
            // (e.g. to reach a specific animation beat), and a preset can
            // legitimately still be settling its warm-up transient this early.
            if frame + 1 >= warmup_frames {
                let raw = readback_raw_halves(&device, &target.texture, args.width, args.height);
                if prev_raw.as_deref() == Some(raw.as_slice()) {
                    stable_count += 1;
                } else {
                    stable_count = 0;
                }
                prev_raw = Some(raw);
                if stable_count >= STABLE_STREAK {
                    converged = true;
                    println!(
                        "render-generator-preset: converged on frame {frame} \
                         (stable for {STABLE_STREAK} frames)"
                    );
                    break;
                }
            }
        }
        if !converged {
            eprintln!(
                "render-generator-preset: WARNING — hit --max-frames={max_frames} before {STABLE_STREAK} \
                 consecutive identical frames; the preset may still be loading async content \
                 (glTF, image_folder, a DNN plugin) and this PNG could be an incomplete render. \
                 Re-run with a higher --max-frames if this preset is expected to keep animating."
            );
        }
    }

    // D2: the ONE shared tonemap/encode (`headless_readback::readback_to_srgb_png`)
    // — never a local Reinhard implementation here. In sequence mode this is
    // the final frame's image, same as the single-shot path.
    let png = readback_to_srgb_png(&device, &target.texture, args.width, args.height);
    std::fs::write(&args.out, &png).unwrap_or_else(|e| panic!("write {}: {e}", args.out.display()));
    println!("OK {} ({}x{}, {} frames)", args.out.display(), args.width, args.height, args.frames);
}
