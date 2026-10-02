//! BUG-l2h3.24 (GPU FLIP speed after the physics ports) — the whole frame
//! Peter sees on "Water — Dam Break (GPU FLIP)" at 1920×1080, as the preset
//! ships (resolution 64, Steps 1, Auto iterations), one deterministic tick a
//! frame from tick 0. 300 measured frames after asset warm-up: every tenth
//! frame carries per-dispatch GPU timestamps (split per node type, plus per
//! dispatch label inside `node.gpu_flip_step` so the solver stays readable);
//! the other frames are plain and give the budget numbers, whole-frame GPU ms
//! and CPU encode (wall time around `runtime.render`). Timestamped frames
//! open one encoder per dispatch and turn encode replay off
//! (ENCODE_REPLAY_DESIGN.md D7), so their split is a ratio, never the budget.
//! Present is not timed here: that is the app with MANIFOLD_RENDER_TRACE=1.
//! Reported, never gated.

use std::collections::BTreeMap;
use std::time::Instant;

use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat, GpuTimestampSampler};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{PrimitiveRegistry, physics::PhysicsStepScope};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde_json::Value;

use crate::harness;

const PRESET: &str = include_str!("../../assets/generator-presets/WaterDamBreakGpuFlip.json");
const MEASURED_FRAMES: usize = 300;
const TIMESTAMP_EVERY: usize = 10;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const STEP: &str = "node.gpu_flip_step";
/// A timestamped frame joins the split only when its spans cover this share
/// of the whole-frame GPU time; below it the calibration is stale (see `render`).
const STAMPED_COVERAGE: f64 = 0.7;

fn manifest(json: &Value) -> ParamManifest {
    let specs: Vec<ParamSpecDef> =
        serde_json::from_value(json["presetMetadata"]["params"].clone()).expect("card params");
    ParamManifest::from_params(specs.into_iter().map(Param::bundled).collect())
}

fn context(frame: i64, tick: u32) -> PresetContext {
    let time = f64::from(tick) / 60.0;
    PresetContext {
        time,
        beat: time * 2.0,
        dt: 1.0 / 60.0,
        width: WIDTH,
        height: HEIGHT,
        output_width: WIDTH,
        output_height: HEIGHT,
        aspect: WIDTH as f32 / HEIGHT as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// One frame's numbers. Plain frames fill `gpu_ms` and `cpu_ms`; timestamped
/// frames also fill the per-type and per-step-label splits.
struct Frame {
    gpu_ms: f64,
    cpu_ms: f64,
    node_error: bool,
    split: Option<(BTreeMap<String, f64>, BTreeMap<String, f64>)>,
}

fn render(
    runtime: &mut PresetRuntime,
    device: &GpuDevice,
    target: &RenderTarget,
    ctx: &PresetContext,
    params: &ParamManifest,
    sampler: Option<&GpuTimestampSampler>,
) -> Frame {
    let mut encoder = device.create_encoder("gpu flip frame perf");
    if let Some(sampler) = sampler {
        encoder.enable_dispatch_profiling(sampler.clone(), device);
    }
    runtime.set_profiling(sampler.is_some());
    let encode_started = Instant::now();
    let status = {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(&mut gpu, &target.texture, ctx, params);
        gpu.frame_status()
    };
    let cpu_ms = encode_started.elapsed().as_secs_f64() * 1e3;
    // A node error (the speed-cap report, BUG-jyot) is still what live shows.
    assert!(status.presentable(), "frame {} is not presentable: {status:?}", ctx.frame_count);
    let profile = encoder.commit_and_wait_profiled(device);
    assert_eq!(profile.failed_command_buffers, 0, "frame {} failed on the GPU", ctx.frame_count);
    let gpu_ms = profile.total_ms;
    let node_error = status != FrameRenderStatus::Complete;
    if sampler.is_none() {
        return Frame { gpu_ms, cpu_ms, node_error, split: None };
    }
    assert_eq!(profile.overflow, 0, "every dispatch must be timed");
    let summed: f64 = profile.spans.iter().map(|span| span.millis).sum();
    println!(
        "    frame {}: {} spans, {} invalid, spans sum {summed:.2} ms of {gpu_ms:.2} ms total",
        ctx.frame_count,
        profile.spans.len(),
        profile.invalid,
    );
    // A zero-group dispatch (a dead solver round) never rewrites its sample
    // slot, so the slot keeps an older frame's stamp and `resolve` calibrates
    // the frame origin on it: every span collapses. Such a frame is dropped
    // from the split, loudly, until the sampler rejects stale stamps.
    if summed < gpu_ms * STAMPED_COVERAGE {
        println!("      dropped from the split: stale timestamp calibration");
        return Frame { gpu_ms, cpu_ms, node_error, split: Some((BTreeMap::new(), BTreeMap::new())) };
    }
    let steps: BTreeMap<String, String> =
        runtime.take_step_profiles().into_iter().map(|step| (step.tag, step.type_id)).collect();
    let mut per_type = BTreeMap::new();
    let mut per_step_label = BTreeMap::new();
    for span in &profile.spans {
        let Some(type_id) = steps.get(&span.tag) else {
            *per_type.entry("(untagged)".to_owned()).or_insert(0.0) += span.millis;
            continue;
        };
        *per_type.entry(type_id.clone()).or_insert(0.0) += span.millis;
        if type_id == STEP {
            *per_step_label.entry(span.label.clone()).or_insert(0.0) += span.millis;
        }
    }
    Frame { gpu_ms, cpu_ms, node_error, split: Some((per_type, per_step_label)) }
}

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn print_split(title: &str, columns: &BTreeMap<String, Vec<f64>>) {
    let mut rows: Vec<(&String, &Vec<f64>)> = columns.iter().collect();
    rows.sort_by(|a, b| percentile(b.1, 0.5).total_cmp(&percentile(a.1, 0.5)));
    println!("  {title} (timestamped frames, ratios only):");
    for (name, samples) in rows {
        println!("    {name:<40} p50 {:>8.3} ms  p95 {:>8.3} ms", percentile(samples, 0.5), percentile(samples, 0.95));
    }
}

#[test]
fn gpu_flip_frame_perf() {
    let harness = harness::shared();
    let device = &harness.device;
    let sampler = device.create_timestamp_sampler(16384).expect("GPU timestamp sampler");
    let _offline = PhysicsStepScope::for_render(true);
    let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "gpu-flip-frame-perf");
    let json: Value = serde_json::from_str(PRESET).expect("GPU FLIP dam break preset parses");
    let params = manifest(&json);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json.to_string(),
        &PrimitiveRegistry::with_builtin(),
        std::sync::Arc::clone(device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("GPU FLIP dam break builds");
    println!("gpu_flip_frame_perf: {} at {WIDTH}x{HEIGHT}, {MEASURED_FRAMES} frames, every {TIMESTAMP_EVERY}th timestamped", device.device_name());
    let mut frame = 0i64;
    let warmup_started = Instant::now();
    loop {
        frame += 1;
        render(&mut runtime, device, &target, &context(frame, 0), &params, None);
        if !runtime.warmup_pending() {
            break;
        }
        assert!(warmup_started.elapsed().as_secs() < 30, "asset warmup did not settle");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut plain_gpu = Vec::new();
    let mut plain_cpu = Vec::new();
    let mut stamped_gpu = Vec::new();
    let mut per_type: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut per_label: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut node_error_frames = 0usize;
    let mut dropped_stamped = 0usize;
    for tick in 0..MEASURED_FRAMES {
        frame += 1;
        let stamped = tick % TIMESTAMP_EVERY == TIMESTAMP_EVERY - 1;
        let result = render(
            &mut runtime,
            device,
            &target,
            &context(frame, tick as u32),
            &params,
            stamped.then_some(&sampler),
        );
        node_error_frames += usize::from(result.node_error);
        match result.split {
            None => {
                plain_gpu.push(result.gpu_ms);
                plain_cpu.push(result.cpu_ms);
            }
            Some((types, labels)) => {
                stamped_gpu.push(result.gpu_ms);
                if types.is_empty() {
                    dropped_stamped += 1;
                    continue;
                }
                for (name, ms) in types {
                    per_type.entry(name).or_default().push(ms);
                }
                for (name, ms) in labels {
                    per_label.entry(name).or_default().push(ms);
                }
            }
        }
    }
    println!("  frames with a node error (live still presents them): {node_error_frames} of {MEASURED_FRAMES}");
    println!(
        "  plain frames ({}): GPU p50 {:.2} ms p95 {:.2} ms | CPU encode p50 {:.2} ms p95 {:.2} ms",
        plain_gpu.len(),
        percentile(&plain_gpu, 0.5),
        percentile(&plain_gpu, 0.95),
        percentile(&plain_cpu, 0.5),
        percentile(&plain_cpu, 0.95),
    );
    println!(
        "  timestamped frames ({}): GPU p50 {:.2} ms p95 {:.2} ms; {dropped_stamped} dropped from the split (stale calibration), {} kept",
        stamped_gpu.len(),
        percentile(&stamped_gpu, 0.5),
        percentile(&stamped_gpu, 0.95),
        stamped_gpu.len() - dropped_stamped,
    );
    print_split("per node type", &per_type);
    print_split("gpu_flip_step per dispatch label", &per_label);
}
