//! `node.whitewater_lifecycle` — FLIP's own whitewater lifecycle for any GPU
//! liquid (`docs/GPU_WHITEWATER_DESIGN.md` D1, D6–D8, D10–D12, section 3.5).
//! Each frame with ticks it snapshots the spawns and fields on the GPU; a
//! later frame, once that one retired, loans them to the lifecycle's own
//! thread, which runs the vendored `DiffuseParticleSimulation` and writes
//! the population into three particle frames. Live never waits on the GPU
//! or the worker; offline waits for both, so an export is the same at any
//! speed.

use std::borrow::Cow;

use manifold_fluids::{WhitewaterGrid, WhitewaterSpawn};
use manifold_gpu::GpuBuffer;

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::offline_simulation;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{face_offset, grid_box, grid_cells, require_extended_faces};
use crate::node_graph::whitewater_handoff::{
    CaptureInputs, Fence, OutputRing, Reply, Request, Reset, Retired, SNAPSHOT_SLOTS, Snapshot, SnapshotRing, SnapshotShape,
    Worker,
};

/// FLIP's own default whitewater budget.
pub const DEFAULT_CAPACITY: u32 = 100_000;
pub const MAX_CAPACITY: u32 = 250_000;

const OUTPUTS: [&str; 3] = ["foam_particles", "bubble_particles", "spray_particles"];

crate::primitive! {
    name: WhitewaterLifecycle,
    type_id: "node.whitewater_lifecycle",
    purpose: "Advance whitewater with FLIP's own lifecycle: spray falls and bounces, bubbles rise and drag, foam rides the surface, each ages and dies as FLIP's native whitewater does. Spawns and the liquid's faces, distance and solid are copied on the GPU each frame with ticks and handed to the lifecycle's own thread once that frame finished, so the population trails the water by one frame offline and two or three live. Out come foam, bubbles and spray as particle frames, each particle's radius its fade, with their counts. A new epoch clears everything; ticks 0 holds it. Past Capacity, spawns are thinned evenly and counted.",
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
        worker_ms: ScalarF32,
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
    composition_notes: "spawns from the whitewater spawn atoms, offsets and count from the running total over their emission counts (its last entry is the frame's emitted count). face_u/v/w, face_cells_x/y/z and face_valid_layers from the liquid frame's face grid (at least one valid layer), solid, grid_bounds and grid_nodes_x/y/z from the particle frame, level from node.crossing_distance, ticks, epoch and gravity_x/gravity/gravity_z from the liquid domain. The face grid must sit centred on the solid lattice's cells by a whole number of cells. Draw each particles output with node.particles_to_copies, its count wired to live_count. emitted, thinned and dropped_ticks count since the epoch began; dropped_ticks grows only live, when the GPU is three frames behind. lifecycle_ms is this frame's time in the node on the content thread; worker_ms is the lifecycle thread's time on the last work it finished.",
    examples: [],
    picker: { label: "Whitewater Lifecycle", category: Atom },
    summary: "Moves and ages spray, foam and bubbles the way FLIP's own whitewater does.",
    category: Particles3D,
    role: Filter,
    aliases: ["whitewater", "foam", "spray", "bubbles", "diffuse particles"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        handoff: Handoff = Handoff::default(),
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
    /// Snapshots captured and not yet loaned to the worker.
    pub pending: usize,
    pub lifecycle_ms: f64,
    pub worker_ms: f64,
    pub failure: Option<String>,
}

/// The content thread's side of the lifecycle: the rings, the worker, and
/// what is on loan to it.
pub(crate) struct Handoff {
    worker: Option<Worker>,
    /// A request is on the worker and its reply has not come back.
    busy: bool,
    /// Snapshot slots, and whether an output slot, ride in that request.
    loaned: (usize, bool),
    /// The next request's snapshot list; inside the request while one is in
    /// flight, so no frame allocates one.
    loan: Option<Vec<Snapshot>>,
    /// The grid and capacity the worker's lifecycle is on.
    lifecycle: Option<(WhitewaterGrid, u32)>,
    epoch: Option<u32>,
    /// Owed to the worker before any later snapshot.
    reset: Option<Reset>,
    /// The reply in flight is for an epoch or grid since replaced.
    stale: bool,
    snapshots: SnapshotRing,
    outputs: OutputRing,
    /// The population changed since the worker last wrote it out.
    owed: bool,
    emitted: u64,
    thinned: u64,
    dropped_ticks: u64,
    worker_ms: f64,
    failure: Option<String>,
    /// Tests only: the next request's worker waits for this.
    #[cfg(all(test, feature = "gpu-proofs"))]
    hold: Option<std::sync::mpsc::Receiver<()>>,
}

impl Default for Handoff {
    fn default() -> Self {
        Self {
            worker: None,
            busy: false,
            loaned: (0, false),
            loan: Some(Vec::with_capacity(SNAPSHOT_SLOTS)),
            lifecycle: None,
            epoch: None,
            reset: None,
            stale: false,
            snapshots: SnapshotRing::default(),
            outputs: OutputRing::default(),
            owed: true,
            emitted: 0,
            thinned: 0,
            dropped_ticks: 0,
            worker_ms: 0.0,
            failure: None,
            #[cfg(all(test, feature = "gpu-proofs"))]
            hold: None,
        }
    }
}

impl Handoff {
    /// A new grid, capacity or epoch starts the population over before
    /// anything loads (D12, I6): the worker is told first, the outputs go
    /// empty now, and a reply still in flight is for the old one.
    pub(crate) fn prepare(&mut self, frame: &Frame) {
        let lifecycle = (frame.shape.grid, frame.shape.capacity);
        if self.lifecycle == Some(lifecycle) && self.epoch == Some(frame.epoch) {
            return;
        }
        self.lifecycle = Some(lifecycle);
        self.epoch = Some(frame.epoch);
        self.reset = Some(Reset { grid: frame.shape.grid, capacity: frame.shape.capacity, epoch: frame.epoch });
        self.stale = self.busy;
        self.outputs.clear();
        self.owed = true;
        self.emitted = 0;
        self.thinned = 0;
        self.dropped_ticks = 0;
        self.failure = None;
    }

    /// Take the worker's reply if there is one; offline, wait for it.
    /// True when the population outgrew the slot it was loaned.
    fn collect(&mut self, block: bool) -> Result<bool, String> {
        if !self.busy {
            return Ok(false);
        }
        let worker = self.worker.as_ref().expect("a request in flight has a worker");
        let reply = if block { worker.reply().map(Some) } else { worker.try_reply() };
        match reply {
            Ok(Some(reply)) => Ok(self.accept(reply)),
            Ok(None) => Ok(false),
            Err(stopped) => {
                self.lost_worker();
                Err(stopped)
            }
        }
    }

    fn accept(&mut self, mut reply: Reply) -> bool {
        self.busy = false;
        self.loaned = (0, false);
        self.snapshots.give_back(&mut reply.snapshots);
        self.loan = Some(reply.snapshots);
        let stale = std::mem::take(&mut self.stale);
        if let Some(slot) = reply.output {
            if reply.written && !stale {
                self.outputs.accept(slot);
                self.owed = false;
            } else {
                self.outputs.give_back(slot);
            }
        }
        if let Some(records) = reply.needs {
            self.outputs.grow(records);
        }
        self.worker_ms = reply.worker_ms;
        if stale {
            return false;
        }
        self.emitted += reply.emitted;
        self.thinned += reply.thinned;
        self.failure = reply.failure;
        reply.needs.is_some()
    }

    /// The worker thread ended with the lifecycle and the slots it held; the
    /// next frame starts a new one over.
    fn lost_worker(&mut self) {
        self.worker = None;
        self.busy = false;
        self.stale = false;
        self.snapshots.lose(self.loaned.0);
        if self.loaned.1 {
            self.outputs.lose();
        }
        self.loaned = (0, false);
        self.loan = Some(Vec::with_capacity(SNAPSHOT_SLOTS));
        self.lifecycle = None;
    }

    /// Loan the worker what is ready: the reset owed, every retired snapshot
    /// and, when the population changed, an output slot.
    fn send(&mut self, gpu: &GpuEncoder<'_>, fence: &dyn Fence, offline: bool, frame: &Frame) -> Result<(), String> {
        let mut loan = self.loan.take().expect("the snapshot list is home while no request is in flight");
        let waited = self.snapshots.take_retired(fence, offline, frame.epoch, frame.shape, &mut loan);
        let reset = self.reset.take();
        self.owed |= reset.is_some() || !loan.is_empty();
        // Without a slot the worker still steps; the population goes out on
        // a later frame, as when every slot is still being read.
        let (output, refused) = match self.owed.then(|| self.outputs.loan(gpu.device, fence)).transpose() {
            Ok(output) => (output.flatten(), None),
            Err(error) => (None, Some(error)),
        };
        if reset.is_none() && loan.is_empty() && output.is_none() {
            self.loan = Some(loan);
            return waited.and(refused.map_or(Ok(()), Err));
        }
        self.loaned = (loan.len(), output.is_some());
        let worker = self.worker.as_ref().expect("send runs with a worker");
        let request = Request {
            reset,
            snapshots: loan,
            output,
            #[cfg(all(test, feature = "gpu-proofs"))]
            hold: self.hold.take(),
        };
        if let Err(stopped) = worker.send(request) {
            self.lost_worker();
            return Err(stopped);
        }
        self.busy = true;
        waited.and(refused.map_or(Ok(()), Err))
    }

    /// One frame after [`Self::prepare`]: take the finished work, loan the
    /// ready work, then capture this frame's (section 3.5). Offline waits for
    /// the worker, so the population is the one live would reach.
    pub(crate) fn advance(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        fence: &dyn Fence,
        offline: bool,
        frame: &Frame,
        inputs: &CaptureInputs<'_>,
    ) -> Report {
        let start = std::time::Instant::now();
        let mut failure = self.hand_over(gpu, fence, offline, frame).err();
        self.outputs.mark_read(fence);
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
            lifecycle_ms: start.elapsed().as_secs_f64() * 1000.0,
            worker_ms: self.worker_ms,
            failure: failure.or_else(|| self.failure.clone()),
        }
    }

    fn hand_over(&mut self, gpu: &GpuEncoder<'_>, fence: &dyn Fence, offline: bool, frame: &Frame) -> Result<(), String> {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn()?);
        }
        self.collect(offline)?;
        if self.busy {
            return Ok(());
        }
        let sent = self.send(gpu, fence, offline, frame);
        if offline && self.collect(true)? {
            // The population outgrew its slot: loan a grown one this frame.
            self.send(gpu, fence, offline, frame)?;
            self.collect(true)?;
        }
        sent
    }

    /// Tests only: wait for the worker and take its reply, as the next frame
    /// would once the worker finished.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn settle(&mut self) {
        self.collect(true).expect("the whitewater worker replies");
    }

    /// Tests only: the next request's worker starts once the returned sender
    /// sends or drops.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn hold_next(&mut self) -> std::sync::mpsc::SyncSender<()> {
        let (release, hold) = std::sync::mpsc::sync_channel(1);
        self.hold = Some(hold);
        release
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn outputs(&self) -> &OutputRing {
        &self.outputs
    }
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
}

impl Primitive for WhitewaterLifecycle {
    fn provides_array_output(&self, port: &str) -> bool {
        OUTPUTS.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let population = OUTPUTS.iter().position(|&name| name == port)?;
        self.handoff.outputs.current().map(|slot| &slot.buffers[population])
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
        let frame = match Self::frame(ctx) {
            Ok(frame) => frame,
            Err(refusal) => {
                ctx.error(format!("Whitewater: {refusal}"));
                return;
            }
        };
        self.handoff.prepare(&frame);
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
        let report = self.handoff.advance(gpu, fence, offline, &frame, &inputs);
        for (name, count) in ["foam_count", "bubble_count", "spray_count"].into_iter().zip(report.counts) {
            ctx.outputs.set_scalar(name, ParamValue::Float(count as f32));
        }
        ctx.outputs.set_scalar("emitted", ParamValue::Float(report.emitted as f32));
        ctx.outputs.set_scalar("thinned", ParamValue::Float(report.thinned as f32));
        ctx.outputs.set_scalar("dropped_ticks", ParamValue::Float(report.dropped_ticks as f32));
        ctx.outputs.set_scalar("lifecycle_ms", ParamValue::Float(report.lifecycle_ms as f32));
        ctx.outputs.set_scalar("worker_ms", ParamValue::Float(report.worker_ms as f32));
        if let Some(failure) = report.failure {
            ctx.error(format!("Whitewater: {failure}"));
        }
    }
}
