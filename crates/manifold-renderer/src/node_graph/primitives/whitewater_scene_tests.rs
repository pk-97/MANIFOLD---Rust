//! The Whitewater chain on GPU FLIP's Dam Break (docs/GPU_WHITEWATER_DESIGN.md
//! P5, P6). Until the chain moves into the GPU FLIP Dam Break preset
//! (BUG-imy3.4 (whitewater P5 preset remainder)) it is wired straight to its
//! atoms on the Rust scene builder and drawn by the engine preset's own foam,
//! bubble and spray objects. `gpu_flip_builder_whitewater_emits` runs it as the
//! app would, frozen; `whitewater_emitter_matches_flip` (O2,
//! `whitewater-oracle`) holds the GPU emitter to FLIP's on fields the scene
//! captures; `whitewater_side_by_side` renders it beside the FLIP engine's
//! own whitewater with the cost table, when `WHITEWATER_DEMO_DIR` names where.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use serde_json::{Value, json};

use super::gpu_flip_preset::{WaterScene, render_def};
use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::{encode_rgba8_png, readback_srgb_rgba8};
use crate::node_graph::depth_rule::DepthRule;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::PrimitiveRegistry;
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use crate::render_target::RenderTarget;

const LIFECYCLE_REPORTS: [&str; 8] =
    ["foam_count", "bubble_count", "spray_count", "emitted", "thinned", "dropped_ticks", "lifecycle_ms", "worker_ms"];

const STUDIO_FLOOR: [&str; 4] = ["studio_floor", "studio_floor_mesh", "studio_floor_material", "studio_floor_transform"];
const OBSTACLE: [&str; 5] = ["obstacle_transform", "obstacle_collider", "obstacle_mesh", "obstacle_material", "obstacle_object"];

fn preset_json(file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets").join(file);
    serde_json::from_str(&std::fs::read_to_string(path).expect("preset reads")).expect("preset parses")
}

fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

type Port = (u64, &'static str);

const PROBE: &str = "test.scalar_probe";

/// Reads one scalar and nothing reads it. A liveness root, so the planner
/// keeps it and gives the scalar storage; its `value` param shadows the
/// input, so the runtime's live parameter tap reports the wire's value.
struct ScalarProbe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    params: Vec<ParamDef>,
}

impl ScalarProbe {
    fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(PROBE),
            inputs: vec![NodePort { name: Cow::Borrowed("value"), ty: PortType::Scalar(ScalarType::F32), kind: PortKind::Input, required: true }],
            params: vec![ParamDef {
                name: Cow::Borrowed("value"),
                label: "Value",
                ty: ParamType::Float,
                default: ParamValue::Float(f32::NAN),
                range: None,
                enum_values: &[],
            }],
        }
    }
}

impl EffectNode for ScalarProbe {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &self.params
    }
    fn depth_rule(&self) -> DepthRule {
        DepthRule::Terminal
    }
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

/// Nodes and wires appended to a def held as JSON, found by name.
struct Appender {
    def: Value,
    next: u64,
}

impl Appender {
    fn new(def: EffectGraphDef) -> Self {
        Self::from_value(serde_json::to_value(def).expect("def serialises"))
    }

    fn from_value(def: Value) -> Self {
        let next = def["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_u64()).max().map_or(0, |m| m + 1);
        Self { def, next }
    }

    fn named(&self, name: &str) -> &Value {
        let nodes = self.def["nodes"].as_array().expect("nodes");
        nodes.iter().find(|n| n["nodeId"] == name).unwrap_or_else(|| panic!("no node {name}"))
    }

    fn id(&self, name: &str) -> u64 {
        self.named(name)["id"].as_u64().expect("numeric id")
    }

    fn add(&mut self, mut node: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        node["id"] = json!(id);
        self.def["nodes"].as_array_mut().expect("nodes").push(node);
        id
    }

    fn node(&mut self, name: &str, type_id: &str, params: Value) -> u64 {
        self.add(json!({"nodeId": name, "typeId": type_id, "params": params}))
    }

    fn wire(&mut self, from: Port, to: u64, port: &str) {
        let wire = json!({"fromNode": from.0, "fromPort": from.1, "toNode": to, "toPort": port});
        self.def["wires"].as_array_mut().expect("wires").push(wire);
    }

    /// A scalar read after the frame as `probe.<label>`.
    fn probe(&mut self, label: &str, from: Port) {
        let id = self.node(&format!("probe.{label}"), PROBE, json!({}));
        self.wire(from, id, "value");
    }

    /// Drops the named nodes and every wire touching them.
    fn remove(&mut self, names: &[&str]) {
        let ids: Vec<u64> = self.def["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .filter(|n| names.iter().any(|name| n["nodeId"] == *name))
            .filter_map(|n| n["id"].as_u64())
            .collect();
        let gone = |id: &Value| id.as_u64().is_some_and(|id| ids.contains(&id));
        self.def["nodes"].as_array_mut().expect("nodes").retain(|n| !gone(&n["id"]));
        self.def["wires"].as_array_mut().expect("wires").retain(|w| !gone(&w["fromNode"]) && !gone(&w["toNode"]));
    }

    fn finish(self) -> EffectGraphDef {
        serde_json::from_value(self.def).expect("def with whitewater")
    }
}

/// `render_def` of `scene` with its faces published, which for the Dam Break
/// at 64 is the shipped preset with its Whitewater group. The group's reports
/// are probed by name, the frame's particle count as `count`.
fn whitewater_render_def(scene: WaterScene) -> EffectGraphDef {
    let mut g = Appender::new(render_def(scene.with_faces()));
    let group = g.id("whitewater");
    for report in LIFECYCLE_REPORTS {
        g.probe(report, (group, report));
    }
    let frame = g.id("frame");
    g.probe("count", (frame, "count_b"));
    g.finish()
}

/// `WaterDamBreakGpu.json` as the race clips run it (studio floor and
/// obstacle left out), its native whitewater on or off, with the engine's
/// counts and simulation time probed by name.
fn flip_def(whitewater: bool) -> EffectGraphDef {
    let mut g = Appender::from_value(preset_json("WaterDamBreakGpu.json"));
    g.remove(&[&STUDIO_FLOOR[..], &OBSTACLE[..]].concat());
    let on = if whitewater { 1.0 } else { 0.0 };
    // The card owns the switch and overwrites the node's param at build.
    for list in ["params", "bindings"] {
        for p in g.def["presetMetadata"][list].as_array_mut().expect("card list") {
            if p["id"] == "whitewater" {
                p["defaultValue"] = json!(on);
            }
        }
    }
    let engine = g.id("fluid_surface");
    let nodes = g.def["nodes"].as_array_mut().expect("nodes");
    nodes.iter_mut().find(|n| n["nodeId"] == "fluid_surface").expect("engine")["params"]["whitewater"] = float(on);
    for output in ["foam_count", "bubble_count", "spray_count", "simulation_ms"] {
        g.probe(output, (engine, output));
    }
    g.finish()
}

/// One preset on the app's generator path, frame by frame at 60 fps.
struct Show {
    device: crate::TestDevice,
    runtime: PresetRuntime,
    target: RenderTarget,
    size: (u32, u32),
    sampler: manifold_gpu::GpuTimestampSampler,
    /// Per plan step, the index of its whitewater label: a step whose node,
    /// or any member of its fused node, is a `ww.` atom.
    step_label: Vec<Option<usize>>,
    labels: Vec<String>,
    frame_count: i64,
    trigger: u32,
    /// The transport is paused: frames hold the clock with dt 0.
    paused: bool,
}

/// One frame's clocks and, when profiled, each whitewater label's own GPU ms.
struct Frame {
    gpu_ms: f64,
    cpu_ms: f64,
    whitewater_ms: Vec<f64>,
    /// Dispatches the sampler couldn't time.
    untimed: usize,
}

impl Show {
    fn new(def: EffectGraphDef, size: (u32, u32), frozen: bool, held: &[String]) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        registry.register(PROBE, || Box::new(ScalarProbe::new()));
        let (def, retarget) = if frozen {
            let view = crate::node_graph::freeze::install::fuse_generator_view(&def, &registry).expect("the scene fuses");
            ((*view.def).clone(), view.node_retarget.clone())
        } else {
            (def, Default::default())
        };
        let device = crate::test_device();
        let runtime = PresetRuntime::from_def_with_device(def, &registry, device.arc(), size.0, size.1, GpuTextureFormat::Rgba16Float, None)
            .expect("scene builds on the device");
        let target = RenderTarget::new(&device, size.0, size.1, GpuTextureFormat::Rgba16Float, "whitewater-scene");
        let sampler = device.create_timestamp_sampler(8_192).expect("timestamp sampling");
        let mut labels: Vec<String> = Vec::new();
        let mut step_label = Vec::new();
        for step in runtime.plan.steps() {
            let name = runtime.graph.nodes().find(|n| n.id == step.node).map_or_else(String::new, |n| n.node_id.as_str().to_string());
            let mut members: Vec<&str> =
                retarget.iter().filter(|(_, fused)| fused.as_str() == name.as_str()).map(|(member, _)| member.as_str()).collect();
            members.sort_unstable();
            let label = if members.is_empty() { name.clone() } else { members.join("+") };
            let whitewater = label.split('+').any(|m| m.starts_with("ww."));
            step_label.push(whitewater.then(|| match labels.iter().position(|l| *l == label) {
                Some(i) => i,
                None => {
                    labels.push(label);
                    labels.len() - 1
                }
            }));
        }
        let mut show = Self { device, runtime, target, size, sampler, step_label, labels, frame_count: 0, trigger: 0, paused: false };
        let held: Vec<NodeId> = held.iter().map(|name| NodeId::from(name.as_str())).collect();
        show.runtime.set_dump_visible(None, &held);
        show
    }

    /// One frame 1/60 s on. Metal's autoreleased objects drain per frame, as
    /// the content thread drains them.
    fn frame(&mut self, profile: bool) -> Frame {
        objc2::rc::autoreleasepool(|_| self.frame_inner(profile))
    }

    fn frame_inner(&mut self, profile: bool) -> Frame {
        if !self.paused {
            self.frame_count += 1;
        }
        let time = self.frame_count as f64 / 60.0;
        let (w, h) = self.size;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: if self.paused { 0.0 } else { 1.0 / 60.0 },
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: self.frame_count,
            anim_progress: 0.0,
            trigger_count: self.trigger,
        };
        let mut enc = self.device.create_encoder("whitewater-scene");
        if profile {
            enc.enable_dispatch_profiling(self.sampler.clone(), &self.device);
        }
        self.runtime.set_profiling(profile);
        let start = Instant::now();
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &ParamManifest::default());
        }
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let result = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(result.failed_command_buffers, 0, "frame {} failed on the GPU", self.frame_count);
        let mut whitewater_ms = vec![0.0; self.labels.len()];
        if profile {
            self.runtime.take_step_profiles();
            let step_of = |tag: &str| tag.rsplit_once(":s").and_then(|(_, idx)| idx.parse::<usize>().ok());
            for span in &result.spans {
                if let Some(label) = step_of(&span.tag).and_then(|idx| self.step_label.get(idx).copied().flatten()) {
                    whitewater_ms[label] += span.millis;
                }
            }
        }
        self.runtime.set_profiling(false);
        Frame { gpu_ms: result.total_ms, cpu_ms, whitewater_ms, untimed: result.overflow + result.invalid }
    }

    /// The first frame, warm-up, then a trigger restart from the fill, as
    /// the GPU FLIP smoke runs start: the next frame is the liquid's first and
    /// counts as frame 1.
    fn restart(&mut self) {
        self.frame(false);
        let mut warmups = 0;
        while self.runtime.warmup_pending() && warmups < 600 {
            self.frame(false);
            warmups += 1;
        }
        self.trigger += 1;
        self.frame(false);
        self.frame_count = 0;
    }

    /// This frame's values at the named probes.
    fn probes<const N: usize>(&self, labels: [&str; N]) -> [f32; N] {
        let live = self.runtime.live_node_params_watched();
        labels.map(|label| {
            let name = format!("probe.{label}");
            let values = live.iter().find(|(id, _)| id.as_str() == name).map(|(_, values)| values).unwrap_or_else(|| panic!("no {name}"));
            let value = values.iter().find(|(param, _)| *param == "value").map(|&(_, v)| v).expect("the probe reads value");
            assert!(value.is_finite(), "{name} saw no value this frame");
            value
        })
    }

    fn readback(&self) -> Vec<u8> {
        objc2::rc::autoreleasepool(|_| readback_srgb_rgba8(&self.device, &self.target.texture, self.size.0, self.size.1))
    }

    fn errors(&self) -> Vec<String> {
        self.runtime.errors().iter().map(|e| format!("{e:?}")).collect()
    }
}

/// The shipped GPU FLIP Dam Break with its Whitewater group, frozen as the app
/// renders it, 90 frames: no node refuses and foam is up by 1.5 s.
#[test]
fn gpu_flip_whitewater_emits() {
    let scene = WaterScene::dam_break(64);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    let mut last = [0.0; 8];
    for frame in 1..=90 {
        show.frame(false);
        last = show.probes(LIFECYCLE_REPORTS);
        if frame % 15 == 0 {
            let [foam, bubble, spray, emitted, thinned, dropped, ms, worker] = last;
            let [count] = show.probes(["count"]);
            println!(
                "frame {frame}: {count} particles; foam {foam} bubble {bubble} spray {spray}, emitted {emitted}, thinned {thinned}, dropped ticks {dropped}, lifecycle {ms:.2} ms, worker {worker:.2} ms"
            );
        }
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
    assert!(last[0] > 0.0, "no foam by 1.5 s: {last:?}");
    assert_eq!(last[5], 0.0, "offline, the lifecycle never drops a tick");
}

/// D11 at 64, live: the scene's whitewater is updated on the lifecycle's
/// thread (the update refuses any other, so a content-thread update fails
/// here), the population it produces reaches the outputs, and no tick drops
/// while the GPU keeps up. The content thread's and the worker's ms are
/// printed for the cost table, never asserted.
#[test]
fn whitewater_live_scene_updates_on_the_lifecycle_thread() {
    let scene = WaterScene::dam_break(64);
    let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    let (mut content, mut worker) = (Vec::new(), Vec::new());
    let mut last = [0.0; 8];
    for frame in 1..=180 {
        show.frame(false);
        last = show.probes(LIFECYCLE_REPORTS);
        // The first frames compile pipelines and hold little water.
        if frame > 30 {
            content.push(f64::from(last[6]));
            worker.push(f64::from(last[7]));
        }
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
    let row = |values: &[f64]| format!("p50 {:.3} p95 {:.3} max {:.3}", percentile(values, 0.5), percentile(values, 0.95), percentile(values, 1.0));
    println!("WHITEWATER live, frames 31-180: content thread {} ms; lifecycle thread {} ms", row(&content), row(&worker));
    println!("WHITEWATER live at frame 180: {last:?}");
    assert!(last[0] > 0.0, "no foam by 3 s: {last:?}");
    assert!(worker.iter().any(|&ms| ms > 0.0), "the lifecycle thread never reported work");
    assert_eq!(last[5], 0.0, "live, with the GPU waited each frame, no tick drops");
}

/// The pause gesture on the shipped preset: paused mid-splash, the
/// whitewater holds (no emission, the same population, the same picture)
/// and moves on when play resumes.
#[test]
fn gpu_flip_whitewater_holds_while_paused() {
    let scene = WaterScene::dam_break(64);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    for _ in 0..60 {
        show.frame(false);
    }
    let playing = show.probes(LIFECYCLE_REPORTS);
    assert!(playing[0] > 0.0, "no foam by 1 s: {playing:?}");
    show.paused = true;
    show.frame(false);
    let (held, image) = (show.probes(LIFECYCLE_REPORTS), show.readback());
    for _ in 0..3 {
        show.frame(false);
    }
    let (still, still_image) = (show.probes(LIFECYCLE_REPORTS), show.readback());
    let changed = image.chunks_exact(4).zip(still_image.chunks_exact(4)).filter(|(a, b)| a != b).count();
    println!("WHITEWATER pause: playing {playing:?}; paused {held:?} then {still:?}; {changed} pixels changed over 3 paused frames");
    // The first paused frame publishes the last playing tick: the lifecycle
    // reads a tick's snapshot a frame after the GPU wrote it (D11).
    assert!(held[3] >= playing[3], "the first paused frame lost emission: {held:?}");
    assert_eq!(still[..6], held[..6], "paused frames moved the whitewater");
    assert_eq!(changed, 0, "paused frames changed the picture");
    show.paused = false;
    for _ in 0..15 {
        show.frame(false);
    }
    let resumed = show.probes(LIFECYCLE_REPORTS);
    assert!(resumed[3] > still[3], "no emission after play resumed: {resumed:?}");
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Peter watches on his phone; its upload limit is 30 MB.
const PHONE_LIMIT_BYTES: u64 = 25 * 1024 * 1024;
const DEMO_SIZE: (u32, u32) = (1920, 1080);
const DEMO_FRAMES: usize = 211;
/// 1.5 s and 3 s, the P5 stills.
const DEMO_STILLS: [usize; 2] = [90, 180];

/// An H.264 file fed raw RGBA frames at 60 fps.
fn encoder(path: &Path) -> std::process::Child {
    let (w, h) = DEMO_SIZE;
    std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-r", "60", "-i", "-"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "16"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("ffmpeg starts")
}

fn finish(mut encoder: std::process::Child) {
    drop(encoder.stdin.take());
    let status = encoder.wait().expect("ffmpeg ran");
    assert!(status.success(), "ffmpeg: {status}");
}

/// Two clips side by side, each cropped to the tank and its splash as the
/// race clips crop them.
fn side_by_side(left: &Path, right: &Path, out: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(left)
        .arg("-i")
        .arg(right)
        .args(["-filter_complex", "[0:v]crop=1200:1080:360:0[l];[1:v]crop=1200:1080:360:0[r];[l][r]hstack=inputs=2[out]"])
        .args(["-map", "[out]", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-movflags", "+faststart"])
        .arg(out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "side by side: {status}");
}

/// A copy under the phone's upload limit, raising the CRF until it fits.
fn phone_copy(source: &Path, phone: &Path) -> Option<(u32, u64)> {
    for crf in [24, 26, 28, 30, 32, 35] {
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(source)
            .args(["-vf", "scale=-2:1080", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", &crf.to_string(), "-movflags", "+faststart"])
            .arg(phone)
            .status();
        let size = std::fs::metadata(phone).map_or(u64::MAX, |m| m.len());
        if status.is_ok_and(|s| s.success()) && size < PHONE_LIMIT_BYTES {
            return Some((crf, size));
        }
    }
    None
}

/// Foam, bubble and spray counts over time, one panel each, FLIP's engine
/// against GPU FLIP's GPU whitewater, drawn with CoreGraphics.
fn plot_counts(path: &Path, flip: &[[f32; 3]], gpu_flip: &[[f32; 3]]) {
    use core_foundation::attributed_string::CFMutableAttributedString;
    use core_foundation::base::{CFRange, TCFType};
    use core_foundation::string::CFString;
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use core_text::font::CTFont;
    use core_text::line::CTLine;
    use core_text::string_attributes::kCTFontAttributeName;

    const W: usize = 1600;
    const H: usize = 1200;
    const LEFT: f64 = 120.0;
    const RIGHT: f64 = 40.0;
    const TOP: f64 = 130.0;
    const BOTTOM: f64 = 70.0;
    const GAP: f64 = 70.0;
    const FLIP_RGB: (f64, f64, f64) = (0.90, 0.45, 0.10);
    const GPU_FLIP_RGB: (f64, f64, f64) = (0.10, 0.40, 0.85);

    let line = |font: &CTFont, text: &str| {
        let text = CFString::new(text);
        let mut attributed = CFMutableAttributedString::new();
        attributed.replace_str(&text, CFRange::init(0, 0));
        // SAFETY: kCTFontAttributeName is a static CoreText attribute key.
        unsafe { attributed.set_attribute(CFRange::init(0, text.char_len()), kCTFontAttributeName, font) };
        CTLine::new_with_attributed_string(attributed.as_concrete_TypeRef())
    };
    let text = |ctx: &CGContext, font: &CTFont, s: &str, x: f64, y: f64, right_aligned: bool| {
        let l = line(font, s);
        let x = if right_aligned { x - l.get_typographic_bounds().width } else { x };
        ctx.set_text_position(x, y);
        l.draw(ctx);
    };
    let space = CGColorSpace::create_device_rgb();
    // kCGImageAlphaPremultipliedLast: RGBA bytes, the PNG encoder's order.
    let mut ctx = CGContext::create_bitmap_context(None, W, H, 8, W * 4, &space, 1);
    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
    ctx.fill_rect(CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(W as f64, H as f64)));
    ctx.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
    let body = core_text::font::new_from_name("Helvetica", 22.0).expect("Helvetica");
    let title = core_text::font::new_from_name("Helvetica-Bold", 28.0).expect("Helvetica Bold");

    let frames = flip.len().max(gpu_flip.len()).max(2);
    let panel_h = (H as f64 - TOP - BOTTOM - 2.0 * GAP) / 3.0;
    let plot_w = W as f64 - LEFT - RIGHT;
    text(&ctx, &title, "Whitewater particles over time, Dam Break at 64", LEFT, H as f64 - 50.0, false);
    for (panel, name) in ["Foam", "Bubbles", "Spray"].into_iter().enumerate() {
        let base = BOTTOM + (2 - panel) as f64 * (panel_h + GAP);
        let peak = flip.iter().chain(gpu_flip).map(|c| c[panel]).fold(1.0_f32, f32::max);
        // Four gridlines at a round step: 1, 2, 2.5 or 5 times a power of ten.
        let raw = f64::from(peak) / 4.0;
        let magnitude = 10f64.powf(raw.log10().floor());
        let step = [1.0, 2.0, 2.5, 5.0, 10.0].into_iter().map(|m| m * magnitude).find(|&s| s >= raw).unwrap_or(raw);
        let top = 4.0 * step;
        let x_of = |frame: usize| LEFT + plot_w * frame as f64 / (frames - 1) as f64;
        let y_of = |v: f32| base + panel_h * f64::from(v) / top;
        ctx.set_rgb_stroke_color(0.85, 0.85, 0.85, 1.0);
        ctx.set_line_width(1.0);
        for tick in 0..=4 {
            let y = base + panel_h * f64::from(tick) / 4.0;
            ctx.move_to_point(LEFT, y);
            ctx.add_line_to_point(LEFT + plot_w, y);
            ctx.stroke_path();
            text(&ctx, &body, &format!("{:.0}", top * f64::from(tick) / 4.0), LEFT - 10.0, y - 7.0, true);
        }
        for second in 0..=(frames - 1) / 60 {
            let x = x_of(second * 60);
            ctx.move_to_point(x, base);
            ctx.add_line_to_point(x, base + panel_h);
            ctx.stroke_path();
            text(&ctx, &body, &format!("{second} s"), x - 10.0, base - 28.0, false);
        }
        text(&ctx, &title, name, LEFT + 12.0, base + panel_h - 34.0, false);
        for (series, (r, g, b)) in [(flip, FLIP_RGB), (gpu_flip, GPU_FLIP_RGB)] {
            ctx.set_rgb_stroke_color(r, g, b, 1.0);
            ctx.set_line_width(3.0);
            for (frame, counts) in series.iter().enumerate() {
                let (x, y) = (x_of(frame), y_of(counts[panel]));
                if frame == 0 {
                    ctx.move_to_point(x, y);
                } else {
                    ctx.add_line_to_point(x, y);
                }
            }
            ctx.stroke_path();
        }
    }
    let legend = [("FLIP engine, native whitewater", FLIP_RGB), ("GPU FLIP, GPU whitewater", GPU_FLIP_RGB)];
    for (k, (label, (r, g, b))) in legend.into_iter().enumerate() {
        let (x, y) = (LEFT + k as f64 * 520.0, H as f64 - 95.0);
        ctx.set_rgb_fill_color(r, g, b, 1.0);
        ctx.fill_rect(CGRect::new(&CGPoint::new(x, y), &CGSize::new(40.0, 8.0)));
        ctx.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
        text(&ctx, &body, label, x + 52.0, y - 4.0, false);
    }
    let rgba = ctx.data().to_vec();
    std::fs::write(path, encode_rgba8_png(&rgba, W as u32, H as u32)).expect("counts plot written");
}

/// A show's first `DEMO_FRAMES` frames at 1080p as `{name}.mp4`, with the
/// P5 stills, and each frame's probes.
fn record<const N: usize>(show: &mut Show, name: &str, dir: &Path, probes: [&str; N]) -> (PathBuf, Vec<[f32; N]>) {
    let clip = dir.join(format!("{name}.mp4"));
    let mut ffmpeg = encoder(&clip);
    let mut values = Vec::new();
    let wall = Instant::now();
    for frame in 1..=DEMO_FRAMES {
        show.frame(false);
        values.push(show.probes(probes));
        let rgba = show.readback();
        if DEMO_STILLS.contains(&frame) {
            let (w, h) = DEMO_SIZE;
            std::fs::write(dir.join(format!("{name}_frame{frame:04}.png")), encode_rgba8_png(&rgba, w, h)).expect("still written");
        }
        ffmpeg.stdin.as_mut().expect("ffmpeg input").write_all(&rgba).expect("clip frame written");
    }
    finish(ffmpeg);
    println!("WHITEWATER {name}: {DEMO_FRAMES} frames in {:.1} s", wall.elapsed().as_secs_f64());
    (clip, values)
}

const FLIP_PROBES: [&str; 4] = ["foam_count", "bubble_count", "spray_count", "simulation_ms"];

/// The engine's simulation ms a frame over `DEMO_FRAMES`, its whitewater on
/// or off, with nothing else drawing on the cores.
fn flip_simulation_ms(whitewater: bool) -> Vec<f64> {
    let mut show = Show::new(flip_def(whitewater), (320, 180), false, &[]);
    let ms = (0..DEMO_FRAMES)
        .map(|_| {
            show.frame(false);
            let [foam, bubble, spray, ms] = show.probes(FLIP_PROBES);
            assert!(whitewater || foam + bubble + spray == 0.0, "the engine's whitewater is off");
            f64::from(ms)
        })
        .collect();
    let errors = show.errors();
    assert!(errors.is_empty(), "the engine ran with errors: {errors:#?}");
    ms
}

/// P6: the Dam Break at 64 for 211 frames, the FLIP engine with its native
/// whitewater left and GPU FLIP with the GPU whitewater right, through
/// `WaterDamBreakGpu.json`'s camera, lights, tank and materials (studio
/// floor and obstacle left out, as in the race clips). Writes
/// `side_by_side.mp4` with a phone copy, `counts.png` and `counts.csv`,
/// stills at 1.5 s and 3 s, and prints the cost table: GPU ms per whitewater
/// kernel, the lifecycle's CPU ms, and FLIP's whitewater cost as its
/// simulation time with whitewater on minus off. The costs come from second
/// runs that neither read back nor encode, since the encoder takes every
/// core the lifecycle and the engine would use. No-op unless
/// `WHITEWATER_DEMO_DIR` is set.
#[test]
fn whitewater_side_by_side() {
    let Some(dir) = std::env::var_os("WHITEWATER_DEMO_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("output directory");
    let scene = WaterScene::dam_break(64);
    let gpu_flip_show = || {
        let mut show = Show::new(whitewater_render_def(scene), DEMO_SIZE, true, &[]);
        show.restart();
        show
    };

    let mut flip = Show::new(flip_def(true), DEMO_SIZE, false, &[]);
    let (flip_clip, flip_values) = record(&mut flip, "flip", &dir, FLIP_PROBES);
    drop(flip);
    let mut gpu_flip = gpu_flip_show();
    let (gpu_flip_clip, gpu_flip_values) = record(&mut gpu_flip, "gpu_flip", &dir, LIFECYCLE_REPORTS);
    let errors = gpu_flip.errors();
    drop(gpu_flip);
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");

    let (flip_on_ms, flip_off_ms) = (flip_simulation_ms(true), flip_simulation_ms(false));
    // Live, as the show runs: offline the node also waits for its worker.
    let live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut gpu_flip = gpu_flip_show();
    let (gpu_flip_frames, gpu_flip_lifecycle_ms): (Vec<Frame>, Vec<[f64; 2]>) = (0..DEMO_FRAMES)
        .map(|_| {
            let frame = gpu_flip.frame(true);
            (frame, gpu_flip.probes(["lifecycle_ms", "worker_ms"]).map(f64::from))
        })
        .unzip();
    let labels = gpu_flip.labels.clone();
    drop(gpu_flip);
    drop(live);

    let clip = dir.join("side_by_side.mp4");
    side_by_side(&flip_clip, &gpu_flip_clip, &clip);
    let phone = dir.join("side_by_side_phone.mp4");
    match phone_copy(&clip, &phone) {
        Some((crf, bytes)) => println!("WHITEWATER phone copy {} at CRF {crf}: {:.1} MB", phone.display(), bytes as f64 / 1048576.0),
        None => println!("WHITEWATER phone copy {}: could not fit under the limit", phone.display()),
    }
    let flip_counts: Vec<[f32; 3]> = flip_values.iter().map(|v| [v[0], v[1], v[2]]).collect();
    let gpu_flip_counts: Vec<[f32; 3]> = gpu_flip_values.iter().map(|v| [v[0], v[1], v[2]]).collect();
    plot_counts(&dir.join("counts.png"), &flip_counts, &gpu_flip_counts);
    let mut csv = String::from(
        "frame,flip_foam,flip_bubble,flip_spray,gpu_flip_foam,gpu_flip_bubble,gpu_flip_spray,gpu_flip_emitted,flip_simulation_ms,flip_off_simulation_ms,gpu_flip_lifecycle_ms,gpu_flip_worker_ms,gpu_flip_whitewater_gpu_ms\n",
    );
    for frame in 0..DEMO_FRAMES {
        let (f, s) = (flip_values[frame], gpu_flip_values[frame]);
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3}\n",
            frame + 1,
            f[0],
            f[1],
            f[2],
            s[0],
            s[1],
            s[2],
            s[3],
            flip_on_ms[frame],
            flip_off_ms[frame],
            gpu_flip_lifecycle_ms[frame][0],
            gpu_flip_lifecycle_ms[frame][1],
            gpu_flip_frames[frame].whitewater_ms.iter().sum::<f64>()
        ));
    }
    std::fs::write(dir.join("counts.csv"), csv).expect("counts csv");

    // The cost table, from frame 11 on: the first frames compile pipelines.
    let settled = 10..DEMO_FRAMES;
    println!("WHITEWATER cost, frames {}..={DEMO_FRAMES}, p50 and p95 ms", settled.start + 1);
    let mut total = vec![0.0; settled.len()];
    for (k, label) in labels.iter().enumerate() {
        let ms: Vec<f64> = gpu_flip_frames[settled.clone()].iter().map(|f| f.whitewater_ms[k]).collect();
        for (t, m) in total.iter_mut().zip(&ms) {
            *t += m;
        }
        println!("WHITEWATER   GPU {label:<56} {:7.3} {:7.3}", percentile(&ms, 0.5), percentile(&ms, 0.95));
    }
    let untimed = gpu_flip_frames.iter().map(|f| f.untimed).max().unwrap_or(0);
    let lifecycle: Vec<f64> = gpu_flip_lifecycle_ms[settled.clone()].iter().map(|ms| ms[0]).collect();
    let worker: Vec<f64> = gpu_flip_lifecycle_ms[settled.clone()].iter().map(|ms| ms[1]).collect();
    let frame_gpu: Vec<f64> = gpu_flip_frames[settled.clone()].iter().map(|f| f.gpu_ms).collect();
    let frame_cpu: Vec<f64> = gpu_flip_frames[settled.clone()].iter().map(|f| f.cpu_ms).collect();
    let (on, off) = (&flip_on_ms[settled.clone()], &flip_off_ms[settled.clone()]);
    let delta: Vec<f64> = on.iter().zip(off).map(|(a, b)| a - b).collect();
    let (gpu_p95, life_p95) = (percentile(&total, 0.95), percentile(&lifecycle, 0.95));
    let row = |name: &str, values: &[f64]| println!("WHITEWATER   {name:<60} {:7.3} {:7.3}", percentile(values, 0.5), percentile(values, 0.95));
    row("GPU whitewater total (target p95 <= 2)", &total);
    row("content-thread lifecycle_ms, live (target p95 <= 3)", &lifecycle);
    row("lifecycle thread worker_ms", &worker);
    row("GPU FLIP whole frame GPU", &frame_gpu);
    row("GPU FLIP whole frame CPU", &frame_cpu);
    row("FLIP simulation_ms, whitewater on", on);
    row("FLIP simulation_ms, whitewater off", off);
    row("FLIP whitewater cost (on minus off, frame by frame)", &delta);
    println!("WHITEWATER   untimed dispatches on the worst frame: {untimed}");
    println!(
        "WHITEWATER verdict: GPU {} the 2 ms p95 target; the content thread {} the 3 ms p95 target",
        if gpu_p95 <= 2.0 { "meets" } else { "misses" },
        if life_p95 <= 3.0 { "meets" } else { "misses" },
    );
    for frame in DEMO_STILLS {
        let (f, s) = (flip_values[frame - 1], gpu_flip_values[frame - 1]);
        println!("WHITEWATER frame {frame}: FLIP foam {} bubble {} spray {}; GPU FLIP foam {} bubble {} spray {}", f[0], f[1], f[2], s[0], s[1], s[2]);
    }
    println!("WHITEWATER wrote {} and {}", clip.display(), dir.join("counts.png").display());
}

/// O2 (section 3.7): the GPU emitter against FLIP's own on the same inputs.
#[cfg(feature = "whitewater-oracle")]
mod emitter_oracle {
    use manifold_fluids::{
        WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterLifecycle as NativeLifecycle, WhitewaterParticle, WhitewaterSpawn,
        whitewater_oracle,
    };
    use manifold_gpu::GpuBuffer;

    use super::super::emission_count::EmissionCount;
    use super::super::energy_potential::EnergyPotential;
    use super::super::jitter_particles::JitterParticles;
    use super::super::liquid_surface_tests::{Harness, params, read};
    use super::super::sample_faces_at_particles::SampleFacesAtParticles;
    use super::super::spawn_whitewater::SpawnWhitewater;
    use super::super::gpu_flip_preset::REST_PER_CELL;
    use super::super::wavecrest_potential::WavecrestPotential;
    use super::super::whitewater_type::WhitewaterType;
    use super::*;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::fluid_particles::FluidParticle;
    use crate::node_graph::liquid::grid::face_len;
    use crate::node_graph::primitive::Primitive;
    use crate::node_graph::whitewater::KnownValue;

    /// The whitewater grid: the surface's solid lattice, its cells and box, and
    /// the face grid centred in it.
    #[derive(Clone, Copy, Debug)]
    struct GridBox {
        center: [f64; 3],
        size: [f64; 3],
        nodes: f64,
        h: f64,
        face_cells: f64,
    }

    impl GridBox {
        /// The solid lattice the GPU FLIP domain publishes for `scene`, and its face
        /// grid.
        fn of(scene: WaterScene) -> Self {
            let n = scene.pressure.n;
            let lattice = crate::node_graph::liquid::lattice::LiquidLattice::from_layout(&scene.layout());
            let bounds = lattice.bounds();
            let nodes = lattice.nodes();
            assert!(nodes.iter().all(|&v| v == nodes[0]), "a cubic lattice: {nodes:?}");
            Self {
                center: bounds.pos.map(f64::from),
                size: bounds.scale.map(f64::from),
                nodes: f64::from(nodes[0]),
                h: f64::from(lattice.cell_size()),
                face_cells: n as f64,
            }
        }

        fn values(&self) -> Vec<(&'static str, f64)> {
            let mut values = Vec::new();
            for axis in 0..3 {
                values.push((["center_x", "center_y", "center_z"][axis], self.center[axis]));
                values.push((["size_x", "size_y", "size_z"][axis], self.size[axis]));
                values.push((["nodes_x", "nodes_y", "nodes_z"][axis], self.nodes));
            }
            values
        }

        fn face_values(&self) -> [(&'static str, f64); 3] {
            [("face_cells_x", self.face_cells), ("face_cells_y", self.face_cells), ("face_cells_z", self.face_cells)]
        }
    }

    const FRAMES: [usize; 4] = [30, 60, 90, 120];
    const CAPACITY: u32 = 250_000;
    const TANK: f32 = 4.0;
    const SPACE_BINS: usize = 8;
    const LIFE_BINS: usize = 10;
    const MAX_LIFETIME: f32 = 7.0;
    const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];
    const DT: f64 = 1.0 / 60.0;
    const SEEDS: u32 = 16;
    /// Past this a frame fails as too noisy to judge rather than passing.
    /// The shipped preset's frame 90 emits under one particle a seed, so its
    /// histograms settle only past 4,096 seeds.
    const SEED_LIMIT: u32 = 16_384;
    const TOTAL_TOLERANCE: f64 = 0.05;
    const KIND_TOLERANCE: f64 = 0.10;
    const KIND_FLOOR: f64 = 200.0;
    const SPACE_TOLERANCE: f64 = 0.15;
    const LIFE_TOLERANCE: f64 = 0.10;

    /// One frame's emitter inputs, as the scene handed them to its chain.
    struct Captured {
        frame: usize,
        count: u32,
        particles: Vec<FluidParticle>,
        faces: [Vec<f32>; 3],
        distance: Vec<f32>,
        curvature: Vec<KnownValue>,
        cells: Vec<u32>,
        solid: Vec<f32>,
    }

    impl Show {
        fn dumped<T: bytemuck::Pod>(&self, name: &str, port: &str, len: usize) -> Vec<T> {
            let arrays = self.runtime.dump_arrays_all();
            let array = arrays
                .iter()
                .find(|a| a.name == name && a.port == port)
                .unwrap_or_else(|| panic!("{name}.{port} is not held; held: {:?}", arrays.iter().map(|a| format!("{}.{}", a.name, a.port)).collect::<Vec<_>>()));
            assert!(array.buffer.size() as usize >= len * std::mem::size_of::<T>(), "{name}.{port} is shorter than {len} records");
            let ptr = array.buffer.mapped_ptr().expect("shared storage");
            // SAFETY: the frame completed and the buffer holds `len` records.
            unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
        }
    }

    fn cells(grid: GridBox) -> u32 {
        grid.nodes as u32 - 1
    }

    fn capture(scene: WaterScene, grid: GridBox) -> Vec<Captured> {
        let held: Vec<String> = ["frame", "ww.distance", "ww.extend2", "ww.cells"].map(String::from).to_vec();
        let mut show = Show::new(whitewater_render_def(scene), (320, 180), false, &held);
        show.restart();
        let lattice = (cells(grid) as usize).pow(3);
        let face_cells = [grid.face_cells as u32; 3];
        let mut captured = Vec::new();
        for frame in 1..=*FRAMES.last().expect("frames") {
            show.frame(false);
            if !FRAMES.contains(&frame) {
                continue;
            }
            captured.push(Captured {
                frame,
                count: show.probes(["count"])[0] as u32,
                particles: show.dumped("frame", "particles_b", scene.particles() as usize),
                faces: [0, 1, 2].map(|axis| show.dumped("frame", ["face_u", "face_v", "face_w"][axis], face_len(face_cells, axis) as usize)),
                distance: show.dumped("ww.distance", "out", lattice),
                curvature: show.dumped("ww.extend2", "out", lattice),
                cells: show.dumped("ww.cells", "out", lattice),
                solid: show.dumped("frame", "solid_b", (grid.nodes as usize).pow(3)),
            });
        }
        let errors = show.errors();
        assert!(errors.is_empty(), "the capture ran with errors: {errors:#?}");
        captured
    }

    /// What one seed of one side left after emission and one lifecycle step.
    #[derive(Clone)]
    struct Outcome {
        kinds: [f64; 3],
        space: Vec<f64>,
        life: Vec<f64>,
    }

    impl Outcome {
        fn of(particles: &[WhitewaterParticle], min: [f64; 3]) -> Self {
            let mut out = Self { kinds: [0.0; 3], space: vec![0.0; SPACE_BINS.pow(3)], life: vec![0.0; LIFE_BINS] };
            let bin = |x: f32, bins: usize| ((x * bins as f32).floor() as i64).clamp(0, bins as i64 - 1) as usize;
            for p in particles {
                let kind = match p.kind {
                    WhitewaterKind::Foam => 0,
                    WhitewaterKind::Bubble => 1,
                    WhitewaterKind::Spray => 2,
                };
                out.kinds[kind] += 1.0;
                let at = |a: usize| bin((p.position[a] - min[a] as f32) / TANK, SPACE_BINS);
                out.space[at(0) + SPACE_BINS * (at(1) + SPACE_BINS * at(2))] += 1.0;
                out.life[bin(p.lifetime / MAX_LIFETIME, LIFE_BINS)] += 1.0;
            }
            out
        }

        fn total(&self) -> f64 {
            self.kinds.iter().sum()
        }
    }

    type Array = (Slot, GpuBuffer);

    /// The GPU emitter chain as standalone atoms on one harness whose arrays
    /// are made once and rewritten each frame.
    struct Chain {
        harness: Harness,
        grid: GridBox,
        slots: usize,
        particles: Array,
        faces: [Array; 3],
        distance: Array,
        curvature: Array,
        cells: Array,
        solid: Array,
        jittered: Array,
        sampled: Array,
        energy: Array,
        wavecrest: Array,
        counts: Array,
        offsets: Array,
        spawns: Array,
        typed: Array,
    }

    fn write<T: bytemuck::Pod>(buffer: &GpuBuffer, values: &[T]) {
        assert!(buffer.size() as usize >= std::mem::size_of_val(values), "the harness array holds the frame");
        // SAFETY: shared storage at least this large; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    }

    impl Chain {
        fn new(grid: GridBox, slots: usize, solid: &[f32]) -> Self {
            let mut h = Harness::new();
            let lattice = (cells(grid) as usize).pow(3);
            let face_cells = [grid.face_cells as u32; 3];
            let particles = h.array::<FluidParticle>(&[], slots);
            let faces = [0, 1, 2].map(|axis| h.array::<f32>(&[], face_len(face_cells, axis) as usize));
            let distance = h.array::<f32>(&[], lattice);
            let curvature = h.array::<KnownValue>(&[], lattice);
            let cells = h.array::<u32>(&[], lattice);
            let solid = h.array::<f32>(solid, solid.len());
            let jittered = h.array::<FluidParticle>(&[], slots);
            let sampled = h.array::<FluidParticle>(&[], slots);
            let energy = h.array::<f32>(&[], slots);
            let wavecrest = h.array::<f32>(&[], slots);
            let counts = h.array::<u32>(&[], slots);
            let offsets = h.array::<u32>(&[], slots);
            let spawns = h.array::<WhitewaterSpawn>(&[], CAPACITY as usize);
            let typed = h.array::<WhitewaterSpawn>(&[], CAPACITY as usize);
            Self {
                harness: h,
                grid,
                slots,
                particles,
                faces,
                distance,
                curvature,
                cells,
                solid,
                jittered,
                sampled,
                energy,
                wavecrest,
                counts,
                offsets,
                spawns,
                typed,
            }
        }

        fn load(&mut self, c: &Captured) {
            assert_eq!(c.particles.len(), self.slots);
            write(&self.particles.1, &c.particles);
            for (array, values) in self.faces.iter().zip(&c.faces) {
                write(&array.1, values);
            }
            write(&self.distance.1, &c.distance);
            write(&self.curvature.1, &c.curvature);
            write(&self.cells.1, &c.cells);
        }

        fn step<P: Primitive>(&mut self, mut prim: P, inputs: &[(&'static str, Slot)], out: Slot, values: &[(&'static str, f64)]) {
            let values: Vec<(&'static str, f32)> = values.iter().map(|&(name, v)| (name, v as f32)).collect();
            let (_, errors) = self.harness.run(&mut prim, inputs, &[("out", out)], &params(&values));
            assert!(errors.is_empty(), "{errors:?}");
        }

        /// This frame's typed spawns at `seed`, made as the graph makes them.
        fn spawns(&mut self, count: u32, seed: u32) -> Vec<WhitewaterSpawn> {
            let grid = self.grid;
            let boxed = grid.values();
            let faced: Vec<(&'static str, f64)> = [&boxed[..], &grid.face_values()[..]].concat();
            let seed = f64::from(seed);
            let faces = [("face_u", self.faces[0].0), ("face_v", self.faces[1].0), ("face_w", self.faces[2].0)];
            self.step(JitterParticles::new(), &[("particles", self.particles.0)], self.jittered.0, &[("cell_size", grid.h), ("seed", seed), ("epoch", 0.0)]);
            let sample = [&[("particles", self.jittered.0)][..], &faces[..]].concat();
            self.step(SampleFacesAtParticles::new(), &sample, self.sampled.0, &faced);
            self.step(EnergyPotential::new(), &[("particles", self.sampled.0)], self.energy.0, &[]);
            let crest = [("particles", self.sampled.0), ("distance", self.distance.0), ("curvature", self.curvature.0), ("cells", self.cells.0)];
            self.step(WavecrestPotential::new(), &crest, self.wavecrest.0, &boxed);
            let emit = [("particles", self.sampled.0), ("energy", self.energy.0), ("wavecrest", self.wavecrest.0)];
            let emit_values = [("points_per_cell", REST_PER_CELL), ("ticks", 1.0), ("live_count", f64::from(count))];
            self.step(EmissionCount::new(), &emit, self.counts.0, &emit_values);
            let counts: Vec<u32> = read(&self.counts.1, count as usize);
            let offsets: Vec<u32> = counts
                .iter()
                .scan(0u32, |sum, &n| {
                    *sum += n;
                    Some(*sum)
                })
                .collect();
            write(&self.offsets.1, &offsets);
            let total = offsets.last().copied().unwrap_or(0);
            assert!(total <= CAPACITY, "{total} spawns past the oracle's capacity");
            let spawn = [&[("offsets", self.offsets.0), ("particles", self.sampled.0), ("energy", self.energy.0), ("solid", self.solid.0)][..], &faces[..]].concat();
            let mut spawn_values = faced.clone();
            spawn_values.extend([
                ("capacity", f64::from(CAPACITY)),
                ("emitters", f64::from(count)),
                ("seed", seed),
                ("epoch", 0.0),
                ("min_lifetime", 0.0),
                ("max_lifetime", f64::from(MAX_LIFETIME)),
                ("lifetime_variance", 0.0),
            ]);
            self.step(SpawnWhitewater::new(), &spawn, self.spawns.0, &spawn_values);
            let typing = [("spawns", self.spawns.0), ("distance", self.distance.0), ("cells", self.cells.0)];
            self.step(WhitewaterType::new(), &typing, self.typed.0, &boxed);
            read(&self.typed.1, total as usize)
        }
    }

    fn lifecycle(grid: GridBox, c: &Captured, solid: &[f32], seed: u32) -> NativeLifecycle {
        let origin = std::array::from_fn(|a| (grid.center[a] - 0.5 * grid.size[a]) as f32);
        let whitewater_grid = WhitewaterGrid { cells: [cells(grid); 3], cell_size: grid.h as f32, origin };
        let mut lifecycle = NativeLifecycle::new(whitewater_grid, CAPACITY, u64::from(seed)).expect("lifecycle");
        let fields = WhitewaterFields {
            face_u: &c.faces[0],
            face_v: &c.faces[1],
            face_w: &c.faces[2],
            face_cells: [grid.face_cells as u32; 3],
            face_offset: [(cells(grid) - grid.face_cells as u32) / 2; 3],
            level: &c.distance,
            solid,
            gravity: GRAVITY,
        };
        lifecycle.set_fields(&fields).expect("fields");
        lifecycle
    }

    fn mean(values: &[f64]) -> f64 {
        values.iter().sum::<f64>() / values.len() as f64
    }

    /// The standard error of the mean.
    fn error(values: &[f64]) -> f64 {
        let (m, n) = (mean(values), values.len() as f64);
        (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0) / n).sqrt()
    }

    /// L1 between the two sides' pooled histograms, each normalised.
    fn l1(a: &[Outcome], b: &[Outcome], pick: fn(&Outcome) -> &[f64]) -> f64 {
        let pool = |side: &[Outcome]| {
            let mut sum = vec![0.0; pick(&side[0]).len()];
            for o in side {
                for (s, v) in sum.iter_mut().zip(pick(o)) {
                    *s += v;
                }
            }
            let total = sum.iter().sum::<f64>().max(1.0);
            sum.into_iter().map(|v| v / total).collect::<Vec<_>>()
        };
        pool(a).iter().zip(pool(b)).map(|(x, y)| (x - y).abs()).sum()
    }

    fn totals(side: &[Outcome]) -> Vec<f64> {
        side.iter().map(Outcome::total).collect()
    }

    fn kind(side: &[Outcome], k: usize) -> Vec<f64> {
        side.iter().map(|o| o.kinds[k]).collect()
    }

    /// The types the per-type gate covers: FLIP emitted at least 200 of
    /// them over the seeds.
    fn kinds_in_scope(flip: &[Outcome]) -> Vec<usize> {
        (0..3).filter(|&k| kind(flip, k).iter().sum::<f64>() >= KIND_FLOOR).collect()
    }

    /// How far each gated measure could move by chance at this many seeds,
    /// against half its tolerance: the total's and each type's standard
    /// error relative to FLIP's mean, and for a histogram the L1 between a
    /// side's odd and even seeds scaled to the whole pool (1/√2). Over 1
    /// means add seeds.
    fn noise(gpu: &[Outcome], flip: &[Outcome]) -> [f64; 4] {
        let spread = |g: &[f64], f: &[f64]| (error(g).powi(2) + error(f).powi(2)).sqrt() / mean(f).max(1e-9);
        let total = spread(&totals(gpu), &totals(flip)) / (TOTAL_TOLERANCE / 2.0);
        let kinds = kinds_in_scope(flip).into_iter().map(|k| spread(&kind(gpu, k), &kind(flip, k)) / (KIND_TOLERANCE / 2.0)).fold(0.0, f64::max);
        let halves = |pick: fn(&Outcome) -> &[f64]| {
            [gpu, flip]
                .iter()
                .map(|side| {
                    let (even, odd): (Vec<_>, Vec<_>) = side.iter().enumerate().partition(|(i, _)| i % 2 == 0);
                    let strip = |v: Vec<(usize, &Outcome)>| v.into_iter().map(|(_, o)| o.clone()).collect::<Vec<_>>();
                    l1(&strip(even), &strip(odd), pick) / std::f64::consts::SQRT_2
                })
                .fold(0.0, f64::max)
        };
        [total, kinds, halves(|o| &o.space) / (SPACE_TOLERANCE / 2.0), halves(|o| &o.life) / (LIFE_TOLERANCE / 2.0)]
    }

    /// GPU FLIP's Dam Break at 64, frames 30, 60, 90 and 120: the particles,
    /// faces, distance, curvature and solid the scene hands its chain go to
    /// FLIP's emitter through the oracle and to the GPU atoms, and both take
    /// one lifecycle step with lifetime variance 0. Over 16 seeds a side,
    /// more while any measure's seed spread is over half its tolerance:
    /// total within 5%; each type within 10% where FLIP emitted at least 200
    /// of it over the seeds (the design's floor on the mean, taken on the
    /// pool so the gate covers more); the 8³ spatial histogram over the tank
    /// within L1 0.15; the 10-bin lifetime histogram within L1 0.1.
    #[test]
    fn whitewater_emitter_matches_flip() {
        let scene = WaterScene::dam_break(64);
        let grid = GridBox::of(scene);
        let tank_min = scene.min();
        let captured = capture(scene, grid);
        let solid = captured[0].solid.clone();
        let mut chain = Chain::new(grid, scene.particles() as usize, &solid);
        let mut failures = Vec::new();
        let mut population = Vec::new();
        println!("O2 frame seeds |     GPU    FLIP   total |     foam (GPU FLIP) |   bubble (GPU FLIP) |    spray (GPU FLIP) | space L1 | life L1 | noise / half tolerance");
        for c in &captured {
            let started = Instant::now();
            chain.load(c);
            let positions: Vec<[f32; 3]> = c.particles[..c.count as usize]
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .map(|p| [p.position_radius[0], p.position_radius[1], p.position_radius[2]])
                .collect();
            let curvature: Vec<f32> = c.curvature.iter().map(|k| k.value).collect();
            let (mut gpu, mut flip) = (Vec::new(), Vec::new());
            let mut seeds = 0;
            loop {
                for seed in seeds + 1..=seeds + SEEDS {
                    let spawns = chain.spawns(c.count, seed);
                    let mut ours = lifecycle(grid, c, &solid, seed);
                    ours.load(&spawns).expect("load");
                    ours.step(DT).expect("step");
                    ours.particles(&mut population).expect("population");
                    gpu.push(Outcome::of(&population, tank_min));
                    let mut theirs = lifecycle(grid, c, &solid, seed);
                    whitewater_oracle::emit(&mut theirs, &curvature, &positions, DT).expect("FLIP emits");
                    theirs.particles(&mut population).expect("population");
                    flip.push(Outcome::of(&population, tank_min));
                }
                seeds += SEEDS;
                let spread = noise(&gpu, &flip);
                if spread.iter().all(|&s| s <= 1.0) {
                    break;
                }
                if seeds >= SEED_LIMIT {
                    failures.push(format!("frame {}: still too noisy to judge at {seeds} seeds: {spread:.2?}", c.frame));
                    break;
                }
            }
            let relative = |a: f64, b: f64| if b > 0.0 { (a - b) / b } else if a > 0.0 { f64::INFINITY } else { 0.0 };
            let (g, f) = (mean(&totals(&gpu)), mean(&totals(&flip)));
            let mut row = format!("O2 {:5} {seeds:5} | {g:7.2} {f:7.2} {:+6.1}%", c.frame, 100.0 * relative(g, f));
            if relative(g, f).abs() > TOTAL_TOLERANCE {
                failures.push(format!("frame {}: total {g:.2} against FLIP's {f:.2}", c.frame));
            }
            let scope = kinds_in_scope(&flip);
            for (k, name) in ["foam", "bubble", "spray"].into_iter().enumerate() {
                let (a, b) = (mean(&kind(&gpu, k)), mean(&kind(&flip, k)));
                let gated = if scope.contains(&k) { " " } else { "*" };
                row += &format!(" | {:+6.1}%{gated}{a:6.2} {b:6.2}", 100.0 * relative(a, b));
                if scope.contains(&k) && relative(a, b).abs() > KIND_TOLERANCE {
                    failures.push(format!("frame {}: {name} {a:.2} against FLIP's {b:.2}", c.frame));
                }
            }
            let (space, life) = (l1(&gpu, &flip, |o| &o.space), l1(&gpu, &flip, |o| &o.life));
            row += &format!(" | {space:8.3} | {life:7.3} | {:.2?} | {:.0} s", noise(&gpu, &flip), started.elapsed().as_secs_f64());
            println!("{row}");
            if space > SPACE_TOLERANCE {
                failures.push(format!("frame {}: spatial L1 {space:.3}", c.frame));
            }
            if life > LIFE_TOLERANCE {
                failures.push(format!("frame {}: lifetime L1 {life:.3}", c.frame));
            }
        }
        println!("O2 * marks a type outside the per-type gate: FLIP emitted fewer than {KIND_FLOOR} over the seeds");
        assert!(failures.is_empty(), "the GPU emitter strays from FLIP's: {failures:#?}");
    }
}
