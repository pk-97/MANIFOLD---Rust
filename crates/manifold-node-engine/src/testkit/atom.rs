//! GPU value proofs for the vector atoms beside the GPU FLIP water
//! (node.dot_products, node.divide_by_value) against CPU f64 references,
//! the shared fixtures the GPU FLIP tests draw on, and [`Chain`], the
//! fused-vs-unfused rig. Every atom's run() refuses arrays shorter than its
//! lattice before dispatch.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};
use serde_json::json;

use crate::testkit::liquid_surface::{Harness, read};
use crate::gpu::gpu_encoder::GpuEncoder;
use crate::exec::backend::Backend;
use crate::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use crate::primitive::Primitive;
use crate::testkit::substep_nodes::register_substep_test_nodes;
use crate::{persistence::EffectGraphDefExt, exec::execution::Executor, exec::metal_backend::MetalBackend, persistence::PrimitiveRegistry, state_store::StateStore, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};

pub fn random_values(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        })
        .collect()
}

/// About seven cells in ten are water.
pub fn random_water(cells: usize, seed: u64) -> Vec<f32> {
    random_values(cells, seed).iter().map(|&v| f32::from(u8::from(v > -0.2))).collect()
}

/// One atom's `run()` with several array ports, into an open encoder.
fn step_ports<P: Primitive>(
    prim: &mut P,
    gpu: &mut GpuEncoder<'_>,
    backend: &dyn Backend,
    errors: &mut Vec<String>,
    inputs: &[(&'static str, Slot)],
    outputs: &[(&'static str, Slot)],
    step_params: &ParamValues,
) {
    let generations = [0_u64; 64];
    let (mut scalars, mut camera, mut light, mut material, mut transform) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut atmosphere, mut render_mode, mut object) = (Vec::new(), Vec::new(), Vec::new());
    let node_inputs = NodeInputs::new(inputs, backend, &generations);
    let node_outputs = NodeOutputs::new(
        outputs,
        backend,
        &mut scalars,
        &mut camera,
        &mut light,
        &mut material,
        &mut transform,
        &mut atmosphere,
        &mut render_mode,
        &mut object,
    );
    let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
    let mut ctx = EffectNodeContext::new(time, step_params, node_inputs, node_outputs, Some(gpu)).with_errors(errors);
    Primitive::run(prim, &mut ctx);
}

/// Run one atom on fresh arrays and read its output back.
pub fn run_atom<P: Primitive>(prim: &mut P, inputs: &[(&'static str, &[f32])], output_len: usize, step_params: &ParamValues) -> Vec<f32> {
    let mut harness = Harness::new();
    let slots: Vec<(&'static str, (Slot, GpuBuffer))> =
        inputs.iter().map(|&(name, values)| (name, harness.array(values, values.len().max(1)))).collect();
    let output = harness.array::<f32>(&[], output_len);
    let ports: Vec<(&'static str, Slot)> = slots.iter().map(|(name, (slot, _))| (*name, *slot)).collect();
    let mut errors = Vec::new();
    let mut native = harness.device.create_encoder("gpu flip atom");
    {
        let mut gpu = GpuEncoder::new(&mut native, &harness.device);
        let backend: &dyn Backend = &harness.backend;
        step_ports(prim, &mut gpu, backend, &mut errors, &ports, &[("out", output.0)], step_params);
    }
    native.commit_and_wait_completed();
    assert!(errors.is_empty(), "{errors:?}");
    read(&output.1, output_len)
}

pub fn assert_close(actual: &[f32], expected: &[f64], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: length");
    let scale = expected.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((f64::from(*a) - e).abs() <= 1e-5 * scale, "{what}[{i}]: {a} vs {e}");
    }
}

/// Floats per face-grid record: velocity, then open fractions.
pub const FACE_FLOATS: usize = 8;

pub fn face_grid_len(n: [usize; 3]) -> usize {
    n.iter().map(|v| v + 1).product::<usize>() * FACE_FLOATS
}





// ── Fused vs unfused ───────────────────────────────────────────────────────

/// A small graph of test sources, atoms and one sink, run once fused and
/// once unfused; each run's sink input is read back.
pub struct Chain {
    nodes: Vec<serde_json::Value>,
    wires: Vec<serde_json::Value>,
    sources: Vec<(&'static str, Vec<f32>)>,
}

impl Default for Chain {
    fn default() -> Self { Self::new() }
}

impl Chain {
    pub fn new() -> Self {
        Self { nodes: Vec::new(), wires: Vec::new(), sources: Vec::new() }
    }

    pub fn node(&mut self, name: &str, type_id: &str, params: serde_json::Value) -> usize {
        let id = self.nodes.len();
        self.nodes.push(json!({"id": id, "typeId": type_id, "nodeId": name, "params": params}));
        id
    }

    pub fn source(&mut self, name: &'static str, values: Vec<f32>) -> usize {
        let id = self.node(name, "test.value_source", json!({"max_capacity": {"type": "Int", "value": values.len()}}));
        self.sources.push((name, values));
        id
    }

    pub fn wire(&mut self, from: usize, from_port: &str, to: usize, to_port: &str) {
        self.wires.push(json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port}));
    }

    fn def(&self, into: usize) -> EffectGraphDef {
        let mut nodes = self.nodes.clone();
        let mut wires = self.wires.clone();
        let sink = nodes.len();
        nodes.push(json!({"id": sink, "typeId": "test.value_sink", "nodeId": "sink", "params": {}}));
        nodes.push(json!({"id": sink + 1, "typeId": "system.final_output", "nodeId": "output", "params": {}}));
        wires.push(json!({"fromNode": into, "fromPort": "out", "toNode": sink, "toPort": "values"}));
        wires.push(json!({"fromNode": sink, "fromPort": "out", "toNode": sink + 1, "toPort": "in"}));
        serde_json::from_value(json!({"version": 3, "nodes": nodes, "wires": wires})).expect("chain def")
    }

    /// The sink's input after one frame, `len` values; `fused` regions in the
    /// graph that ran.
    fn run(&self, def: &EffectGraphDef, len: usize) -> (Vec<f32>, usize) {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let mut graph = def.clone().into_graph(&registry, &Default::default()).expect("chain builds");
        // The host fills the sources and reads the sink's input outside the frame.
        let sink = crate::testkit::atom::node_named(&graph, "sink");
        let (into_node, into_port) = graph.wires_into(sink).map(|w| w.from).next().expect("the sink is wired");
        for name in self.sources.iter().map(|(name, _)| name) {
            graph.add_external_output(crate::testkit::atom::node_named(&graph, name), "out").expect("a source port");
        }
        graph.add_external_output(into_node, into_port).expect("the sink's producer port");
        let plan = compile(&graph).expect("chain compiles");
        let fused = graph.nodes().filter(|node| node.node.type_id().as_str() == "node.wgsl_compute").count();
        let device = manifold_gpu::testkit::test_device();
        let mut backend = MetalBackend::new(device.arc(), 8, 8, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&mut graph, &plan, &device, &mut backend).expect("pre-allocate");
        let output = |backend: &MetalBackend, name: &str, port: &str| {
            let node = crate::testkit::atom::node_named(&graph, name);
            let slot = backend.slot_for(crate::testkit::atom::output_of(&plan, node, port)).expect("bound");
            Backend::array_buffer(backend, slot).expect("buffer").clone()
        };
        for (name, values) in &self.sources {
            let buffer = output(&backend, name, "out");
            assert!(buffer.size as usize >= values.len() * 4, "{name} holds its values");
            // SAFETY: a shared buffer at least this long; nothing runs yet.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        let step = plan.steps().iter().find(|s| s.node == sink).expect("sink compiled");
        let input = step.inputs.iter().find(|(name, _)| *name == "values").map(|&(_, r)| r).expect("sink input");
        let mut exec = Executor::new(Box::new(backend));
        let mut enc = device.create_encoder("gpu flip chain");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &device);
            let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut StateStore::new(), 0);
        }
        enc.commit_and_wait_completed();
        let buffer = exec.host_array_buffer(&graph, &plan, input).expect("the sink's input keeps its own storage");
        assert!(buffer.size as usize >= len * 4, "the sink reads {} bytes, not {len} values ({fused} fused regions)", buffer.size);
        (read(buffer, len), fused)
    }

    /// Fused and unfused give the same values bit for bit, and the fused graph
    /// ran one fused kernel.
    pub fn fused_matches_unfused(&self, into: usize, len: usize) -> Vec<f32> {
        let def = self.def(into);
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let Some(fused_def) = crate::freeze::install::fuse_canonical_def(&def, &registry).map(|fused| fused.def) else {
            let report = crate::freeze::fusion_report(&def, &registry);
            let cuts: Vec<String> =
                report.nodes.iter().map(|n| format!("{} {}: {:?}", n.type_id, n.kind, n.cut_reason)).collect();
            panic!("the chain does not fuse: {cuts:#?}");
        };
        let (unfused, none) = self.run(&def, len);
        let (fused, regions) = self.run(&fused_def, len);
        assert_eq!((none, regions), (0, 1), "one fused region");
        let differ: Vec<(usize, f32, f32)> = unfused
            .iter()
            .zip(&fused)
            .enumerate()
            .filter(|(_, (a, b))| a.to_bits() != b.to_bits())
            .map(|(i, (&a, &b))| (i, a, b))
            .collect();
        assert!(
            differ.is_empty(),
            "fused differs from unfused in {} of {len} (index, unfused, fused): {:?}",
            differ.len(),
            &differ[..differ.len().min(12)]
        );
        fused
    }
}



use crate::{graph::Graph, exec::execution_plan::ExecutionPlan, exec::effect_node::NodeInstanceId, exec::execution_plan::ResourceId};
pub fn node_named(graph: &Graph, name: &str) -> NodeInstanceId {
    graph.nodes().find(|n| n.node_id.as_str() == name).map(|n| n.id).unwrap_or_else(|| panic!("no node {name}"))
}
pub fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    step.outputs.iter().find(|(name, _)| *name == port).map(|&(_, r)| r).expect("output port")
}
