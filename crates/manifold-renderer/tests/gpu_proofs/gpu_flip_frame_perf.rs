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
//! `node.render_scene` is split per pass label and encoder kind the same way.
//! Present is not timed here: that is the app with MANIFOLD_RENDER_TRACE=1.
//! Timing is reported, never gated. Two things are checked: the raster shadow
//! map stays cached on every timestamped frame (its casters are the static
//! cubes; the water is transmissive and never a caster), and every timestamped
//! frame's output is hashed, so a bit-exact render lever is proven by the
//! hashes matching run to run (the simulation is deterministic).

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
const RENDER: &str = "node.render_scene";
const SHADOW_LABEL: &str = "node.render_scene shadow";
const BYTES_PER_PIXEL: u32 = 8;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

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

/// A timestamped frame's split: per node type, per dispatch label inside the
/// solver, per pass label (with its encoder kind) inside render_scene, and the
/// output hash.
struct Split {
    per_type: BTreeMap<String, f64>,
    per_step_label: BTreeMap<String, f64>,
    per_render_label: BTreeMap<String, f64>,
    shadow_rendered: bool,
    hash: u64,
}

/// One frame's numbers. Plain frames fill `gpu_ms` and `cpu_ms`; timestamped
/// frames also fill the split.
struct Frame {
    gpu_ms: f64,
    cpu_ms: f64,
    node_error: bool,
    split: Option<Split>,
}

fn fnv1a(seed: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(seed, |h, b| (h ^ u64::from(b)).wrapping_mul(FNV_PRIME))
}

/// FNV-1a over the target's bytes, read back through its own encoder so the
/// copy never lands in the profiled frame.
fn output_hash(device: &GpuDevice, target: &RenderTarget) -> u64 {
    let bytes_per_row = target.width * BYTES_PER_PIXEL;
    let total = u64::from(target.height * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("gpu flip frame hash");
    enc.copy_texture_to_buffer(&target.texture, &buf, target.width, target.height, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    // SAFETY: the buffer is `total` bytes, shared, and the copy has completed.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ptr, total as usize) };
    fnv1a(FNV_OFFSET, bytes.iter().copied())
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
    let steps: BTreeMap<String, String> =
        runtime.take_step_profiles().into_iter().map(|step| (step.tag, step.type_id)).collect();
    let mut per_type = BTreeMap::new();
    let mut per_step_label = BTreeMap::new();
    let mut per_render_label = BTreeMap::new();
    let mut shadow_rendered = false;
    for span in &profile.spans {
        let Some(type_id) = steps.get(&span.tag) else {
            *per_type.entry("(untagged)".to_owned()).or_insert(0.0) += span.millis;
            continue;
        };
        *per_type.entry(type_id.clone()).or_insert(0.0) += span.millis;
        if type_id == STEP {
            *per_step_label.entry(span.label.clone()).or_insert(0.0) += span.millis;
        } else if type_id == RENDER {
            shadow_rendered |= span.label == SHADOW_LABEL;
            *per_render_label.entry(format!("{:?} {}", span.kind, span.label)).or_insert(0.0) += span.millis;
        }
    }
    let hash = output_hash(device, target);
    Frame {
        gpu_ms,
        cpu_ms,
        node_error,
        split: Some(Split { per_type, per_step_label, per_render_label, shadow_rendered, hash }),
    }
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
    let mut per_render: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut shadow_frames = Vec::new();
    let mut hashes = Vec::new();
    let mut node_error_frames = 0usize;
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
            Some(split) => {
                stamped_gpu.push(result.gpu_ms);
                for (name, ms) in split.per_type {
                    per_type.entry(name).or_default().push(ms);
                }
                for (name, ms) in split.per_step_label {
                    per_label.entry(name).or_default().push(ms);
                }
                for (name, ms) in split.per_render_label {
                    per_render.entry(name).or_default().push(ms);
                }
                if split.shadow_rendered {
                    shadow_frames.push(tick);
                }
                hashes.push((tick, split.hash));
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
        "  timestamped frames ({}): GPU p50 {:.2} ms p95 {:.2} ms",
        stamped_gpu.len(),
        percentile(&stamped_gpu, 0.5),
        percentile(&stamped_gpu, 0.95),
    );
    print_split("per node type", &per_type);
    print_split("gpu_flip_step per dispatch label", &per_label);
    print_split("render_scene per pass label", &per_render);
    let combined = fnv1a(FNV_OFFSET, hashes.iter().flat_map(|&(_, hash)| hash.to_le_bytes()));
    println!("  output hash over the timestamped frames: {combined:016x}");
    for (tick, hash) in &hashes {
        println!("    tick {tick}: {hash:016x}");
    }
    println!("  timestamped frames that re-rendered the shadow map: {shadow_frames:?}");
    assert!(
        shadow_frames.is_empty(),
        "the raster shadow map must stay cached: its casters are static, so a re-render means the dirty key moved"
    );
}
