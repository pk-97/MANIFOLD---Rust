//! BUG-l2h3.24 (GPU FLIP speed after the physics ports) — the whole frame
//! Peter sees on "Water — Dam Break (GPU FLIP)" at 1920×1080, as the preset
//! ships (resolution 64, Steps 1, Auto iterations), one deterministic tick a
//! frame from tick 0. 300 measured frames after asset warm-up: every tenth
//! frame carries per-dispatch GPU timestamps (split per node type, plus per
//! dispatch label inside `node.gpu_flip_step` and `node.whitewater_step`,
//! keyed on the node's tag and the label, so the solvers stay readable);
//! the other frames are plain and give the budget numbers, whole-frame GPU ms
//! and CPU encode (wall time around `runtime.render`). Timestamped frames
//! open one encoder per dispatch and turn encode replay off
//! (ENCODE_REPLAY_DESIGN.md D7), so their split is a ratio, never the budget.
//! `node.render_scene` is split per pass label and encoder kind the same way.
//! The whitewater's published counts (foam, bubble, spray, pool full) are
//! read back every frame through the node preview so a speed change that
//! moved the particle population shows up beside the time it saved.
//! Every split is reported twice, for the splash (ticks before
//! `SPLASH_END_TICK`, the column falling and hitting the far wall) and for
//! the calm after it, because the two phases have different costs and Peter
//! hears both on stage. Plain frames print their GPU ms per tick so a
//! bimodal (fast/slow alternating) phase is visible as a sequence, not
//! hidden in a percentile.
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
const TIMESTAMP_EVERY: usize = 5;
/// First tick of the calm phase. The column has fallen, hit the far wall
/// and the sloshing has settled into a swaying pool by here (checked on the
/// plain-frame GPU sequence this probe prints: the splash hump ends before it).
const SPLASH_END_TICK: usize = 150;
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const STEP: &str = "node.gpu_flip_step";
const WHITEWATER: &str = "node.whitewater_step";
/// The preset's whitewater node, whose scalar outputs carry the counts.
const WHITEWATER_NODE: &str = "whitewater";
/// Node types split per dispatch label.
const LABELLED: [&str; 2] = [STEP, WHITEWATER];
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

/// A dispatch label of one node: (type id, node tag, label).
type LabelKey = (String, String, String);

/// A timestamped frame's split: per node type, per dispatch label inside the
/// labelled solvers, per pass label (with its encoder kind) inside
/// render_scene, and the output hash.
struct Split {
    per_type: BTreeMap<String, f64>,
    per_step_label: BTreeMap<LabelKey, f64>,
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

/// The whitewater node's published counts after a frame (they lag the
/// frame that wrote them by the readback).
#[derive(Clone, Copy, Default)]
struct Counts {
    foam: f64,
    bubble: f64,
    spray: f64,
    pool_full: f64,
}

impl Counts {
    fn read(runtime: &PresetRuntime) -> Self {
        let (_, outputs) = runtime.preview_scalar_io();
        let port = |name: &str| outputs.iter().find_map(|(port, value)| (port == name).then_some(f64::from(*value))).unwrap_or(0.0);
        Self { foam: port("foam_count"), bubble: port("bubble_count"), spray: port("spray_count"), pool_full: port("pool_full") }
    }

    fn live(self) -> f64 {
        self.foam + self.bubble + self.spray
    }
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
        if LABELLED.contains(&type_id.as_str()) {
            let key = (type_id.clone(), span.tag.clone(), span.label.clone());
            *per_step_label.entry(key).or_insert(0.0) += span.millis;
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
        println!("    {name:<52} p50 {:>8.3} ms  p95 {:>8.3} ms", percentile(samples, 0.5), percentile(samples, 0.95));
    }
}

/// One phase's accumulated numbers: plain-frame budgets plus the
/// timestamped splits.
#[derive(Default)]
struct Phase {
    plain_gpu: Vec<f64>,
    plain_cpu: Vec<f64>,
    stamped_gpu: Vec<f64>,
    per_type: BTreeMap<String, Vec<f64>>,
    per_label: BTreeMap<LabelKey, Vec<f64>>,
    per_render: BTreeMap<String, Vec<f64>>,
    counts: Vec<Counts>,
}

impl Phase {
    fn add(&mut self, result: Frame, counts: Counts) {
        self.counts.push(counts);
        match result.split {
            None => {
                self.plain_gpu.push(result.gpu_ms);
                self.plain_cpu.push(result.cpu_ms);
            }
            Some(split) => {
                self.stamped_gpu.push(result.gpu_ms);
                for (name, ms) in split.per_type {
                    self.per_type.entry(name).or_default().push(ms);
                }
                for (name, ms) in split.per_step_label {
                    self.per_label.entry(name).or_default().push(ms);
                }
                for (name, ms) in split.per_render_label {
                    self.per_render.entry(name).or_default().push(ms);
                }
            }
        }
    }

    fn report(&self, name: &str) {
        println!("== {name} ==");
        println!(
            "  plain frames ({}): GPU p50 {:.2} ms p95 {:.2} ms | CPU encode p50 {:.2} ms p95 {:.2} ms",
            self.plain_gpu.len(),
            percentile(&self.plain_gpu, 0.5),
            percentile(&self.plain_gpu, 0.95),
            percentile(&self.plain_cpu, 0.5),
            percentile(&self.plain_cpu, 0.95),
        );
        println!(
            "  timestamped frames ({}): GPU p50 {:.2} ms p95 {:.2} ms",
            self.stamped_gpu.len(),
            percentile(&self.stamped_gpu, 0.5),
            percentile(&self.stamped_gpu, 0.95),
        );
        let live: Vec<f64> = self.counts.iter().map(|c| c.live()).collect();
        let last = self.counts.last().copied().unwrap_or_default();
        println!(
            "  whitewater live particles: p50 {:.0} p95 {:.0} max {:.0} | last frame foam {:.0} bubble {:.0} spray {:.0} | pool full on {} frames",
            percentile(&live, 0.5),
            percentile(&live, 0.95),
            live.iter().copied().fold(0.0, f64::max),
            last.foam,
            last.bubble,
            last.spray,
            self.counts.iter().filter(|c| c.pool_full > 0.0).count(),
        );
        print_split("per node type", &self.per_type);
        for type_id in LABELLED {
            let labels: BTreeMap<String, Vec<f64>> = self
                .per_label
                .iter()
                .filter(|((t, _, _), _)| t == type_id)
                .map(|((_, tag, label), samples)| (format!("{tag}: {label}"), samples.clone()))
                .collect();
            print_split(&format!("{type_id} per dispatch label"), &labels);
        }
        print_split("render_scene per pass label", &self.per_render);
    }
}

/// What the probe renders: the preset as it ships, or with one of its draws
/// removed so a render pass can be attributed to the object that owns it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Shipped,
    /// Whitewater Budget at its card minimum (1000 of 100000): the foam,
    /// spray and bubble instanced draws all but vanish.
    WhitewaterMinimum,
    /// The water's scene object unwired from render_scene: no transmissive
    /// water layer, so its depth prepass, colour copy and layer draw vanish.
    WaterUnwired,
}

/// Node ids in the shipped preset (`WaterDamBreakGpuFlip.json`).
const WATER_OBJECT_NODE: u64 = 442;
const RENDER_SCENE_NODE: u64 = 463;
const WHITEWATER_BUDGET_PARAM: &str = "whitewater_capacity";

#[test]
fn gpu_flip_frame_perf() {
    probe(Variant::Shipped);
}

#[test]
fn gpu_flip_frame_perf_whitewater_minimum() {
    probe(Variant::WhitewaterMinimum);
}

#[test]
fn gpu_flip_frame_perf_water_unwired() {
    probe(Variant::WaterUnwired);
}

fn probe(variant: Variant) {
    let harness = harness::shared();
    let device = &harness.device;
    let sampler = device.create_timestamp_sampler(16384).expect("GPU timestamp sampler");
    let _offline = PhysicsStepScope::for_render(true);
    let target = RenderTarget::new(device, WIDTH, HEIGHT, GpuTextureFormat::Rgba16Float, "gpu-flip-frame-perf");
    let mut json: Value = serde_json::from_str(PRESET).expect("GPU FLIP dam break preset parses");
    if variant == Variant::WaterUnwired {
        let wires = json["wires"].as_array_mut().expect("preset wires");
        let before = wires.len();
        wires.retain(|wire| {
            !(wire["fromNode"] == WATER_OBJECT_NODE && wire["toNode"] == RENDER_SCENE_NODE)
        });
        assert_eq!(before - wires.len(), 1, "exactly one wire carries the water into render_scene");
    }
    let mut params = manifest(&json);
    if variant == Variant::WhitewaterMinimum {
        let budget = params.get_mut(WHITEWATER_BUDGET_PARAM).expect("whitewater budget card param");
        let minimum = budget.spec.min;
        assert!(minimum <= 1000.0, "the budget floor moved: {minimum}");
        budget.value = minimum;
        budget.base = minimum;
    }
    println!("gpu_flip_frame_perf variant: {variant:?}");
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
    // The preview copies the node's CPU-side scalars; it adds no GPU work.
    runtime.set_preview_node(Some(&manifold_core::NodeId::from(WHITEWATER_NODE)));
    let mut whole = Phase::default();
    let mut splash = Phase::default();
    let mut calm = Phase::default();
    let mut plain_sequence = Vec::new();
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
        let counts = Counts::read(&runtime);
        node_error_frames += usize::from(result.node_error);
        if let Some(split) = &result.split {
            if split.shadow_rendered {
                shadow_frames.push(tick);
            }
            hashes.push((tick, split.hash));
        } else {
            plain_sequence.push((tick, result.gpu_ms));
        }
        // The splits are moved into one phase and cloned into the whole-run
        // view: a `Frame` is read-only data, so the copy is the price of two
        // tables, never a render.
        let for_whole = Frame {
            gpu_ms: result.gpu_ms,
            cpu_ms: result.cpu_ms,
            node_error: result.node_error,
            split: result.split.as_ref().map(|split| Split {
                per_type: split.per_type.clone(),
                per_step_label: split.per_step_label.clone(),
                per_render_label: split.per_render_label.clone(),
                shadow_rendered: split.shadow_rendered,
                hash: split.hash,
            }),
        };
        whole.add(for_whole, counts);
        if tick < SPLASH_END_TICK { splash.add(result, counts) } else { calm.add(result, counts) }
    }
    println!("  frames with a node error (live still presents them): {node_error_frames} of {MEASURED_FRAMES}");
    println!("  plain-frame GPU ms per tick:");
    for row in plain_sequence.chunks(12) {
        let cells: Vec<String> = row.iter().map(|(tick, ms)| format!("{tick:>3}:{ms:>6.2}")).collect();
        println!("    {}", cells.join(" "));
    }
    whole.report("whole run");
    splash.report(&format!("splash (ticks 0..{SPLASH_END_TICK})"));
    calm.report(&format!("calm (ticks {SPLASH_END_TICK}..{MEASURED_FRAMES})"));
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
