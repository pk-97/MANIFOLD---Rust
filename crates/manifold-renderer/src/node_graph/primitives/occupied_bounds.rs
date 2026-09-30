//! `node.occupied_bounds` — the box around a lattice's occupied nodes, read
//! back to the CPU a frame or more late (docs/FFT_WATER_SOLVER_DESIGN.md P3c).
//! A barriered two-pass reduction plus a readback bridge
//! (docs/ADDING_PRIMITIVES.md exclusions 1 and 3). The reading lands in a
//! ring of shared buffers, each fenced by its own event and polled, never
//! waited on: the CPU reads a slot only once the GPU has signalled it done.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuEvent};

use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, NodeInstanceId, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::{NodeState, OwnerKey, StateStore};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoundsParams {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    groups: u32,
    threshold: f32,
    _pad: [u32; 3],
}

/// First-pass workgroups at most; `partials` holds one reading per group.
pub(super) const MAX_GROUPS: u32 = 256;
/// Words in one reading: lowest x, y, z; one past the highest x, y, z; count; 0.
pub(super) const READING_WORDS: u32 = 8;
/// Nodes each first-pass thread visits, about.
const NODES_PER_THREAD: u32 = 16;
/// Readings in flight at once. A frame with no free slot skips its reading.
const RING: usize = 3;

pub(super) const MIN_PORTS: [&str; 3] = ["min_x", "min_y", "min_z"];
pub(super) const END_PORTS: [&str; 3] = ["end_x", "end_y", "end_z"];

/// First-pass workgroups for a lattice of `nodes` nodes.
pub(super) fn reduction_groups(nodes: u32) -> u32 {
    nodes.div_ceil(256 * NODES_PER_THREAD).clamp(1, MAX_GROUPS)
}

/// Lattice lengths from the params: whole numbers, 1 to 1024.
pub(super) fn bounds_lattice(params: &ParamValues) -> Option<[u32; 3]> {
    let mut nodes = [0; 3];
    for (axis, name) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().enumerate() {
        let n = match params.get(name) {
            Some(ParamValue::Float(n)) => *n,
            _ => 64.0,
        };
        if !(1.0..=1024.0).contains(&n) || n.fract() != 0.0 {
            return None;
        }
        nodes[axis] = n as u32;
    }
    Some(nodes)
}

/// One completed reading and the frame it was taken on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Reading {
    pub low: [u32; 3],
    pub end: [u32; 3],
    pub count: u32,
    pub frame: u64,
}

impl Reading {
    fn from_words(words: [u32; 8], frame: u64) -> Self {
        Self { low: [words[0], words[1], words[2]], end: [words[3], words[4], words[5]], count: words[6], frame }
    }
}

/// One liquid's readings, kept in the StateStore so a cleared state (a
/// transport stop, a project load) starts over with no reading.
pub struct BoundsReadings {
    /// Tells this run of readings from any before it.
    generation: u64,
    frame: u64,
    latest: Option<Reading>,
}

impl NodeState for BoundsReadings {}

struct Slot {
    buffer: GpuBuffer,
    event: Option<GpuEvent>,
    /// The event value that marks this slot's reading done; 0 when free.
    signal: u64,
    owner: OwnerKey,
    generation: u64,
    frame: u64,
}

/// The node's GPU side: pipelines, the partials and the readback ring. Every
/// owner shares it; each slot names the owner and generation it reads for.
pub struct BoundsGpu {
    partial: Option<GpuComputePipeline>,
    finalize: Option<GpuComputePipeline>,
    partials: Option<GpuBuffer>,
    ring: Vec<Slot>,
    next: usize,
    generations: u64,
}

impl BoundsGpu {
    pub fn new() -> Self {
        Self { partial: None, finalize: None, partials: None, ring: Vec::new(), next: 0, generations: 0 }
    }

    /// Hand every slot the GPU has finished to its owner's readings, if that
    /// run of readings still exists; the newest reading wins.
    fn poll(&mut self, store: &mut StateStore, node: NodeInstanceId) {
        for slot in &mut self.ring {
            if slot.signal == 0 || !slot.event.as_ref().is_some_and(|event| event.is_done(slot.signal)) {
                continue;
            }
            slot.signal = 0;
            let Some(readings) = store.get::<BoundsReadings>(node, slot.owner) else { continue };
            if readings.generation != slot.generation || readings.latest.is_some_and(|r| r.frame >= slot.frame) {
                continue;
            }
            let Some(ptr) = slot.buffer.mapped_ptr() else { continue };
            // SAFETY: a shared buffer of READING_WORDS words the GPU finished
            // writing (its event reached `signal`); nothing writes it until
            // the slot is handed out again.
            let words = unsafe { std::ptr::read_unaligned(ptr.cast::<[u32; 8]>()) };
            readings.latest = Some(Reading::from_words(words, slot.frame));
        }
    }
}

impl Default for BoundsGpu {
    fn default() -> Self {
        Self::new()
    }
}

crate::primitive! {
    name: OccupiedBounds,
    type_id: "node.occupied_bounds",
    purpose: "The box around a lattice's occupied nodes (value above threshold; nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k)) and how many there are, read back to the CPU without waiting: min_x/y/z is the lowest occupied node, end_x/y/z one past the highest, count the occupied nodes, age how many frames ago the reading was taken (0: no reading yet; min 0 and end 0 when nothing is occupied).",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        min_x: ScalarF32,
        min_y: ScalarF32,
        min_z: ScalarF32,
        end_x: ScalarF32,
        end_y: ScalarF32,
        end_z: ScalarF32,
        count: ScalarF32,
        age: ScalarF32,
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 1.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 1.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 1.0, 1024.0),
        float_param!("threshold", "Threshold", 0.5, -1e6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "Feeds node.active_region, which turns the reading into the box a solve runs on. The reading is at least one frame old, since the CPU never waits for this frame's; wire age through so the consumer knows which frame it describes.",
    examples: [],
    picker: { label: "Occupied Bounds", category: Atom },
    summary: "Finds the box around the filled cells of a 3D grid, a frame late.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["bounding box", "occupied region", "extent"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        bounds: BoundsGpu = BoundsGpu::new(),
    },
}

impl Primitive for OccupiedBounds {
    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        bounds_lattice(params).is_none().then(|| "Occupied Bounds: every length must be a whole number, 1 to 1024".to_string())
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (node, owner) = (ctx.node_id, ctx.owner_key);
        let Some(store) = ctx.state.as_deref_mut() else {
            ctx.error("Occupied Bounds: needs the state store its readings live in".to_string());
            return;
        };
        self.bounds.poll(store, node);
        if store.get::<BoundsReadings>(node, owner).is_none() {
            self.bounds.generations += 1;
            store.insert(node, owner, BoundsReadings { generation: self.bounds.generations, frame: 0, latest: None });
        }
        let readings = store.get::<BoundsReadings>(node, owner).expect("readings inserted above");
        let (frame, generation, latest) = (readings.frame, readings.generation, readings.latest);
        readings.frame += 1;
        let (low, end) = latest.filter(|r| r.count > 0).map_or(([0; 3], [0; 3]), |r| (r.low, r.end));
        for axis in 0..3 {
            ctx.outputs.set_scalar(MIN_PORTS[axis], ParamValue::Float(low[axis] as f32));
            ctx.outputs.set_scalar(END_PORTS[axis], ParamValue::Float(end[axis] as f32));
        }
        ctx.outputs.set_scalar("count", ParamValue::Float(latest.map_or(0, |r| r.count) as f32));
        ctx.outputs.set_scalar("age", ParamValue::Float(latest.map_or(0, |r| frame - r.frame) as f32));

        let Some(nodes) = bounds_lattice(ctx.params) else {
            ctx.error("Occupied Bounds: every length must be a whole number, 1 to 1024".to_string());
            return;
        };
        let total = nodes.iter().product::<u32>();
        let Some(values) = ctx.inputs.array("values") else { return };
        if u64::from(total) * 4 > values.size {
            ctx.error(format!("Occupied Bounds: a {nodes:?} lattice is larger than its array"));
            return;
        }
        let threshold = ctx.scalar_or_param("threshold", 0.5);
        let gpu = ctx.gpu_encoder();
        let state = &mut self.bounds;
        if state.ring.is_empty() {
            state.ring = (0..RING)
                .map(|_| Slot {
                    buffer: gpu.device.create_buffer_shared(u64::from(READING_WORDS) * 4),
                    event: None,
                    signal: 0,
                    owner,
                    generation: 0,
                    frame: 0,
                })
                .collect();
            state.partials = Some(gpu.device.create_buffer(u64::from(MAX_GROUPS * READING_WORDS) * 4));
            let source = include_str!("shaders/occupied_bounds.wgsl");
            state.partial = Some(gpu.device.create_compute_pipeline(source, "partial_main", "node.occupied_bounds"));
            state.finalize = Some(gpu.device.create_compute_pipeline(source, "finalize_main", "node.occupied_bounds"));
        }
        let Some(index) = (0..RING).map(|step| (state.next + step) % RING).find(|&i| state.ring[i].signal == 0) else {
            return;
        };
        state.next = (index + 1) % RING;
        let groups = reduction_groups(total);
        let uniforms = BoundsParams {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            groups,
            threshold,
            _pad: [0; 3],
        };
        let partials = state.partials.as_ref().expect("partials allocated");
        let slot = &mut state.ring[index];
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: partials, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &slot.buffer, offset: 0 },
        ];
        let encoder = &mut *gpu.native_enc;
        encoder.dispatch_compute(state.partial.as_ref().expect("pipeline built"), &bindings, [groups, 1, 1], "node.occupied_bounds.partial");
        encoder.dispatch_compute(state.finalize.as_ref().expect("pipeline built"), &bindings, [1, 1, 1], "node.occupied_bounds.finalize");
        let event = slot.event.get_or_insert_with(|| gpu.device.create_event());
        encoder.signal_event(event);
        slot.signal = event.current_value();
        (slot.owner, slot.generation, slot.frame) = (owner, generation, frame);
    }
}
