//! GPU proofs for `node.switch_mesh` and `node.switch_indices`: the chosen
//! input, written on the GPU earlier in the same command buffer, reaches `out`
//! element for element with its live extent; everything past it is zero; an
//! unwired choice publishes a live extent of zero.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
use manifold_node_engine::exec::execution::Executor;
use manifold_node_engine::exec::execution_plan::{ExecutionPlan, ResourceId, compile};
use manifold_node_engine::exec::metal_backend::MetalBackend;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamValue};
use manifold_node_engine::ports::{ArrayType, KnownItem, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_node_engine::scene::live_extent::LiveExtent;

use super::SwitchMesh;
use crate::node_graph::primitives::switch_indices::SwitchIndices;

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
        let items = (0..capacity).map(|i| if i < live { make(i) } else { T::zeroed() }).collect();
        Self { items, live, bound, per_item, extent: true }
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
        gpu.native_enc.copy_buffer_to_buffer(&self.staged, out, self.staged.size);
        gpu.native_enc.copy_buffer_to_buffer(&self.staged_count, &self.counts, 4);
        if self.extent {
            ctx.outputs.set_live_extent("out", LiveExtent {
                counts: self.counts.clone(),
                offset: 0,
                per_item: self.per_item,
                bound: self.bound,
            });
        }
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

fn output_resource(plan: &ExecutionPlan, node: manifold_node_engine::exec::effect_node::NodeInstanceId) -> ResourceId {
    plan.steps()
        .iter()
        .find(|step| step.node == node)
        .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "out"))
        .map(|&(_, resource)| resource)
        .expect("node has a bound `out`")
}

fn shared<T: bytemuck::Pod>(device: &manifold_gpu::GpuDevice, items: &[T], fill: u8) -> GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(items);
    let buffer = device.create_buffer_shared(bytes.len().max(4) as u64);
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
    out: Vec<u8>,
    count: u32,
    per_item: u32,
    bound: u32,
}

/// Runs `in_0 = a`, `in_1 = b` (either may be unwired) through `switch`, one
/// frame per selector, and returns what its consumer saw each frame. Every
/// pre-bound output starts as garbage, so only GPU writes can match.
fn run<T: KnownItem>(switch: Box<dyn EffectNode>, a: Option<&Input<T>>, b: Option<&Input<T>>, selectors: &[f32]) -> Vec<Frame> {
    let device = manifold_gpu::testkit::test_device();
    let ty = PortType::Array(ArrayType::of_known::<T>());
    let mut graph = Graph::new();
    let switch = graph.add_node(switch);
    let mut writers = Vec::new();
    for (input, port) in [(a, "in_0"), (b, "in_1")] {
        let Some(input) = input else { continue };
        let writer = graph.add_node(Box::new(GpuWriter {
            type_id: EffectNodeType::new(port),
            outputs: vec![NodePort { name: Cow::Borrowed("out"), ty, kind: PortKind::Output, required: false }],
            staged: shared(&device, &input.items, 0),
            staged_count: shared(&device, &[(input.live as u32) / input.per_item], 0),
            counts: shared(&device, &[u32::MAX], 0xAB),
            per_item: input.per_item,
            bound: input.bound,
            extent: input.extent,
        }));
        graph.connect((writer, "out"), (switch, port)).unwrap();
        writers.push((writer, input.items.len()));
    }
    let seen: Seen = Arc::new(Mutex::new(None));
    let sink = graph.add_node(Box::new(Sink {
        inputs: vec![NodePort { name: Cow::Borrowed("in"), ty, kind: PortKind::Input, required: true }],
        seen: seen.clone(),
    }));
    graph.connect((switch, "out"), (sink, "in")).unwrap();
    let plan = compile(&graph).unwrap();

    let item = std::mem::size_of::<T>();
    let capacity = writers.iter().map(|&(_, n)| n).max().unwrap_or(1);
    let mut backend = MetalBackend::new(device.arc(), 1, 1, GpuTextureFormat::Rgba16Float);
    for &(writer, n) in &writers {
        backend.pre_bind_array(output_resource(&plan, writer), shared(&device, &vec![0xEEu8; n * item], 0xEE));
    }
    backend.pre_bind_array(output_resource(&plan, switch), shared(&device, &vec![0xCDu8; capacity * item], 0xCD));
    let mut executor = Executor::new(Box::new(backend));

    let mut frames = Vec::new();
    for (frame_count, &selector) in selectors.iter().enumerate() {
        graph.set_param(switch, "selector", ParamValue::Float(selector)).unwrap();
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
        native.commit_and_wait_completed();
        let (buffer, extent) = seen.lock().unwrap().take().expect("sink ran");
        let extent = extent.expect("the switch always publishes a live extent");
        let read = |buffer: &GpuBuffer, offset: u64, len: usize| {
            let ptr = buffer.mapped_ptr().expect("shared buffer");
            // SAFETY: shared buffer holding `offset + len` bytes; GPU work done.
            unsafe { std::slice::from_raw_parts(ptr.add(offset as usize), len) }.to_vec()
        };
        let count = u32::from_le_bytes(read(&extent.counts, extent.offset, 4).try_into().unwrap());
        frames.push(Frame {
            out: read(&buffer, 0, buffer.size as usize),
            count,
            per_item: extent.per_item,
            bound: extent.bound,
        });
    }
    frames
}

/// The frame the chosen input should produce in an `out` of `capacity` items.
fn expected<T: KnownItem>(input: Option<&Input<T>>, capacity: usize) -> Frame {
    let mut out = vec![0u8; capacity * std::mem::size_of::<T>()];
    let Some(input) = input else {
        return Frame { out, count: 0, per_item: 1, bound: 0 };
    };
    let live: &[u8] = bytemuck::cast_slice(&input.items[..input.live]);
    out[..live.len()].copy_from_slice(live);
    if input.extent {
        Frame { out, count: input.live as u32 / input.per_item, per_item: input.per_item, bound: input.bound }
    } else {
        let n = input.items.len() as u32;
        Frame { out, count: n, per_item: 1, bound: n }
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

/// Frames choose b, a, b: the first clears fresh garbage by copying b's zero
/// tail, the second clears what b left past a's capacity, the third grows back.
fn assert_routes<T: KnownItem>(switch: fn() -> Box<dyn EffectNode>, a: Input<T>, b: Input<T>) {
    let capacity = a.items.len().max(b.items.len());
    let frames = run(switch(), Some(&a), Some(&b), &[1.0, 0.0, 1.0]);
    let want = [expected(Some(&b), capacity), expected(Some(&a), capacity), expected(Some(&b), capacity)];
    for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "frame {i}");
    }
}

/// Only in_1 is wired: choosing in_0 draws nothing, with no garbage left.
fn assert_unwired_choice_is_empty<T: KnownItem>(switch: fn() -> Box<dyn EffectNode>, b: Input<T>) {
    let capacity = b.items.len();
    let frames = run(switch(), None, Some(&b), &[0.0, 1.0, 0.0]);
    let want = [expected::<T>(None, capacity), expected(Some(&b), capacity), expected::<T>(None, capacity)];
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
    assert_routes(mesh_switch, Input::new(vertex, 3, 4, 6, 1), Input::new(vertex, 5, 7, 9, 1));
}

#[test]
fn switch_indices_routes_the_chosen_gpu_written_indices_and_their_live_extent() {
    assert_routes(index_switch, Input::new(index, 6, 9, 12, 3), Input::new(index, 9, 12, 15, 3));
}

#[test]
fn switch_mesh_unwired_choice_has_zero_live_extent() {
    assert_unwired_choice_is_empty(mesh_switch, Input::new(vertex, 5, 7, 9, 1));
}

#[test]
fn switch_indices_unwired_choice_has_zero_live_extent() {
    assert_unwired_choice_is_empty(index_switch, Input::new(index, 9, 12, 15, 3));
}

/// An input that publishes no extent is live to its capacity, which the
/// switch states as an extent so `out`'s spare capacity is not drawn.
#[test]
fn switch_mesh_input_without_extent_is_live_to_its_capacity() {
    let a = Input::new(vertex, 6, 6, 6, 1).without_extent();
    let b = Input::new(vertex, 5, 7, 9, 1);
    let frames = run(mesh_switch(), Some(&a), Some(&b), &[0.0]);
    assert_eq!(frames[0], expected(Some(&a), 9));
}
