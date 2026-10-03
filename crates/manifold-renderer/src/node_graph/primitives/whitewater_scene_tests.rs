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

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuTextureFormat;
use serde_json::{Value, json};

use super::gpu_flip_preset::{WaterScene, render_def};
use super::gpu_flip_step::face_bytes;
use crate::gpu_encoder::GpuEncoder;
use crate::headless_readback::{encode_rgba8_png, readback_srgb_rgba8};
use crate::node_graph::depth_rule::DepthRule;
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::PrimitiveRegistry;
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use crate::render_target::RenderTarget;

const STEP_REPORTS: [&str; 6] = ["foam_count", "bubble_count", "spray_count", "emitted", "thinned", "pool_full"];

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
const COUNTS_PROBE: &str = "test.whitewater_counts_probe";

/// A liveness root that keeps the observed output bound. Scalar probes
/// shadow their input with `value` for the runtime's live parameter tap;
/// the count-buffer probe retains the boundary's completed tick reports.
struct Probe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    params: Vec<ParamDef>,
}

impl Probe {
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

    /// Keep the boundary's count output bound for late capture. The CPU
    /// reads its shared buffer only after the frame completes.
    fn whitewater_counts() -> Self {
        Self {
            type_id: EffectNodeType::new(COUNTS_PROBE),
            inputs: vec![NodePort { name: Cow::Borrowed("counts"), ty: PortType::Array(ArrayType::of_known::<u32>()), kind: PortKind::Input, required: true }],
            params: Vec::new(),
        }
    }
}

impl EffectNode for Probe {
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
/// at 64 is the shipped preset with its `node.whitewater_step`. The node's
/// reports are read from the boundary's captured counts after the frame;
/// the frame's particle count is probed as `count`.
pub(super) fn whitewater_render_def(scene: WaterScene) -> EffectGraphDef {
    let mut g = Appender::new(render_def(scene.with_faces()));
    let state = g.id("state");
    let counts = g.node("whitewater_reports", COUNTS_PROBE, json!({}));
    g.wire((state, "whitewater_counts"), counts, "counts");
    let frame = g.id("frame");
    g.probe("count", (frame, "count_b"));
    g.finish()
}

/// CPU-only regression: the probed scene must remain compilable after fusion.
#[test]
fn whitewater_scene_fuses_without_gpu() {
    use crate::node_graph::{EffectGraphDefExt, compile};
    use crate::node_graph::freeze::install::fuse_generator_view;

    let def = whitewater_render_def(WaterScene::dam_break(64));
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry.register(PROBE, || Box::new(Probe::new()));
    registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
    let report = crate::node_graph::fusion_report(&def, &registry);
    assert_eq!(report.regions.len(), 3, "the surface chain, Fill Pits and the display blend must remain fusable");
    let members: Vec<_> = report.nodes.iter().filter(|node| node.fused).map(|node| node.type_id.as_str()).collect();
    assert_eq!(members, [
        "node.smooth_lattice", "node.clamp_liquid_to_solids",
        "node.redistance_lattice", "node.offset_lattice",
        "node.interpolate_particle_frames", "node.push_out_of_solid",
    ]);
    let graph = def.clone().into_graph(&registry, &Default::default()).expect("authored scene loads");
    let plan = compile(&graph).expect("probes must not escape the authored tick region");
    let state = graph.nodes().find(|n| n.node_id.as_str() == "state").expect("the liquid boundary").id;
    assert!(plan.steps().iter().find(|s| s.node == state).unwrap().outputs.iter().any(|(port, _)| *port == "whitewater_counts"),
        "the counts must stay bound for late capture");
    let fused = fuse_generator_view(&def, &registry).expect("the whitewater scene must fuse");
    let graph = (*fused.def).clone().into_graph(&registry, &fused.mesh_rules).expect("fused scene loads");
    let plan = compile(&graph).expect("probes must not escape the fused tick region");
    let state = graph.nodes().find(|n| n.node_id.as_str() == "state").expect("the liquid boundary survives fusion").id;
    assert!(plan.steps().iter().find(|s| s.node == state).unwrap().outputs.iter().any(|(port, _)| *port == "whitewater_counts"),
        "fusion must keep the counts bound for late capture");
}

/// The vendored lifecycle replaces the preset's `node.whitewater_step` at the
/// same document id. The vendored lifecycle takes Capacity as a param, while
/// the current step takes a scalar port: the splice must adapt that interface
/// and retain the card binding to the lifecycle's real capacity control.
#[test]
fn vendored_whitewater_scene_loads_and_compiles_without_gpu() {
    use crate::node_graph::{EffectGraphDefExt, compile};
    use crate::node_graph::freeze::install::fuse_generator_view;

    let def = vendored_render_def(WaterScene::dam_break(16));
    let group = def.nodes.iter().find(|node| node.node_id.as_str() == "whitewater").expect("the vendored whitewater group");
    let body = group.group.as_ref().expect("whitewater is a group");
    assert!(!body.interface.inputs.iter().any(|input| input.name == "capacity"),
        "the lifecycle uses a param, not a silently unused capacity input");
    assert!(!def.wires.iter().any(|wire| wire.to_node == group.id && wire.to_port == "capacity"),
        "the step-only capacity wire must be removed by the fixture splice");
    let budget = def.preset_metadata.as_ref().unwrap().bindings.iter()
        .find(|binding| binding.id == "whitewater_capacity").expect("budget binding");
    assert_eq!(budget.target, manifold_core::effect_graph_def::BindingTarget::Node {
        node_id: "ww.lifecycle".into(), param: "capacity".into(),
    }, "the budget card must still drive the lifecycle");
    let capacity = body.interface.params.iter().find(|param| param.name == "capacity").expect("the Capacity group param");
    assert_eq!(capacity.target_handle, "Whitewater Lifecycle");
    assert_eq!(capacity.target_param, "capacity");

    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry.register(PROBE, || Box::new(Probe::new()));
    registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
    let graph = def.clone().into_graph(&registry, &Default::default()).expect("vendored whitewater scene loads");
    let lifecycle = graph.nodes().find(|node| node.node_id.as_str() == "ww.lifecycle").expect("the lifecycle survives flattening");
    assert_eq!(lifecycle.params.get("capacity").and_then(ParamValue::as_scalar), Some(100_000.0),
        "the group's Capacity param must route to the lifecycle");
    let plan = compile(&graph).expect("vendored whitewater scene compiles");
    assert!(plan.steps().iter().any(|step| step.node == lifecycle.id), "the lifecycle must remain in the compiled plan");
    let fused = fuse_generator_view(&def, &registry).expect("the vendored whitewater scene must fuse");
    let fused_graph = (*fused.def).clone().into_graph(&registry, &fused.mesh_rules).expect("fused vendored whitewater scene loads");
    compile(&fused_graph).expect("fused vendored whitewater scene compiles");
}

/// The preset's Whitewater group before `node.whitewater_step` replaced it:
/// the GPU emitter atoms feeding the vendored lifecycle (`ww.lifecycle`). Its
/// frame-based ports are adapted by `vendored_render_def`. Kept for
/// L5's side-by-side and O2, which reads the group's inner arrays.
const VENDORED_GROUP: &str = include_str!("../../../tests/fixtures/whitewater_vendored_group.json");

/// `whitewater_render_def` with the vendored group in place of the node, its
/// lifecycle reports probed by name.
fn vendored_render_def(scene: WaterScene) -> EffectGraphDef {
    let mut g = Appender::new(render_def(scene.with_faces()));
    let mut group: Value = serde_json::from_str(VENDORED_GROUP).expect("the vendored group parses");
    let id = g.id("whitewater");
    // The render ids move with the water def's node count; the group takes the node's.
    group["id"] = json!(id);
    let nodes = g.def["nodes"].as_array_mut().expect("nodes");
    *nodes.iter_mut().find(|n| n["nodeId"] == "whitewater").expect("the whitewater node") = group;
    // b1a1f5f65 moved whitewater_step into the tick region with pool state
    // and a distance lattice. The vendored lifecycle is still a post-frame
    // observer. Restore its original frame/surface inputs and direct render
    // outputs, rather than feeding it the step's incompatible tick interface.
    g.def["wires"].as_array_mut().expect("wires")
        .retain(|wire| wire["toNode"] != id && wire["fromNode"] != id);
    g.remove(&["whitewater_face_u", "whitewater_face_v", "whitewater_face_w"]);
    let frame = g.id("frame");
    for (source, input) in [("particles_b", "particles"), ("count_b", "count"), ("solid_b", "solid")] {
        g.wire((frame, source), id, input);
    }
    for port in ["grid_bounds", "grid_nodes_x", "grid_nodes_y", "grid_nodes_z",
        "face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"] {
        g.wire((frame, port), id, port);
    }
    let surface = g.id("surface");
    for port in ["level_set", "level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"] {
        g.wire((surface, port), id, port);
    }
    let domain = g.id("domain");
    for port in ["ticks", "epoch", "gravity_x", "gravity", "gravity_z"] {
        g.wire((domain, port), id, port);
    }
    g.wire((domain, "simulation_time"), id, "seed");
    for (kind, particles, count) in [("foam", "foam_particles", "foam_count"),
        ("bubble", "bubble_particles", "bubble_count"), ("spray", "spray_particles", "spray_count")] {
        let copies = g.id(&format!("{kind}_copies"));
        let object = g.id(&format!("{kind}_object"));
        g.def["wires"].as_array_mut().expect("wires")
            .retain(|wire| !(wire["toNode"] == copies && wire["toPort"] == "particles"));
        g.wire((id, particles), copies, "particles");
        g.wire((id, count), copies, "live_count");
        g.wire((id, count), object, "instance_count");
    }
    let target = json!({"kind": "node", "nodeId": "ww.lifecycle", "param": "capacity"});
    for binding in g.def["presetMetadata"]["bindings"].as_array_mut().expect("bindings") {
        if binding["id"] == "whitewater_capacity" {
            binding["target"] = target.clone();
        }
    }
    for report in LIFECYCLE_REPORTS {
        g.probe(report, (id, report));
    }
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
pub(super) struct Show {
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
    /// The cards' values, as the clip hands them to the runtime.
    cards: ParamManifest,
}

/// One frame's clocks and, when profiled, each whitewater label's own GPU ms.
pub(super) struct Frame {
    gpu_ms: f64,
    cpu_ms: f64,
    whitewater_ms: Vec<f64>,
    /// Dispatches the sampler couldn't time.
    untimed: usize,
}

impl Show {
    pub(super) fn new(def: EffectGraphDef, size: (u32, u32), frozen: bool, held: &[String]) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        registry.register(PROBE, || Box::new(Probe::new()));
        registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
        let (def, retarget) = match frozen.then(|| super::gpu_flip_preset::fused_as_rendered(&def, &registry)).flatten() {
            Some(view) => ((*view.def).clone(), view.node_retarget.clone()),
            None => (def, Default::default()),
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
            let whitewater = label.split('+').any(|m| m.starts_with("ww.") || m == "whitewater");
            step_label.push(whitewater.then(|| match labels.iter().position(|l| *l == label) {
                Some(i) => i,
                None => {
                    labels.push(label);
                    labels.len() - 1
                }
            }));
        }
        let mut show = Self {
            device,
            runtime,
            target,
            size,
            sampler,
            step_label,
            labels,
            frame_count: 0,
            trigger: 0,
            paused: false,
            cards: ParamManifest::default(),
        };
        show.hold(held);
        show
    }

    /// One frame 1/60 s on. Metal's autoreleased objects drain per frame, as
    /// the content thread drains them.
    pub(super) fn frame(&mut self, profile: bool) -> Frame {
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
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &self.cards);
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
    pub(super) fn restart(&mut self) {
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

    /// This frame's values at the named probes. Per-tick whitewater reports
    /// come from the liquid boundary, after `frame` has waited for the GPU.
    fn probes<const N: usize>(&self, labels: [&str; N]) -> [f32; N] {
        let live = self.runtime.live_node_params_watched();
        labels.map(|label| {
            let name = format!("probe.{label}");
            let values = live.iter().find(|(id, _)| id.as_str() == name).map(|(_, values)| values);
            let Some(values) = values else {
                let word = STEP_REPORTS.iter().position(|&report| report == label).unwrap_or_else(|| panic!("no {name}"));
                let state = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == "state").expect("the liquid boundary");
                let counts = state.node.provided_array_output("whitewater_counts").expect("the boundary captures whitewater reports");
                assert!(counts.size >= (STEP_REPORTS.len() * std::mem::size_of::<u32>()) as u64);
                let ptr = counts.mapped_ptr().expect("shared whitewater counts");
                // SAFETY: `frame` waits for GPU completion, and the boundary
                // owns at least six u32 count words, checked above.
                return unsafe { *ptr.cast::<u32>().add(word) } as f32;
            };
            let value = values.iter().find(|(param, _)| *param == "value").map(|&(_, v)| v).expect("the probe reads value");
            assert!(value.is_finite(), "{name} saw no value this frame");
            value
        })
    }

    fn readback(&self) -> Vec<u8> {
        objc2::rc::autoreleasepool(|_| readback_srgb_rgba8(&self.device, &self.target.texture, self.size.0, self.size.1))
    }

    pub(super) fn errors(&self) -> Vec<String> {
        self.runtime.errors().iter().map(|e| format!("{e:?}")).collect()
    }

    /// Hold the arrays the named nodes write on the next frames, for
    /// `dumped`; an empty list stops.
    pub(super) fn hold(&mut self, names: &[String]) {
        let held: Vec<manifold_core::NodeId> = names.iter().map(|name| manifold_core::NodeId::from(name.as_str())).collect();
        self.runtime.set_dump_arrays(None, &held);
    }

    /// The first `len` records the named held node wrote on `port` this frame.
    #[cfg(feature = "whitewater-oracle")]
    pub(super) fn dumped<T: bytemuck::Pod>(&self, name: &str, port: &str, len: usize) -> Vec<T> {
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

    /// Bytes of the storage the named node provides on `port`; none, 0.
    fn provided_bytes(&self, name: &str, port: &str) -> u64 {
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        node.node.provided_array_output(port).map_or(0, |buffer| buffer.size)
    }
}

/// Resolution is a live card (BUG-9an1 (resolution change), BUG-o65k (GPU
/// FLIP lattice wiring)): the shipped preset moves 64 → 32 → 100 under a
/// running clip. On the first frame at each size the state already holds that
/// lattice's face grid; within 1.5 s the fill has restarted at that size, the
/// step's faces are that lattice's and its water throws whitewater.
#[test]
fn gpu_flip_resolution_card_resizes_at_runtime() {
    let scene = WaterScene::dam_break(64);
    let def = whitewater_render_def(scene);
    let spec = def
        .preset_metadata
        .as_ref()
        .and_then(|cards| cards.params.iter().find(|card| card.id == "resolution"))
        .expect("the Resolution card")
        .clone();
    let mut show = Show::new(def, (320, 180), true, &[]);
    show.restart();
    let step = super::gpu_flip_preset::STEP_NODE;
    for n in [64u32, 32, 100] {
        let mut card = Param::bundled(spec.clone());
        card.value = n as f32;
        card.base = n as f32;
        show.cards = ParamManifest::from_params(vec![card]);
        show.frame(false);
        let faces = face_bytes([n; 3]);
        assert_eq!(show.provided_bytes("state", "faces"), faces, "Resolution {n}: the state's faces on its first frame");
        let mut last = [0.0; 6];
        let mut gpu_ms = Vec::new();
        for _ in 0..90 {
            gpu_ms.push(show.frame(false).gpu_ms);
            last = show.probes(STEP_REPORTS);
        }
        let [count] = show.probes(["count"]);
        println!("Resolution {n}: {count} particles, GPU p50 {:.2} ms; foam {} bubble {} spray {}", percentile(&gpu_ms, 0.5), last[0], last[1], last[2]);
        assert_eq!(show.provided_bytes(step, "faces"), faces, "Resolution {n}: the step's faces");
        assert_eq!(count as u64, WaterScene::dam_break(n as usize).particles(), "Resolution {n}: the fill");
        assert!(last[0] + last[1] + last[2] > 0.0, "Resolution {n}: no whitewater by 1.5 s: {last:?}");
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the resize ran with errors: {errors:#?}");
}

/// The shipped GPU FLIP Dam Break with its `node.whitewater_step`, as the app
/// renders it, 90 frames: no node refuses and foam is up by 1.5 s.
#[test]
fn gpu_flip_whitewater_emits() {
    let scene = WaterScene::dam_break(64);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    let mut last = [0.0; 6];
    for frame in 1..=90 {
        show.frame(false);
        last = show.probes(STEP_REPORTS);
        if frame % 15 == 0 {
            let [foam, bubble, spray, emitted, thinned, pool_full] = last;
            let [count] = show.probes(["count"]);
            println!(
                "frame {frame}: {count} particles; foam {foam} bubble {bubble} spray {spray}, emitted {emitted}, thinned {thinned}, pool full {pool_full}"
            );
        }
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
    assert!(last[0] > 0.0, "no foam by 1.5 s: {last:?}");
}

/// D11 at 64, live: the scene's whitewater is updated on the lifecycle's
/// thread (the update refuses any other, so a content-thread update fails
/// here), the population it produces reaches the outputs, and no tick drops
/// while the GPU keeps up. The content thread's and the worker's ms are
/// printed for the cost table, never asserted. Runs the vendored group, the
/// only whitewater with a lifecycle thread.
#[test]
fn whitewater_live_scene_updates_on_the_lifecycle_thread() {
    let scene = WaterScene::dam_break(64);
    let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut show = Show::new(vendored_render_def(scene), (320, 180), true, &[]);
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
    let playing = show.probes(STEP_REPORTS);
    assert!(playing[0] > 0.0, "no foam by 1 s: {playing:?}");
    show.paused = true;
    show.frame(false);
    let (held, image) = (show.probes(STEP_REPORTS), show.readback());
    for _ in 0..3 {
        show.frame(false);
    }
    let (still, still_image) = (show.probes(STEP_REPORTS), show.readback());
    let changed = image.chunks_exact(4).zip(still_image.chunks_exact(4)).filter(|(a, b)| a != b).count();
    println!("WHITEWATER pause: playing {playing:?}; paused {held:?} then {still:?}; {changed} pixels changed over 3 paused frames");
    // Counts are captured at each tick boundary, so even the first paused
    // frame must preserve the last completed playing tick.
    assert_eq!(held, playing, "the first paused frame changed whitewater counts");
    assert_eq!(still[..6], held[..6], "paused frames moved the whitewater");
    assert_eq!(changed, 0, "paused frames changed the picture");
    show.paused = false;
    for _ in 0..15 {
        show.frame(false);
    }
    let resumed = show.probes(STEP_REPORTS);
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
/// kernel, and FLIP's whitewater cost as its
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
    let scene = WaterScene::race_dam_break(64);
    let gpu_flip_show = || {
        let mut show = Show::new(whitewater_render_def(scene), DEMO_SIZE, true, &[]);
        show.restart();
        show
    };

    let mut flip = Show::new(flip_def(true), DEMO_SIZE, false, &[]);
    let (flip_clip, flip_values) = record(&mut flip, "flip", &dir, FLIP_PROBES);
    drop(flip);
    let mut gpu_flip = gpu_flip_show();
    let (gpu_flip_clip, gpu_flip_values) = record(&mut gpu_flip, "gpu_flip", &dir, STEP_REPORTS);
    let errors = gpu_flip.errors();
    drop(gpu_flip);
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");

    let (flip_on_ms, flip_off_ms) = (flip_simulation_ms(true), flip_simulation_ms(false));
    // Live, as the show runs.
    let live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut gpu_flip = gpu_flip_show();
    let gpu_flip_frames: Vec<Frame> = (0..DEMO_FRAMES).map(|_| gpu_flip.frame(true)).collect();
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
        "frame,flip_foam,flip_bubble,flip_spray,gpu_flip_foam,gpu_flip_bubble,gpu_flip_spray,gpu_flip_emitted,flip_simulation_ms,flip_off_simulation_ms,gpu_flip_whitewater_gpu_ms\n",
    );
    for frame in 0..DEMO_FRAMES {
        let (f, s) = (flip_values[frame], gpu_flip_values[frame]);
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.3},{:.3},{:.3}\n",
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
    let frame_gpu: Vec<f64> = gpu_flip_frames[settled.clone()].iter().map(|f| f.gpu_ms).collect();
    let frame_cpu: Vec<f64> = gpu_flip_frames[settled.clone()].iter().map(|f| f.cpu_ms).collect();
    let (on, off) = (&flip_on_ms[settled.clone()], &flip_off_ms[settled.clone()]);
    let delta: Vec<f64> = on.iter().zip(off).map(|(a, b)| a - b).collect();
    let gpu_p95 = percentile(&total, 0.95);
    let row = |name: &str, values: &[f64]| println!("WHITEWATER   {name:<60} {:7.3} {:7.3}", percentile(values, 0.5), percentile(values, 0.95));
    row("GPU whitewater total (target p95 <= 2)", &total);
    row("GPU FLIP whole frame GPU", &frame_gpu);
    row("GPU FLIP whole frame CPU", &frame_cpu);
    row("FLIP simulation_ms, whitewater on", on);
    row("FLIP simulation_ms, whitewater off", off);
    row("FLIP whitewater cost (on minus off, frame by frame)", &delta);
    println!("WHITEWATER   untimed dispatches on the worst frame: {untimed}");
    println!(
        "WHITEWATER verdict: GPU {} the 2 ms p95 target",
        if gpu_p95 <= 2.0 { "meets" } else { "misses" },
    );
    for frame in DEMO_STILLS {
        let (f, s) = (flip_values[frame - 1], gpu_flip_values[frame - 1]);
        println!("WHITEWATER frame {frame}: FLIP foam {} bubble {} spray {}; GPU FLIP foam {} bubble {} spray {}", f[0], f[1], f[2], s[0], s[1], s[2]);
    }
    println!("WHITEWATER wrote {} and {}", clip.display(), dir.join("counts.png").display());
}

const EMISSION_FRAMES: usize = 150;

/// O2's whole-scene companion: the Dam Break at 64 for its first 150 frames,
/// the shipped GPU FLIP preset beside the FLIP engine running its own water
/// and whitewater (read-only, as the race clips run it). Per frame: GPU FLIP's
/// emission (the step in `emitted`) and population, and the engine's diffuse
/// population. The engine publishes no emission count, and counting it would
/// mean editing vendored code, so its side is the population. Writes
/// `emission_150.csv` to the temp directory and prints the totals; asserts
/// that both emit, that GPU FLIP never thins or fills its pool, and that its
/// population is what it emitted less what died (never more).
#[test]
fn whitewater_emission_against_engine_150() {
    let mut flip = Show::new(flip_def(true), (320, 180), false, &[]);
    let flip_rows: Vec<[f32; 3]> = (0..EMISSION_FRAMES)
        .map(|_| {
            flip.frame(false);
            let [foam, bubble, spray, _] = flip.probes(FLIP_PROBES);
            [foam, bubble, spray]
        })
        .collect();
    let errors = flip.errors();
    drop(flip);
    assert!(errors.is_empty(), "the engine ran with errors: {errors:#?}");

    let mut show = Show::new(whitewater_render_def(WaterScene::race_dam_break(64)), (320, 180), true, &[]);
    show.restart();
    let gpu_rows: Vec<[f32; 6]> = (0..EMISSION_FRAMES)
        .map(|_| {
            show.frame(false);
            show.probes(STEP_REPORTS)
        })
        .collect();
    let errors = show.errors();
    drop(show);
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");

    let mut csv = String::from("frame,gpu_flip_emitted_this_frame,gpu_flip_population,engine_population,engine_foam,engine_bubble,engine_spray\n");
    let mut previous = 0.0;
    let (mut gpu_emitted, mut gpu_pop_sum, mut engine_pop_sum) = (0.0f64, 0.0f64, 0.0f64);
    for (frame, (g, e)) in gpu_rows.iter().zip(&flip_rows).enumerate() {
        let emitted = g[3] - previous;
        previous = g[3];
        assert!(emitted >= 0.0, "frame {}: emitted ran backwards", frame + 1);
        let population = g[0] + g[1] + g[2];
        assert!(population <= g[3], "frame {}: population {population} above all emitted {}", frame + 1, g[3]);
        let engine = e[0] + e[1] + e[2];
        gpu_emitted += f64::from(emitted);
        gpu_pop_sum += f64::from(population);
        engine_pop_sum += f64::from(engine);
        csv.push_str(&format!("{},{emitted},{population},{engine},{},{},{}\n", frame + 1, e[0], e[1], e[2]));
    }
    let path = std::env::temp_dir().join("emission_150.csv");
    std::fs::write(&path, csv).expect("emission csv");
    let last = gpu_rows[EMISSION_FRAMES - 1];
    let engine_last = flip_rows[EMISSION_FRAMES - 1];
    println!(
        "WHITEWATER 150 frames: GPU FLIP emitted {gpu_emitted}, mean population {:.0}, last {:?}; engine mean population {:.0}, last {engine_last:?}; population ratio {:.3}; csv {}",
        gpu_pop_sum / EMISSION_FRAMES as f64,
        &last[..3],
        engine_pop_sum / EMISSION_FRAMES as f64,
        gpu_pop_sum / engine_pop_sum.max(1.0),
        path.display()
    );
    assert!(gpu_emitted > 0.0, "GPU FLIP emitted nothing in 150 frames");
    assert!(engine_pop_sum > 0.0, "the engine made no whitewater in 150 frames");
    assert_eq!(last[4], 0.0, "GPU FLIP thinned spawns");
    assert_eq!(last[5], 0.0, "the pool filled at the preset's capacity");
}

/// One whitewater's foam, bubble and spray counts per frame over
/// `EMISSION_FRAMES`, with its cumulative emission.
fn count_rows<const N: usize>(def: EffectGraphDef, reports: [&str; N], name: &str) -> Vec<[f32; 4]> {
    let mut show = Show::new(def, (320, 180), true, &[]);
    show.restart();
    let rows = (0..EMISSION_FRAMES)
        .map(|_| {
            show.frame(false);
            let r = show.probes(reports);
            [r[0], r[1], r[2], r[3]]
        })
        .collect();
    let errors = show.errors();
    assert!(errors.is_empty(), "{name} ran with errors: {errors:#?}");
    rows
}

/// L5: the Dam Break at 64 for 150 frames, the same GPU FLIP water feeding
/// `node.whitewater_step` and, read-only, the vendored lifecycle behind the
/// GPU emitter group it replaced. Per frame and type, both counts. Writes
/// `whitewater_step_vs_vendored_150.csv` to the temp directory and prints
/// both sides every 30 frames with the per-type totals over the run. The two
/// are compared, not held equal: the node's lifecycle is a port, so its float
/// path is not the engine's.
#[test]
fn whitewater_step_against_vendored_lifecycle_150() {
    let scene = WaterScene::dam_break(64);
    let step = count_rows(whitewater_render_def(scene), STEP_REPORTS, "whitewater_step");
    let vendored = count_rows(vendored_render_def(scene), LIFECYCLE_REPORTS, "the vendored lifecycle");

    let mut csv = String::from("frame,step_foam,step_bubble,step_spray,step_emitted,vendored_foam,vendored_bubble,vendored_spray,vendored_emitted\n");
    let (mut step_sum, mut vendored_sum) = ([0.0f64; 3], [0.0f64; 3]);
    for (frame, (s, v)) in step.iter().zip(&vendored).enumerate() {
        for kind in 0..3 {
            step_sum[kind] += f64::from(s[kind]);
            vendored_sum[kind] += f64::from(v[kind]);
        }
        csv.push_str(&format!("{},{},{},{},{},{},{},{},{}\n", frame + 1, s[0], s[1], s[2], s[3], v[0], v[1], v[2], v[3]));
        if (frame + 1) % 30 == 0 {
            println!(
                "L5 frame {}: step foam {} bubble {} spray {} emitted {}; vendored foam {} bubble {} spray {} emitted {}",
                frame + 1, s[0], s[1], s[2], s[3], v[0], v[1], v[2], v[3]
            );
        }
    }
    let path = std::env::temp_dir().join("whitewater_step_vs_vendored_150.csv");
    std::fs::write(&path, csv).expect("parity csv");
    let ratio: Vec<String> =
        (0..3).map(|k| format!("{:.3}", step_sum[k] / vendored_sum[k].max(1.0))).collect();
    println!(
        "L5 150 frames, summed population foam/bubble/spray: step {step_sum:?}, vendored {vendored_sum:?}, step/vendored [{}]; csv {}",
        ratio.join(", "),
        path.display()
    );
    assert!(step_sum.iter().sum::<f64>() > 0.0, "whitewater_step made no whitewater in 150 frames");
    assert!(vendored_sum.iter().sum::<f64>() > 0.0, "the vendored lifecycle made no whitewater in 150 frames");
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

    fn cells(grid: GridBox) -> u32 {
        grid.nodes as u32 - 1
    }

    fn capture(scene: WaterScene, grid: GridBox) -> Vec<Captured> {
        let held: Vec<String> = ["frame", "ww.distance", "ww.extend2", "ww.cells"].map(String::from).to_vec();
        let mut show = Show::new(vendored_render_def(scene), (320, 180), false, &held);
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
