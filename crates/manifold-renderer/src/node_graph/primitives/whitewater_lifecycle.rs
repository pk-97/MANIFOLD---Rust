//! `node.whitewater_lifecycle` — FLIP's own whitewater lifecycle for any GPU
//! liquid (`docs/GPU_WHITEWATER_DESIGN.md` D1, D6–D8, D10–D12, section 3.5).
//! Each frame with ticks it snapshots the spawns and fields on the GPU; a
//! later frame, once that one retired, hands them to the vendored
//! `DiffuseParticleSimulation` and publishes the population as three
//! particle frames. Live never waits on the GPU; offline does, so an export
//! is the same at any speed.

use std::borrow::Cow;

use manifold_fluids::{WhitewaterGrid, WhitewaterLifecycle as NativeLifecycle, WhitewaterParticle, WhitewaterSpawn};
use manifold_gpu::GpuBuffer;

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::offline_simulation;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{face_offset, grid_box, grid_cells, require_extended_faces};
use crate::node_graph::whitewater_handoff::{CaptureInputs, Fence, OutputRing, Retired, SnapshotRing, SnapshotShape};

/// FLIP's own default whitewater budget.
pub const DEFAULT_CAPACITY: u32 = 100_000;
pub const MAX_CAPACITY: u32 = 250_000;

const OUTPUTS: [&str; 3] = ["foam_particles", "bubble_particles", "spray_particles"];

crate::primitive! {
    name: WhitewaterLifecycle,
    type_id: "node.whitewater_lifecycle",
    purpose: "Advance whitewater with FLIP's own lifecycle: spray falls and bounces, bubbles rise and drag, foam rides the surface, each ages and dies as FLIP's native whitewater does. Spawns and the liquid's faces, distance and solid are copied on the GPU each frame with ticks and handed to the lifecycle once that frame finished, so the population trails the water by one frame offline and one or two live. Out come foam, bubbles and spray as particle frames, each particle's radius its fade, with their counts. A new epoch clears everything; ticks 0 holds it. Past Capacity, spawns are thinned evenly and counted.",
    inputs: {
        spawns: Array(WhitewaterSpawn) required,
        offsets: Array(u32) optional,
        count: ScalarF32 optional,
        face_u: Array(f32) required, face_v: Array(f32) required, face_w: Array(f32) required,
        face_cells_x: ScalarF32 optional, face_cells_y: ScalarF32 optional, face_cells_z: ScalarF32 optional,
        face_valid_layers: ScalarF32 optional,
        level: Array(f32) required,
        solid: Array(f32) required,
        grid_bounds: Transform optional,
        grid_nodes_x: ScalarF32 optional, grid_nodes_y: ScalarF32 optional, grid_nodes_z: ScalarF32 optional,
        ticks: ScalarF32 optional,
        epoch: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
    },
    outputs: {
        foam_particles: Array(FluidParticle),
        bubble_particles: Array(FluidParticle),
        spray_particles: Array(FluidParticle),
        foam_count: ScalarF32,
        bubble_count: ScalarF32,
        spray_count: ScalarF32,
        emitted: ScalarF32,
        thinned: ScalarF32,
        dropped_ticks: ScalarF32,
        lifecycle_ms: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("capacity"),
            label: "Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(DEFAULT_CAPACITY as f32),
            range: Some((1.0, MAX_CAPACITY as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "spawns from the whitewater spawn atoms, offsets and count from the running total over their emission counts (its last entry is the frame's emitted count). face_u/v/w, face_cells_x/y/z and face_valid_layers from the liquid frame's face grid (at least one valid layer), solid, grid_bounds and grid_nodes_x/y/z from the particle frame, level from node.crossing_distance, ticks, epoch and gravity_x/gravity/gravity_z from the liquid domain. The face grid must sit centred on the solid lattice's cells by a whole number of cells. Draw each particles output with node.particles_to_copies, its count wired to live_count. emitted, thinned and dropped_ticks count since the epoch began; dropped_ticks grows only live, when the GPU is three frames behind. lifecycle_ms is this frame's CPU time in the lifecycle.",
    examples: [],
    picker: { label: "Whitewater Lifecycle", category: Atom },
    summary: "Moves and ages spray, foam and bubbles the way FLIP's own whitewater does.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater", "foam", "spray", "bubbles", "diffuse particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        lifecycle: Option<NativeLifecycle> = None,
        epoch: Option<u32> = None,
        snapshots: SnapshotRing = SnapshotRing::default(),
        outputs: OutputRing = OutputRing::default(),
        population: Vec<WhitewaterParticle> = Vec::new(),
        dirty: bool = true,
        emitted: u64 = 0,
        thinned: u64 = 0,
        dropped_ticks: u64 = 0,
    },
}

/// What one frame's inputs say, once every placement rule held.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub shape: SnapshotShape,
    pub ticks: u32,
    pub epoch: u32,
    pub gravity: [f32; 3],
}

/// What one frame did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    /// Foam, bubbles, spray in the provided outputs.
    pub counts: [u32; 3],
    pub emitted: u64,
    pub thinned: u64,
    pub dropped_ticks: u64,
    /// Snapshots captured and not yet handed to the lifecycle.
    pub pending: usize,
    pub lifecycle_ms: f64,
    pub failure: Option<String>,
}

impl WhitewaterLifecycle {
    fn frame(ctx: &EffectNodeContext<'_, '_>) -> Result<Frame, String> {
        let whole = |v: f32| v.round().max(0.0) as u32;
        let capacity = whole(ctx.param_f32("capacity", DEFAULT_CAPACITY as f32)).clamp(1, MAX_CAPACITY);
        let nodes = ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].map(|name| whole(ctx.scalar_or_param(name, 0.0)));
        let cells = grid_cells(nodes).ok_or_else(|| format!("a {nodes:?} solid lattice has too few or too many nodes"))?;
        let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| whole(ctx.scalar_or_param(name, 0.0)));
        let face_offset = face_offset(nodes, face_cells)?;
        require_extended_faces(ctx.scalar_or_param("face_valid_layers", 0.0))?;
        let bounds = ctx.inputs.transform("grid_bounds").ok_or("the grid_bounds input is not wired")?;
        let (origin, cell_size) = grid_box(bounds, nodes)?;
        Ok(Frame {
            shape: SnapshotShape { grid: WhitewaterGrid { cells, cell_size, origin }, face_cells, face_offset, capacity },
            ticks: whole(ctx.scalar_or_param("ticks", 0.0)),
            epoch: whole(ctx.scalar_or_param("epoch", 0.0)),
            gravity: [ctx.scalar_or_param("gravity_x", 0.0), ctx.scalar_or_param("gravity", -9.81), ctx.scalar_or_param("gravity_z", 0.0)],
        })
    }

    /// A lifecycle on this grid with this capacity, and the population
    /// cleared on a new epoch before anything loads (D12, I6).
    pub(crate) fn prepare(&mut self, frame: &Frame) -> Result<(), String> {
        let shape = frame.shape;
        let fits = self.lifecycle.as_ref().is_some_and(|l| l.grid() == shape.grid && l.capacity() == shape.capacity);
        if !fits {
            self.lifecycle = None;
            self.lifecycle = Some(NativeLifecycle::new(shape.grid, shape.capacity, u64::from(frame.epoch)).map_err(|e| e.to_string())?);
            self.epoch = None;
        }
        if self.epoch != Some(frame.epoch) {
            if fits {
                self.lifecycle.as_mut().expect("lifecycle").clear(u64::from(frame.epoch)).map_err(|e| e.to_string())?;
            }
            self.epoch = Some(frame.epoch);
            self.emitted = 0;
            self.thinned = 0;
            self.dropped_ticks = 0;
            self.dirty = true;
        }
        Ok(())
    }

    /// Hand every retired snapshot of this epoch and shape to the lifecycle,
    /// oldest first (I4). Live stops at the first that hasn't retired (I3);
    /// offline waits for it.
    fn consume(&mut self, fence: &dyn Fence, offline: bool, frame: &Frame) -> Result<(), String> {
        let lifecycle = self.lifecycle.as_mut().ok_or("no lifecycle: prepare runs first")?;
        while let Some(index) = self.snapshots.oldest_pending() {
            let slot = &mut self.snapshots.slots[index];
            if !fence.is_complete(slot.stamp) {
                if !offline {
                    break;
                }
                if !fence.wait(slot.stamp) {
                    return Err("a frame's snapshot did not finish on the GPU within 5 seconds".into());
                }
            }
            slot.pending = false;
            if slot.epoch != frame.epoch || slot.shape != frame.shape {
                continue;
            }
            // SAFETY: the slot's frame retired and nothing captured over it since.
            let (fields, spawns, emitted) = unsafe { (slot.fields(), slot.spawns(), slot.emitted()) };
            lifecycle.set_fields(&fields).map_err(|e| e.to_string())?;
            let (_, load_thinned) = lifecycle.load(spawns).map_err(|e| e.to_string())?;
            self.emitted += u64::from(emitted);
            self.thinned += u64::from(emitted.saturating_sub(frame.shape.capacity)) + u64::from(load_thinned);
            for _ in 0..slot.ticks {
                lifecycle.step(TICK).map_err(|e| e.to_string())?;
            }
            self.dirty = true;
        }
        Ok(())
    }

    /// Republish a changed population into an output slot whose readers all
    /// retired; with none, the current slot stays and the next frame retries.
    fn publish(&mut self, gpu: &GpuEncoder<'_>, fence: &dyn Fence) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let lifecycle = self.lifecycle.as_mut().ok_or("no lifecycle: prepare runs first")?;
        lifecycle.particles(&mut self.population).map_err(|e| e.to_string())?;
        self.dirty = !self.outputs.publish(gpu.device, fence, &self.population)?;
        Ok(())
    }

    /// One frame after [`Self::prepare`]: consume, publish, capture, in that
    /// order (section 3.5).
    pub(crate) fn advance(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        fence: &dyn Fence,
        offline: bool,
        frame: &Frame,
        inputs: &CaptureInputs<'_>,
    ) -> Report {
        let start = std::time::Instant::now();
        let mut failure = self.consume(fence, offline, frame).and_then(|()| self.publish(gpu, fence)).err();
        self.outputs.mark_read(fence);
        let lifecycle_ms = start.elapsed().as_secs_f64() * 1000.0;
        if failure.is_none() && frame.ticks > 0 {
            failure = match self.snapshots.free_slot(gpu.device, frame.shape) {
                Ok(Some(index)) => self.snapshots.slots[index]
                    .capture(gpu.native_enc, inputs, fence.stamp(), frame.epoch, frame.ticks, frame.gravity)
                    .err(),
                Ok(None) => {
                    self.dropped_ticks += u64::from(frame.ticks);
                    None
                }
                Err(error) => Some(error),
            };
        }
        Report {
            counts: self.outputs.current().map_or([0; 3], |slot| slot.counts),
            emitted: self.emitted,
            thinned: self.thinned,
            dropped_ticks: self.dropped_ticks,
            pending: self.snapshots.pending_count(),
            lifecycle_ms,
            failure,
        }
    }
}

impl Primitive for WhitewaterLifecycle {
    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let population = OUTPUTS.iter().position(|&name| name == port)?;
        self.outputs.current().map(|slot| &slot.buffers[population])
    }

    /// Planned at Capacity, so every copies array downstream holds the whole
    /// budget; the provided storage grows to the population in 4,096 steps.
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        OUTPUTS.contains(&port).then(|| match params.get("capacity") {
            Some(ParamValue::Float(v)) => (v.round().max(1.0) as u32).min(MAX_CAPACITY),
            _ => DEFAULT_CAPACITY,
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let frame = match Self::frame(ctx).and_then(|frame| self.prepare(&frame).map(|()| frame)) {
            Ok(frame) => frame,
            Err(refusal) => {
                ctx.error(format!("Whitewater: {refusal}"));
                return;
            }
        };
        let (Some(spawns), Some(face_u), Some(face_v), Some(face_w), Some(level), Some(solid)) = (
            ctx.inputs.array("spawns"),
            ctx.inputs.array("face_u"),
            ctx.inputs.array("face_v"),
            ctx.inputs.array("face_w"),
            ctx.inputs.array("level"),
            ctx.inputs.array("solid"),
        ) else {
            ctx.error("Whitewater: spawns, face_u/v/w, level and solid must all be wired");
            return;
        };
        let emitters = ctx.scalar_or_param("count", 0.0).round().max(0.0) as u32;
        let inputs = CaptureInputs {
            spawns,
            offsets: ctx.inputs.array("offsets").map(|offsets| (offsets, emitters)),
            faces: [face_u, face_v, face_w],
            level,
            solid,
        };
        let offline = offline_simulation();
        let gpu = ctx.gpu_encoder();
        let clock = gpu.device.frame_clock();
        let fence: &dyn Fence = match &clock {
            Some(clock) => clock,
            None => &Retired,
        };
        let report = self.advance(gpu, fence, offline, &frame, &inputs);
        for (name, count) in ["foam_count", "bubble_count", "spray_count"].into_iter().zip(report.counts) {
            ctx.outputs.set_scalar(name, ParamValue::Float(count as f32));
        }
        ctx.outputs.set_scalar("emitted", ParamValue::Float(report.emitted as f32));
        ctx.outputs.set_scalar("thinned", ParamValue::Float(report.thinned as f32));
        ctx.outputs.set_scalar("dropped_ticks", ParamValue::Float(report.dropped_ticks as f32));
        ctx.outputs.set_scalar("lifecycle_ms", ParamValue::Float(report.lifecycle_ms as f32));
        if let Some(failure) = report.failure {
            ctx.error(format!("Whitewater: {failure}"));
        }
    }
}
