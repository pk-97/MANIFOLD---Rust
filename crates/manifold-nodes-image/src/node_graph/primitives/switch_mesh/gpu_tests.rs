//! GPU proofs for `node.switch_mesh`, `node.switch_indices` and
//! `node.switch_instances`. With a param selector the consumer of `out` reads
//! the chosen input's own buffer and live extent (same identities), holding
//! what the input's GPU write left there; an unwired choice is the switch's
//! own buffer, zeroed, with a live extent of zero. A wired selector copies the
//! chosen live items into `out` on the GPU, zero past them.

use manifold_gpu::BufferUse;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use manifold_node_engine::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime,
};
use manifold_node_engine::exec::execution::Executor;
use manifold_node_engine::exec::execution_plan::{ExecutionPlan, ResourceId, compile};
use manifold_node_engine::exec::metal_backend::MetalBackend;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::mesh::{InstanceTransform, MeshVertex};
use manifold_node_engine::parameters::{ParamDef, ParamValue};
use manifold_node_engine::ports::{
    ArrayType, KnownItem, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
};
use manifold_node_engine::scene::live_extent::LiveExtent;

use super::SwitchMesh;
use crate::node_graph::primitives::switch_indices::SwitchIndices;
use crate::node_graph::primitives::switch_instances::SwitchInstances;

/// One switch input: `live` items followed by zeros up to `capacity`, with a
/// live extent of `live / per_item` items and `bound`, or none.
#[derive(Clone)]
struct Input<T> {
    items: Vec<T>,
    live: usize,
    bound: u32,
    per_item: u32,
    extent: bool,
}

impl<T: KnownItem> Input<T> {
    fn new(make: fn(usize) -> T, live: usize, bound: u32, capacity: usize, per_item: u32) -> Self {
        let items = (0..capacity)
            .map(|i| if i < live { make(i) } else { T::zeroed() })
            .collect();
        Self {
            items,
            live,
            bound,
            per_item,
            extent: true,
        }
    }

    fn without_extent(mut self) -> Self {
        self.extent = false;
        self
    }
}

/// Copies its staged items and live count into place on the frame's encoder,
/// then publishes the live extent, as a GPU-counted producer does.
struct GpuWriter {
    type_id: EffectNodeType,
    outputs: Vec<NodeOutput>,
    staged: GpuBuffer,
    staged_count: GpuBuffer,
    counts: GpuBuffer,
    per_item: u32,
    bound: u32,
    extent: bool,
}

impl EffectNode for GpuWriter {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let out = ctx.outputs.array("out").expect("writer output pre-bound");
        let gpu = ctx.gpu_encoder();
        gpu.native_enc
            .copy_buffer_to_buffer(&self.staged, out, self.staged.size);
        gpu.native_enc
            .copy_buffer_to_buffer(&self.staged_count, &self.counts, 4);
        if self.extent {
            ctx.outputs.set_live_extent(
                "out",
                LiveExtent {
                    counts: self.counts.clone(),
                    offset: 0,
                    per_item: self.per_item,
                    bound: self.bound,
                },
            );
        }
    }
}

/// Drives the switch's selector through a wire, which keeps both inputs live
/// and makes the switch copy instead of alias.
struct SelectorWire {
    outputs: Vec<NodeOutput>,
    value: Arc<Mutex<f32>>,
}

impl EffectNode for SelectorWire {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        static ID: std::sync::OnceLock<EffectNodeType> = std::sync::OnceLock::new();
        ID.get_or_init(|| EffectNodeType::new("test.selector_wire"))
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        ctx.outputs
            .set_scalar("out", ParamValue::Float(*self.value.lock().unwrap()));
    }
}

type Seen = Arc<Mutex<Option<(GpuBuffer, Option<LiveExtent>)>>>;

/// Keeps the switch live and records the buffer and extent it receives.
struct Sink {
    inputs: Vec<NodeInput>,
    seen: Seen,
}

impl EffectNode for Sink {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        static ID: std::sync::OnceLock<EffectNodeType> = std::sync::OnceLock::new();
        ID.get_or_init(|| EffectNodeType::new("test.switch_sink"))
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let buffer = ctx.inputs.array("in").expect("switch output bound").clone();
        *self.seen.lock().unwrap() = Some((buffer, ctx.inputs.live_extent("in")));
    }
    fn is_liveness_root(&self) -> bool {
        true
    }
}

fn output_resource(
    plan: &ExecutionPlan,
    node: manifold_node_engine::exec::effect_node::NodeInstanceId,
) -> ResourceId {
    plan.steps()
        .iter()
        .find(|step| step.node == node)
        .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "out"))
        .map(|&(_, resource)| resource)
        .expect("node has a bound `out`")
}

fn shared<T: bytemuck::Pod>(device: &manifold_gpu::GpuDevice, items: &[T], fill: u8) -> GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(items);
    let buffer = device.create_buffer(bytes.len().max(4) as u64, BufferUse::Upload);
    // SAFETY: fresh shared buffer; no GPU work has seen it.
    unsafe {
        buffer.write(0, &vec![fill; buffer.size as usize]);
        buffer.write(0, bytes);
    }
    buffer
}

/// What the switch's consumer saw after one frame.
#[derive(Debug, PartialEq)]
struct Frame {
    /// Identity of the buffer the consumer read.
    buffer: usize,
    out: Vec<u8>,
    /// Count word, items per count and bound of the extent it read.
    extent: Option<(u32, u32, u32)>,
    /// Identity of the extent's count buffer when it is a writer's own.
    counts: Option<usize>,
}

/// Identities of the pre-bound buffers: each wired input's storage and count
/// word, and the switch's own output storage.
struct Keys {
    inputs: [Option<(usize, usize)>; 2],
    switch: usize,
}

/// Runs `in_0 = a`, `in_1 = b` (either may be unwired) through `switch`, one
/// frame per selector (a param, or through a wire when `wired`), and returns
/// what its consumer saw each frame. Every pre-bound output starts as
/// garbage, so only GPU writes can match.
fn run<T: KnownItem>(
    switch: Box<dyn EffectNode>,
    a: Option<&Input<T>>,
    b: Option<&Input<T>>,
    selectors: &[f32],
    wired: bool,
) -> (Vec<Frame>, Keys) {
    let device = manifold_gpu::testkit::test_device();
    let ty = PortType::Array(ArrayType::of_known::<T>());
    let mut graph = Graph::new();
    let switch = graph.add_node(switch);
    let mut writers = Vec::new();
    let mut keys = Keys {
        inputs: [None, None],
        switch: 0,
    };
    for (slot, (input, port)) in [(a, "in_0"), (b, "in_1")].into_iter().enumerate() {
        let Some(input) = input else { continue };
        let counts = shared(&device, &[u32::MAX], 0xAB);
        let counts_key = counts.identity_key();
        let writer = graph.add_node(Box::new(GpuWriter {
            type_id: EffectNodeType::new(port),
            outputs: vec![NodePort {
                name: Cow::Borrowed("out"),
                ty,
                kind: PortKind::Output,
                required: false,
            }],
            staged: shared(&device, &input.items, 0),
            staged_count: shared(&device, &[(input.live as u32) / input.per_item], 0),
            counts,
            per_item: input.per_item,
            bound: input.bound,
            extent: input.extent,
        }));
        graph.connect((writer, "out"), (switch, port)).unwrap();
        writers.push((writer, input.items.len(), slot, counts_key));
    }
    let seen: Seen = Arc::new(Mutex::new(None));
    let sink = graph.add_node(Box::new(Sink {
        inputs: vec![NodePort {
            name: Cow::Borrowed("in"),
            ty,
            kind: PortKind::Input,
            required: true,
        }],
        seen: seen.clone(),
    }));
    graph.connect((switch, "out"), (sink, "in")).unwrap();
    let wire = Arc::new(Mutex::new(0.0));
    if wired {
        let scalar = PortType::Scalar(ScalarType::F32);
        let selector = graph.add_node(Box::new(SelectorWire {
            outputs: vec![NodePort {
                name: Cow::Borrowed("out"),
                ty: scalar,
                kind: PortKind::Output,
                required: false,
            }],
            value: wire.clone(),
        }));
        graph
            .connect((selector, "out"), (switch, "selector"))
            .unwrap();
    }
    let plan = compile(&graph).unwrap();

    let item = std::mem::size_of::<T>();
    let capacity = writers.iter().map(|&(_, n, _, _)| n).max().unwrap_or(1);
    let mut backend = MetalBackend::new(device.arc(), 1, 1, GpuTextureFormat::Rgba16Float);
    for &(writer, n, slot, counts_key) in &writers {
        let buffer = shared(&device, &vec![0xEEu8; n * item], 0xEE);
        keys.inputs[slot] = Some((buffer.identity_key(), counts_key));
        backend.pre_bind_array(output_resource(&plan, writer), buffer);
    }
    let own = shared(&device, &vec![0xCDu8; capacity * item], 0xCD);
    keys.switch = own.identity_key();
    backend.pre_bind_array(output_resource(&plan, switch), own);
    let mut executor = Executor::new(Box::new(backend));

    let mut frames = Vec::new();
    for (frame_count, &selector) in selectors.iter().enumerate() {
        graph
            .set_param(switch, "selector", ParamValue::Float(selector))
            .unwrap();
        *wire.lock().unwrap() = selector;
        let mut native = device.create_encoder("switch gpu proof");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native, &device);
            let time = FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: frame_count as i64,
            };
            executor.execute_frame_with_gpu(&mut graph, &plan, time, &mut gpu);
        }
        native.commit_and_wait().expect("GPU work completes");
        let (buffer, extent) = seen.lock().unwrap().take().expect("sink ran");
        let read = |buffer: &GpuBuffer, offset: u64, len: usize| {
            let ptr = buffer.mapped_ptr().expect("shared buffer");
            // SAFETY: shared buffer holding `offset + len` bytes; GPU work done.
            unsafe { std::slice::from_raw_parts(ptr.add(offset as usize), len) }.to_vec()
        };
        let counts = extent
            .as_ref()
            .map(|e| e.counts.identity_key())
            .filter(|key| {
                keys.inputs
                    .iter()
                    .flatten()
                    .any(|&(_, counts)| counts == *key)
            });
        frames.push(Frame {
            buffer: buffer.identity_key(),
            out: read(&buffer, 0, buffer.size as usize),
            extent: extent.map(|e| {
                let count = u32::from_le_bytes(read(&e.counts, e.offset, 4).try_into().unwrap());
                (count, e.per_item, e.bound)
            }),
            counts,
        });
    }
    (frames, keys)
}

/// What the consumer should see when `in_{port}` is chosen. A wired choice is
/// the input's own buffer and extent, untouched; an unwired one is the
/// switch's own buffer, zeroed, with an extent of zero.
fn expected<T: KnownItem>(
    input: Option<&Input<T>>,
    port: usize,
    keys: &Keys,
    capacity: usize,
) -> Frame {
    let Some(input) = input else {
        let out = vec![0u8; capacity * std::mem::size_of::<T>()];
        return Frame {
            buffer: keys.switch,
            out,
            extent: Some((0, 1, 0)),
            counts: None,
        };
    };
    let (buffer, counts) = keys.inputs[port].expect("a wired input is pre-bound");
    let out = bytemuck::cast_slice(&input.items).to_vec();
    if input.extent {
        let extent = Some((
            input.live as u32 / input.per_item,
            input.per_item,
            input.bound,
        ));
        Frame {
            buffer,
            out,
            extent,
            counts: Some(counts),
        }
    } else {
        Frame {
            buffer,
            out,
            extent: None,
            counts: None,
        }
    }
}

/// What the consumer should see when a wired selector copies `in_{port}`:
/// the switch's own buffer holding the live items then zeros, and the input's
/// extent.
fn copied<T: KnownItem>(input: &Input<T>, port: usize, keys: &Keys, capacity: usize) -> Frame {
    let mut out = vec![0u8; capacity * std::mem::size_of::<T>()];
    let live: &[u8] = bytemuck::cast_slice(&input.items[..input.live]);
    out[..live.len()].copy_from_slice(live);
    let counts = keys.inputs[port].map(|(_, counts)| counts);
    Frame {
        buffer: keys.switch,
        out,
        extent: Some((
            input.live as u32 / input.per_item,
            input.per_item,
            input.bound,
        )),
        counts,
    }
}

fn vertex(i: usize) -> MeshVertex {
    let f = i as f32 + 1.0;
    MeshVertex {
        position: [f, -f, 0.5 * f],
        _pad0: 0.0,
        normal: [0.0, 1.0, 0.0],
        _pad1: 0.0,
        uv: [0.1 * f, 0.2 * f],
        _pad2: [0.0; 2],
        tangent: [1.0, 0.0, 0.0, 1.0],
        color: [f, 1.0, 1.0, 1.0],
    }
}

fn index(i: usize) -> u32 {
    (i as u32 * 7 + 3) % 1000 + 1
}

/// Frames choose b, a, b: each frame the consumer reads the chosen writer's
/// own buffer and extent, the bytes its GPU write left there, and the
/// switch's own garbage-filled buffer is never seen.
fn assert_routes<T: KnownItem>(switch: fn() -> Box<dyn EffectNode>, a: Input<T>, b: Input<T>) {
    let capacity = a.items.len().max(b.items.len());
    let (frames, keys) = run(switch(), Some(&a), Some(&b), &[1.0, 0.0, 1.0], false);
    let want = [
        expected(Some(&b), 1, &keys, capacity),
        expected(Some(&a), 0, &keys, capacity),
        expected(Some(&b), 1, &keys, capacity),
    ];
    for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "frame {i}");
    }
}

/// Only in_1 is wired: choosing in_0 draws nothing from the switch's own
/// buffer, zeroed once; passing b through in between leaves it zero.
fn assert_unwired_choice_is_empty<T: KnownItem>(switch: fn() -> Box<dyn EffectNode>, b: Input<T>) {
    let capacity = b.items.len();
    let (frames, keys) = run(switch(), None, Some(&b), &[0.0, 1.0, 0.0], false);
    let want = [
        expected::<T>(None, 0, &keys, capacity),
        expected(Some(&b), 1, &keys, capacity),
        expected::<T>(None, 0, &keys, capacity),
    ];
    for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "frame {i}");
    }
}

fn mesh_switch() -> Box<dyn EffectNode> {
    Box::new(SwitchMesh::new())
}

fn index_switch() -> Box<dyn EffectNode> {
    Box::new(SwitchIndices::new())
}

#[test]
fn switch_mesh_routes_the_chosen_gpu_written_vertices_and_their_live_extent() {
    assert_routes(
        mesh_switch,
        Input::new(vertex, 3, 4, 6, 1),
        Input::new(vertex, 5, 7, 9, 1),
    );
}

#[test]
fn switch_indices_routes_the_chosen_gpu_written_indices_and_their_live_extent() {
    assert_routes(
        index_switch,
        Input::new(index, 6, 9, 12, 3),
        Input::new(index, 9, 12, 15, 3),
    );
}

#[test]
fn switch_mesh_unwired_choice_has_zero_live_extent() {
    assert_unwired_choice_is_empty(mesh_switch, Input::new(vertex, 5, 7, 9, 1));
}

#[test]
fn switch_indices_unwired_choice_has_zero_live_extent() {
    assert_unwired_choice_is_empty(index_switch, Input::new(index, 9, 12, 15, 3));
}

/// An input that publishes no extent passes through without one: the
/// consumer reads the input's own buffer, live to its own size.
#[test]
fn switch_mesh_input_without_extent_passes_through_without_one() {
    let a = Input::new(vertex, 6, 6, 6, 1).without_extent();
    let b = Input::new(vertex, 5, 7, 9, 1);
    let (frames, keys) = run(mesh_switch(), Some(&a), Some(&b), &[0.0], false);
    assert_eq!(frames[0], expected(Some(&a), 0, &keys, 9));
}

/// A wired selector keeps both inputs live, so the switch copies on the GPU:
/// frames choose b, a, b, and each copy leaves `out` zero past the live
/// items (fresh garbage first, then what b left past a).
#[test]
fn switch_mesh_wired_selector_copies_the_chosen_live_vertices() {
    let a = Input::new(vertex, 3, 4, 6, 1);
    let b = Input::new(vertex, 5, 7, 9, 1);
    let (frames, keys) = run(mesh_switch(), Some(&a), Some(&b), &[1.0, 0.0, 1.0], true);
    let want = [
        copied(&b, 1, &keys, 9),
        copied(&a, 0, &keys, 9),
        copied(&b, 1, &keys, 9),
    ];
    for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "frame {i}");
    }
}

fn instance(i: usize) -> InstanceTransform {
    let f = i as f32 + 1.0;
    InstanceTransform {
        pos_scale: [f, -f, 0.5 * f, 0.01 * f],
        rot_pad: [0.1 * f, 0.2 * f, 0.3 * f, 0.0],
    }
}

fn instance_switch() -> Box<dyn EffectNode> {
    Box::new(SwitchInstances::new())
}

#[test]
fn switch_instances_routes_the_chosen_gpu_written_instances_and_their_live_extent() {
    assert_routes(
        instance_switch,
        Input::new(instance, 3, 4, 6, 1),
        Input::new(instance, 5, 7, 9, 1),
    );
}

/// Physics View's shape: copies on in_1 only; the off choice is empty.
#[test]
fn switch_instances_unwired_choice_has_zero_live_extent() {
    assert_unwired_choice_is_empty(instance_switch, Input::new(instance, 5, 7, 9, 1));
}

/// Copies publish no extent: the consumer reads their own buffer, live to
/// its size, and the switch's buffer is not touched.
#[test]
fn switch_instances_input_without_extent_passes_through_without_one() {
    let b = Input::new(instance, 9, 9, 9, 1).without_extent();
    let (frames, keys) = run(instance_switch(), None, Some(&b), &[1.0], false);
    assert_eq!(frames[0], expected(Some(&b), 1, &keys, 9));
}
