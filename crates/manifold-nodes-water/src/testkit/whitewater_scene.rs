use std::borrow::Cow;
use std::time::Instant;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};
use manifold_core::params::ParamManifest;
use manifold_gpu::GpuTextureFormat;
use serde_json::{Value, json};

use crate::primitives::gpu_flip_preset::{WaterScene, render_def};
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::testkit::gpu::readback_srgb_rgba8;
use manifold_node_engine::scene::depth_rule::DepthRule;
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;

pub const STEP_REPORTS: [&str; 6] = ["foam_count", "bubble_count", "spray_count", "emitted", "thinned", "pool_full"];
pub fn float(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

type Port<'a> = (u64, &'a str);

fn renumber_scope_ids(value: &mut Value, next: &mut u64) {
    let mut remap = std::collections::BTreeMap::new();
    {
        let Some(nodes) = value["nodes"].as_array_mut() else { return };
        for node in nodes.iter_mut() {
            let old = node["id"].as_u64().expect("numeric id");
            let fresh = *next;
            *next += 1;
            remap.insert(old, fresh);
            node["id"] = json!(fresh);
        }
        for node in nodes.iter_mut() {
            if node["group"].is_object() {
                renumber_scope_ids(&mut node["group"], next);
            }
        }
    }
    if let Some(wires) = value["wires"].as_array_mut() {
        for wire in wires {
            let from = wire["fromNode"].as_u64().expect("numeric from id");
            let to = wire["toNode"].as_u64().expect("numeric to id");
            wire["fromNode"] = json!(remap[&from]);
            wire["toNode"] = json!(remap[&to]);
        }
    }
}

use crate::liquid::conformance::json_node_mut;

pub fn node_scope_wires<'a>(
    nodes: &'a [EffectGraphNode],
    wires: &'a [manifold_core::effect_graph_def::EffectGraphWire],
    node_id: &str,
) -> Option<&'a [manifold_core::effect_graph_def::EffectGraphWire]> {
    for node in nodes {
        if node.node_id.as_str() == node_id {
            return Some(wires);
        }
        if let Some(group) = node.group.as_deref()
            && let Some(found) = node_scope_wires(&group.nodes, &group.wires, node_id)
        {
            return Some(found);
        }
    }
    None
}

pub const PROBE: &str = "test.scalar_probe";
pub const COUNTS_PROBE: &str = "test.whitewater_counts_probe";

/// A liveness root that keeps the observed output bound. Scalar probes
/// shadow their input with `value` for the runtime's live parameter tap;
/// the count-buffer probe retains the boundary's completed tick reports.
pub struct Probe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    params: Vec<ParamDef>,
}

impl Default for Probe {
    fn default() -> Self { Self::new() }
}

impl Probe {
    pub fn new() -> Self {
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
    pub fn whitewater_counts() -> Self {
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
pub struct Appender {
    def: Value,
    next: u64,
}



impl Appender {
    pub fn new(def: EffectGraphDef) -> Self {
        Self::from_value(serde_json::to_value(def).expect("def serialises"))
    }

    pub fn from_value(def: Value) -> Self {
        let mut first = 0;
        let mut def = def;
        renumber_scope_ids(&mut def, &mut first);
        Self { def, next: first }
    }

    fn named(&mut self, name: &str) -> &Value {
        json_node_mut(&mut self.def, name).unwrap_or_else(|| panic!("no node {name}"))
    }

    pub fn id(&mut self, name: &str) -> u64 {
        self.named(name)["id"].as_u64().expect("numeric id")
    }

    fn scope_path(&self, id: u64) -> Option<Vec<usize>> {
        fn find(value: &Value, id: u64, path: &mut Vec<usize>) -> bool {
            let Some(nodes) = value["nodes"].as_array() else { return false };
            for (index, node) in nodes.iter().enumerate() {
                if node["id"].as_u64() == Some(id) {
                    return true;
                }
                if node["group"].is_object() {
                    path.push(index);
                    if find(&node["group"], id, path) {
                        return true;
                    }
                    path.pop();
                }
            }
            false
        }
        let mut path = Vec::new();
        find(&self.def, id, &mut path).then_some(path)
    }

    fn scope_mut(&mut self, path: &[usize]) -> &mut Value {
        let mut scope = &mut self.def;
        for &index in path {
            scope = &mut scope["nodes"][index]["group"];
        }
        scope
    }

    fn scope_for_pair(&self, from: u64, to: u64) -> Vec<usize> {
        let from = self.scope_path(from).unwrap_or_else(|| panic!("no node id {from}"));
        let to = self.scope_path(to).unwrap_or_else(|| panic!("no node id {to}"));
        assert_eq!(from, to, "fixture wire crosses a group boundary without an interface pin");
        from
    }

    pub fn node_in_scope(&mut self, name: &str, type_id: &str, params: Value, scope_node: u64) -> u64 {
        let id = self.next;
        self.next += 1;
        let path = self.scope_path(scope_node).unwrap_or_else(|| panic!("no node id {scope_node}"));
        self.scope_mut(&path)["nodes"].as_array_mut().expect("nodes").push(json!({
            "id": id,
            "nodeId": name,
            "typeId": type_id,
            "params": params
        }));
        id
    }

    pub fn replace(&mut self, name: &str, mut replacement: Value) -> u64 {
        fn replace_in(value: &mut Value, name: &str, replacement: &mut Value) -> Option<u64> {
            let nodes = value["nodes"].as_array_mut()?;
            for node in nodes.iter_mut() {
                if node["nodeId"] == name {
                    let id = node["id"].as_u64().expect("numeric id");
                    replacement["id"] = json!(id);
                    *node = replacement.take();
                    return Some(id);
                }
                if node["group"].is_object()
                    && let Some(id) = replace_in(&mut node["group"], name, replacement)
                {
                    return Some(id);
                }
            }
            None
        }
        renumber_scope_ids(&mut replacement["group"], &mut self.next);
        replace_in(&mut self.def, name, &mut replacement).unwrap_or_else(|| panic!("no node {name}"))
    }

    pub fn wire(&mut self, from: Port<'_>, to: u64, port: &str) {
        let wire = json!({"fromNode": from.0, "fromPort": from.1, "toNode": to, "toPort": port});
        let path = self.scope_for_pair(from.0, to);
        self.scope_mut(&path)["wires"].as_array_mut().expect("wires").push(wire);
    }

    pub fn retain_wires<F>(&mut self, node: u64, mut keep: F)
    where
        F: FnMut(&Value) -> bool,
    {
        let path = self.scope_path(node).unwrap_or_else(|| panic!("no node id {node}"));
        self.scope_mut(&path)["wires"].as_array_mut().expect("wires").retain(|wire| keep(wire));
    }

    /// A scalar read after the frame as `probe.<label>`.
    pub fn probe(&mut self, label: &str, from: Port<'_>) {
        let id = self.next;
        self.next += 1;
        let path = self.scope_path(from.0).unwrap_or_else(|| panic!("no node id {}", from.0));
        self.scope_mut(&path)["nodes"].as_array_mut().expect("nodes").push(json!({
            "id": id,
            "nodeId": format!("probe.{label}"),
            "typeId": PROBE,
            "params": {}
        }));
        self.wire(from, id, "value");
    }

    /// Drops the named nodes and every wire touching them.
    pub fn remove(&mut self, names: &[&str]) {
        fn remove_in(value: &mut Value, names: &[&str]) {
            let Some(nodes) = value["nodes"].as_array() else { return };
            let ids: Vec<u64> = nodes.iter().filter(|node| names.iter().any(|name| node["nodeId"] == *name))
                .filter_map(|node| node["id"].as_u64()).collect();
            let gone = |id: &Value| id.as_u64().is_some_and(|id| ids.contains(&id));
            value["nodes"].as_array_mut().expect("nodes").retain(|node| !gone(&node["id"]));
            value["wires"].as_array_mut().expect("wires").retain(|wire| !gone(&wire["fromNode"]) && !gone(&wire["toNode"]));
            for node in value["nodes"].as_array_mut().expect("nodes") {
                if node["group"].is_object() {
                    remove_in(&mut node["group"], names);
                }
            }
        }
        remove_in(&mut self.def, names);
    }

    pub fn finish(self) -> EffectGraphDef {
        serde_json::from_value(self.def).expect("def with whitewater")
    }
}

/// `render_def` of `scene`, which for the Dam Break
/// at 64 is the shipped preset with its `node.whitewater_step`. The node's
/// reports are read from the boundary's captured counts after the frame;
/// the frame's particle count is probed as `count`.
pub fn whitewater_render_def(scene: WaterScene) -> EffectGraphDef {
    with_whitewater_reports(render_def(scene))
}

pub fn with_whitewater_reports(def: EffectGraphDef) -> EffectGraphDef {
    let mut g = Appender::new(def);
    let state = g.id("state");
    let counts = g.node_in_scope("whitewater_reports", COUNTS_PROBE, json!({}), state);
    g.wire((state, "whitewater_counts"), counts, "counts");
    let frame = g.id("frame");
    g.probe("count", (frame, "count_b"));
    g.finish()
}

/// `def` with the liquid domain's clock probed under its own port names
/// (`ticks`, `epoch`, `simulation_time`, `dropped_seconds`), so a proof can
/// see how many ticks each frame ran and that the clock accepted them.
pub fn with_tick_probe(def: EffectGraphDef) -> EffectGraphDef {
    let mut g = Appender::new(def);
    let domain = g.id("domain");
    for port in ["ticks", "epoch", "simulation_time", "dropped_seconds"] {
        g.probe(port, (domain, port));
    }
    g.finish()
}

/// One preset on the app's generator path, frame by frame at 60 fps.
pub struct Show {
    device: manifold_gpu::testkit::TestDevice,
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
    /// A proof injects a node error this frame; its refusal is expected.
    expect_node_error: bool,
    /// The last frame.s status, for proofs that expect a refusal.
    last_status: String,
}

/// One frame's clocks and, when profiled, each whitewater label's own GPU ms.
pub struct Frame {
    pub gpu_ms: f64,
    pub cpu_ms: f64,
    pub whitewater_ms: Vec<f64>,
    /// Dispatches the sampler couldn't time.
    pub untimed: usize,
}

impl Show {
    pub fn new(def: EffectGraphDef, size: (u32, u32), frozen: bool, held: &[String]) -> Self {
        Self::new_with_emitter_oracle(def, size, frozen, held, None)
    }

    pub fn new_with_emitter_oracle(def: EffectGraphDef, size: (u32, u32), frozen: bool, held: &[String], reference: Option<bool>) -> Self {
        let mut registry = PrimitiveRegistry::with_builtin();
        if let Some(reference) = reference {
            registry.register("node.whitewater_step", if reference {
                crate::primitives::whitewater_step::reference_proof_node
            } else {
                crate::primitives::whitewater_step::fused_proof_node
            });
        }
        register_substep_test_nodes(&mut registry);
        registry.register(PROBE, || Box::new(Probe::new()));
        registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
        let (def, retarget) = match frozen.then(|| crate::primitives::gpu_flip_preset::testkit::fused_as_rendered(&def, &registry)).flatten() {
            Some(view) => ((*view.def).clone(), view.node_retarget.clone()),
            None => (def, Default::default()),
        };
        let device = manifold_gpu::testkit::test_device();
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
            expect_node_error: false,
            last_status: String::new(),
        };
        show.hold(held);
        show
    }

    /// One frame 1/60 s on. Metal's autoreleased objects drain per frame, as
    /// the content thread drains them.
    pub fn frame(&mut self, profile: bool) -> Frame {
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
        let status = {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &self.cards);
            gpu.frame_status()
        };
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let result = enc.commit_and_wait_profiled(&self.device);
        assert_eq!(result.failed_command_buffers, 0, "frame {} failed on the GPU", self.frame_count);
        // A failed frame is not the one the graph describes: a refusing node
        // drew a fallback, and every probe past it reads nothing computed.
        assert!(self.expect_node_error || !matches!(status, FrameRenderStatus::Failed(_)), "frame {} failed: {status:?}", self.frame_count);
        self.last_status = format!("{status:?}");
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
    /// The simulation step every following frame runs under.
    pub fn set_sim_step(&mut self, step: crate::physics::SimStep) {
        self.runtime.set_sim_step(step);
    }

    pub fn restart(&mut self) {
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
    pub fn probes<const N: usize>(&self, labels: [&str; N]) -> [f32; N] {
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

    pub fn readback(&self) -> Vec<u8> {
        objc2::rc::autoreleasepool(|_| readback_srgb_rgba8(&self.device, &self.target.texture, self.size.0, self.size.1))
    }

    pub fn errors(&self) -> Vec<String> {
        self.runtime.errors().iter().map(|e| format!("{e:?}")).collect()
    }

    /// Hold the arrays the named nodes write on the next frames, for
    /// `dumped`; an empty list stops.
    fn hold(&mut self, names: &[String]) {
        let held: Vec<manifold_core::NodeId> = names.iter().map(|name| manifold_core::NodeId::from(name.as_str())).collect();
        self.runtime.set_dump_arrays(None, &held);
    }

    /// The runtime still warming up, as `restart` leaves it when its frame
    /// bound ran out.
    pub(super) fn warmup_pending(&self) -> bool {
        self.runtime.warmup_pending()
    }

    /// Every byte of the storage the named node provides on `port`, the
    /// whole buffer, read after the frame completed. The array dump never
    /// holds a tick region's body, so per-tick results are read from the
    /// boundary's captures.
    pub fn provided_all_bytes(&self, name: &str, port: &str) -> Vec<u8> {
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        let buffer = node.node.provided_array_output(port).unwrap_or_else(|| panic!("{name} provides no {port}"));
        let bytes = buffer.size;
        // The storage may be GPU-private: copy it to shared storage first.
        let staged = self.device.create_buffer_shared(bytes.max(4));
        let mut encoder = self.device.create_encoder("whitewater-scene provided readback");
        encoder.copy_buffer_to_buffer(buffer, &staged, bytes);
        encoder.commit_and_wait_completed();
        let ptr = staged.mapped_ptr().expect("shared readback");
        // SAFETY: shared storage of `bytes`, the copy completed above.
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>().cast_const(), bytes as usize) }.to_vec()
    }

    /// The first `len` records the named held node wrote on `port` this frame.
    #[cfg(feature = "whitewater-oracle")]
    pub fn dumped<T: bytemuck::Pod>(&self, name: &str, port: &str, len: usize) -> Vec<T> {
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

    /// The first `len` records of the storage the named node provides on
    /// `port`, read after the frame completed.
    #[cfg(feature = "whitewater-oracle")]
    pub fn provided<T: bytemuck::Pod>(&self, name: &str, port: &str, len: usize) -> Vec<T> {
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        let buffer = node.node.provided_array_output(port).unwrap_or_else(|| panic!("{name} provides no {port}"));
        assert!(buffer.size as usize >= len * std::mem::size_of::<T>(), "{name}.{port} is shorter than {len} records");
        // The storage may be GPU-private: copy it to shared storage first.
        let bytes = (len * std::mem::size_of::<T>()) as u64;
        let staged = self.device.create_buffer_shared(bytes.max(4));
        let mut encoder = self.device.create_encoder("whitewater-scene provided readback");
        encoder.copy_buffer_to_buffer(buffer, &staged, bytes);
        encoder.commit_and_wait_completed();
        let ptr = staged.mapped_ptr().expect("shared readback");
        // SAFETY: shared storage of `bytes`, the copy completed above.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), len) }.to_vec()
    }

    /// Bytes of the storage the named node provides on `port`; none, 0.
    pub fn provided_bytes(&self, name: &str, port: &str) -> u64 {
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        node.node.provided_array_output(port).map_or(0, |buffer| buffer.size)
    }

    /// Live particles (radius above 0) in the storage the named node provides
    /// on `port`.
    pub fn provided_live(&self, name: &str, port: &str) -> u64 {
        use manifold_node_engine::particles::FluidParticle;
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        let buffer = node.node.provided_array_output(port).unwrap_or_else(|| panic!("{name} provides no {port}"));
        let ptr = buffer.mapped_ptr().expect("shared particle storage");
        let len = buffer.size as usize / std::mem::size_of::<FluidParticle>();
        // SAFETY: `frame` waits for GPU completion, and the buffer holds `len` records.
        let particles = unsafe { std::slice::from_raw_parts(ptr.cast::<FluidParticle>(), len) };
        particles.iter().filter(|p| p.position_radius[3] > 0.0).count() as u64
    }
}

impl Show {
    /// The bytes of the storage the named node provides on `port`.
    pub fn provided_copy(&self, name: &str, port: &str) -> Vec<u8> {
        let node = self.runtime.graph.nodes().find(|n| n.node_id.as_str() == name).unwrap_or_else(|| panic!("no node {name}"));
        let buffer = node.node.provided_array_output(port).unwrap_or_else(|| panic!("{name} provides no {port}"));
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: `frame` waited for the GPU; the buffer holds `size` bytes.
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), buffer.size as usize) }.to_vec()
    }
}

impl Show {
    pub fn runtime(&self) -> &PresetRuntime { &self.runtime }
    pub fn labels(&self) -> &[String] { &self.labels }
    pub fn set_paused(&mut self, paused: bool) { self.paused = paused; }
    pub fn set_cards(&mut self, cards: ParamManifest) { self.cards = cards; }
    pub fn expect_node_error(&mut self, expected: bool) { self.expect_node_error = expected; }
    pub fn last_status(&self) -> &str { &self.last_status }
}
impl Appender {
    pub fn retarget_binding(&mut self, id: &str, target: Value) {
        for binding in self.def["presetMetadata"]["bindings"].as_array_mut().expect("bindings") {
            if binding["id"] == id { binding["target"] = target.clone(); }
        }
    }
    pub fn set_card_default(&mut self, list: &str, id: &str, value: Value) {
        for p in self.def["presetMetadata"][list].as_array_mut().expect("card list") {
            if p["id"] == id { p["defaultValue"] = value.clone(); }
        }
    }
    pub fn set_node_param(&mut self, id: &str, name: &str, value: Value) {
        self.def["nodes"].as_array_mut().expect("nodes").iter_mut().find(|n| n["nodeId"] == id).expect("engine")["params"][name] = value;
    }
}
