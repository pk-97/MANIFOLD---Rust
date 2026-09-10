//! `water-stage-profile` — per-stage GPU profile of the WaterPrototype
//! generator preset on the production `PresetRuntime` render path.
//!
//! The release perf-soak shows WaterPrototype at ~42ms wall/frame (still
//! water, 1080p, M4 Max) vs ~1.4ms for the no-water baseline. Headless
//! frames.jsonl has no GPU timestamps, so this bin measures the per-stage
//! split directly: every frame runs through `PresetRuntime::render` with
//! per-dispatch GPU timestamp profiling enabled on the frame's encoder, and
//! spans are aggregated by kernel label. Region-body stages repeat once per
//! simulation tick (16x/frame at the water clock), which the
//! dispatches/frame column makes visible.
//!
//! Run:
//!   cargo run --release -p manifold-renderer --bin water-stage-profile
//!   cargo run --release -p manifold-renderer --bin water-stage-profile -- \
//!       --param pourRate=15000 --param cubeStrike=1
//!
//! Notes on the measurement ( profiling.rs module doc):
//! Apple counter sampling happens at stage boundaries only, so profiled
//! mode splits one encoder per dispatch — per-dispatch times lose the
//! cross-dispatch overlap production gets, and the SUM of spans is an UPPER
//! BOUND on production GPU time. The per-frame command-buffer total carries
//! the same caveat. Shares and per-stage ranking are the signal.
//!
//! Step tags (`scope:sN`) are NOT stamped on these spans: the executor only
//! tags when its attribution profiling is on, and `PresetRuntime` exposes
//! no accessor for it. That is deliberate — `Executor::set_profiling(true)`
//! force-dirties every step, changing which dispatches production would
//! skip. Aggregation by kernel label on the untouched production dispatch
//! stream is the faithful measurement.

use std::collections::BTreeMap;

use manifold_core::params::{Param, ParamManifest};
use manifold_core::Seconds;
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{PrimitiveRegistry, substeps::SimulationFrame};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const DT: f32 = 1.0 / 60.0;
const FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const GENERATOR_PRESETS_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets");

struct Args {
    width: u32,
    height: u32,
    frames: u32,
    warmup: u32,
    overrides: Vec<(String, f32)>,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let mut args = Args {
        width: 1920,
        height: 1080,
        frames: 120,
        warmup: 8,
        overrides: Vec::new(),
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
            "--frames" => {
                args.frames = value.parse().map_err(|e| format!("bad frames: {e}"))?;
            }
            "--warmup" => {
                args.warmup = value.parse().map_err(|e| format!("bad warmup: {e}"))?;
            }
            "--param" => {
                let (id, v) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--param wants id=value, got {value}"))?;
                let v: f32 = v.parse().map_err(|e| format!("bad value for {id}: {e}"))?;
                args.overrides.push((id.to_string(), v));
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

/// The S3 clock contract, verbatim from render_generator_preset: one frame
/// through the production path with an explicit advancing SimulationFrame
/// (fixed 60 Hz, epoch 0) installed before render.
fn render_frame(
    device: &std::sync::Arc<GpuDevice>,
    runtime: &mut PresetRuntime,
    target: &RenderTarget,
    manifest: &ParamManifest,
    frame: u32,
    width: u32,
    height: u32,
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
        anim_progress: 1.0,
        trigger_count: 0,
    };
    let mut enc = device.create_encoder("water-stage-profile");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        runtime.set_simulation_frame(SimulationFrame {
            frame_id: frame as u64 + 1,
            delta: Seconds(1.0 / 60.0),
            epoch: 0,
            advancing: true,
            exporting: false,
        });
        runtime.render(&mut gpu, &target.texture, &ctx, manifest);
    }
    enc.commit_and_wait_completed();
}

fn percentile(sorted: &[f64], pct: usize) -> f64 {
    sorted[((sorted.len() * pct) / 100).min(sorted.len() - 1)]
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let json_path = format!("{GENERATOR_PRESETS_DIR}/WaterPrototype.json");
    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("read {json_path}: {e}"));
    let def: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&json).expect("parse WaterPrototype JSON");

    // Outer-card manifest seeded from the preset's own specs, overrides
    // applied the same way render_generator_preset does.
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
    let manifest = ParamManifest::from_params(params.clone());

    let device = std::sync::Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&device),
        args.width,
        args.height,
        FORMAT,
        None,
    )
    .expect("WaterPrototype build failed");

    let target = RenderTarget::new(
        &device,
        args.width,
        args.height,
        FORMAT,
        "water-stage-profile-target",
    );

    let Some(sampler) = device.create_timestamp_sampler(8192) else {
        eprintln!("error: device does not support GPU counter sampling");
        std::process::exit(1);
    };

    for frame in 0..args.warmup {
        render_frame(
            &device,
            &mut runtime,
            &target,
            &manifest,
            frame,
            args.width,
            args.height,
        );
    }

    let mut per_label: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut frame_totals: Vec<f64> = Vec::with_capacity(args.frames as usize);
    let mut frame_attributed: Vec<f64> = Vec::with_capacity(args.frames as usize);
    let mut overflow = 0usize;
    let mut invalid = 0usize;
    let mut failed_buffers = 0usize;

    for i in 0..args.frames {
        let frame = args.warmup + i;
        let time = frame as f64 * DT as f64;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: DT,
            width: args.width,
            height: args.height,
            output_width: args.width,
            output_height: args.height,
            aspect: args.width as f32 / args.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 1.0,
            trigger_count: 0,
        };
        let mut enc = device.create_encoder("water-stage-profiled");
        enc.enable_dispatch_profiling(sampler.clone(), &device);
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.set_simulation_frame(SimulationFrame {
                frame_id: frame as u64 + 1,
                delta: Seconds(1.0 / 60.0),
                epoch: 0,
                advancing: true,
                exporting: false,
            });
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        let profile = enc.commit_and_wait_profiled(&device);
        overflow += profile.overflow;
        invalid += profile.invalid;
        failed_buffers += profile.failed_command_buffers;
        frame_attributed.push(profile.attributed_ms());
        frame_totals.push(profile.total_ms);
        for span in &profile.spans {
            per_label
                .entry(span.label.clone())
                .or_default()
                .push(span.millis);
        }
    }

    let frames = f64::from(args.frames);
    let override_desc = if args.overrides.is_empty() {
        "none (defaults — still water)".to_string()
    } else {
        args.overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };

    println!(
        "water-stage-profile — WaterPrototype @ {}x{}, {} warmup + {} measured frames",
        args.width, args.height, args.warmup, args.frames
    );
    println!("params: {override_desc}");
    println!(
        "NOTE: profiled mode splits one encoder per dispatch (Apple counter sampling is \
         stage-boundary only), so cross-dispatch overlap is lost and span/total sums are an \
         UPPER BOUND on production GPU time. Ranking and shares are the signal."
    );
    if overflow > 0 || invalid > 0 || failed_buffers > 0 {
        println!(
            "WARNING: overflow={overflow} invalid={invalid} failed_buffers={failed_buffers}"
        );
    }
    println!();

    let mut rows: Vec<(String, Vec<f64>)> = per_label.into_iter().collect();
    for (_, samples) in &mut rows {
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }
    rows.sort_by(|a, b| {
        let pa = percentile(&a.1, 95);
        let pb = percentile(&b.1, 95);
        pb.partial_cmp(&pa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ma = a.1.iter().sum::<f64>() / a.1.len() as f64;
                let mb = b.1.iter().sum::<f64>() / b.1.len() as f64;
                mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    println!(
        "{:<44} {:>9} {:>10} {:>10} {:>11}",
        "kernel label", "disp/frame", "mean ms", "p95 ms", "total ms/f"
    );
    println!("{}", "-".repeat(90));
    for (label, samples) in &rows {
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let p95 = percentile(samples, 95);
        let per_frame = samples.len() as f64 / frames;
        let total_per_frame = mean * per_frame;
        println!(
            "{:<44} {:>9.1} {:>10.4} {:>10.4} {:>11.4}",
            label, per_frame, mean, p95, total_per_frame
        );
    }

    frame_totals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    frame_attributed.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean_total = frame_totals.iter().sum::<f64>() / frames;
    let p95_total = percentile(&frame_totals, 95);
    let max_total = *frame_totals.last().unwrap_or(&0.0);
    let mean_attributed = frame_attributed.iter().sum::<f64>() / frames;
    println!();
    println!(
        "per-frame command-buffer GPU time: mean {mean_total:.3} ms | p50 {:.3} ms | \
         p95 {p95_total:.3} ms | max {max_total:.3} ms",
        percentile(&frame_totals, 50),
    );
    println!(
        "span-sum per frame (upper bound): mean {mean_attributed:.3} ms | \
         unattributed gap: mean {:.3} ms",
        mean_total - mean_attributed,
    );
}
