//! Forces and impulses for every GPU liquid domain
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P8; GPU_MPM_SOLVER_DESIGN.md D13).
//!
//! A domain calls the same things whatever its solver:
//! - [`LiquidImpulses`] keeps scene impulses on the domain's fixed-tick clock.
//!   Call [`LiquidImpulses::observe_frame`] right after `LiquidClock::advance`,
//!   and [`LiquidImpulses::commit_frame`] once the frame's ticks are encoded;
//!   the impulse hooks call `stamp`, `enqueue`, `drain_applied` and
//!   `drain_discarded`.
//! - The scene's acceleration field is evaluated at every tick's start, never
//!   per display frame: before each frame the host's physics history replay
//!   asks [`LiquidFields::request_samples`] for the transport times the
//!   coming ticks start at, runs the field's authored ancestry at exactly
//!   those times, and hands each result to [`LiquidFields::observe_sample`].
//!   [`LiquidFields::prepare`] then lays each tick's field onto a coarse
//!   [`FieldLattice`], a quarter of the solver's resolution per axis: one
//!   lattice while the ticks agree, one per tick otherwise, so a tick reads
//!   the same forces at any frame rate. A tick nobody sampled is an error.
//!   This frame's impulses go onto one more lattice.
//! - [`LiquidFields::upload`] copies what changed into the stable `forces` and
//!   `impulses` buffers in encoder order. The solver's atoms read them with
//!   `LIQUID_FIELD` (`shaders/liquid_field.wgsl`): forces every substep from
//!   their tick's lattice, impulses once, on the first substep of
//!   [`FieldFrame::impulse_tick`].

use manifold_core::Seconds;
use manifold_gpu::{FrameClock, GpuBuffer};
use std::collections::VecDeque;

use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{FieldValue, TickStamp, VectorField};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::TICK;
use crate::node_graph::liquid::clock::{ClockFrame, LiquidClock, MAX_LIVE_TICKS};
use crate::node_graph::liquid::coupling::LiquidRigidOwner;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::physics::ResolvedRigidImpulse;
use crate::node_graph::physics_events::{ResolvedNodeImpulse, map_rigid_receipt};

/// Trilinear reads of a field lattice; pure math, included by each atom that
/// reads one (its own buffer reads stay in its body).
pub(crate) const LIQUID_FIELD: &str = include_str!("../primitives/shaders/liquid_field.wgsl");

/// Impulses held at once: queued, discarded and undrained receipts together.
pub const IMPULSE_CAPACITY: usize = 256;
/// Solver cells per field cell along each axis (D13).
pub const FIELD_STRIDE: u32 = 4;
/// Staging slots: one per frame that writes, three frames in flight plus one.
pub const STAGING_SLOTS: usize = 4;

/// The coarse lattice forces and impulses are sampled on. It starts at the
/// solver lattice's min corner and covers its last node, so a solver node
/// always sits inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldLattice {
    min: [f32; 3],
    spacing: f32,
    nodes: [u32; 3],
}

impl FieldLattice {
    pub fn covering(min: [f32; 3], cell_size: f32, solver_nodes: [u32; 3]) -> Self {
        Self {
            min,
            spacing: cell_size * FIELD_STRIDE as f32,
            nodes: solver_nodes.map(|n| n.saturating_sub(1).div_ceil(FIELD_STRIDE).max(1) + 1),
        }
    }

    pub fn of(lattice: &LiquidLattice) -> Self {
        Self::covering(lattice.min(), lattice.cell_size(), lattice.nodes())
    }

    pub fn nodes(&self) -> [u32; 3] {
        self.nodes
    }

    pub fn spacing(&self) -> f32 {
        self.spacing
    }

    pub fn node_count(&self) -> usize {
        self.nodes.iter().map(|&n| n as usize).product()
    }

    /// Bytes of one lattice: four floats per node.
    pub fn bytes(&self) -> u64 {
        self.node_count() as u64 * 16
    }

    fn position(&self, index: usize) -> [f32; 3] {
        let [nx, ny, _] = self.nodes.map(|n| n as usize);
        let coord = [index % nx, (index / nx) % ny, index / (nx * ny)];
        std::array::from_fn(|d| self.min[d] + coord[d] as f32 * self.spacing)
    }

    /// The CPU twin of `liquid_field_corner` summed over its eight corners.
    pub fn sample(&self, values: &[[f32; 4]], x: [f32; 3]) -> [f32; 3] {
        let mut sum = [0.0f32; 3];
        for k in 0..8u32 {
            let mut index = 0usize;
            let mut stride = 1usize;
            let mut weight = 1.0f32;
            for (d, ((&x, &min), &nodes)) in x.iter().zip(&self.min).zip(&self.nodes).enumerate() {
                let g = (x - min) / self.spacing;
                let base = g.floor().max(0.0).min((nodes - 2) as f32);
                let f = (g - base).clamp(0.0, 1.0);
                let o = (k >> d) & 1;
                weight *= if o == 1 { f } else { 1.0 - f };
                index += (base as usize + o as usize) * stride;
                stride *= nodes as usize;
            }
            for (axis, total) in sum.iter_mut().enumerate() {
                *total += values[index][axis] * weight;
            }
        }
        sum
    }
}

/// What a domain publishes for its atoms about this frame's fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldFrame {
    pub lattice: FieldLattice,
    /// Force lattices in `forces`: 0 without a field, 1 while it holds still
    /// (every tick reads it), else one per tick from the frame's first tick.
    pub force_lattices: u32,
    /// The tick (counted in the epoch) whose first substep adds the impulses.
    pub impulse_tick: Option<u64>,
}

impl FieldFrame {
    /// The scalar outputs, by name. A frame without impulses publishes −1.
    pub fn outputs(&self) -> [(&'static str, f32); 6] {
        let [x, y, z] = self.lattice.nodes.map(|n| n as f32);
        [
            ("field_nodes_x", x),
            ("field_nodes_y", y),
            ("field_nodes_z", z),
            ("field_spacing", self.lattice.spacing),
            ("force_lattices", self.force_lattices as f32),
            ("impulse_tick", self.impulse_tick.map_or(-1.0, |tick| tick as f32)),
        ]
    }
}

/// The field scalars and lattices an atom binds: wired lattices too small
/// for the wired field are refused by name; unwired ones read nothing.
pub(crate) struct FieldBinding<'a> {
    pub nodes: [i32; 3],
    pub spacing: f32,
    pub force_lattices: i32,
    pub impulse_tick: i32,
    pub first_tick: i32,
    pub forces: Option<&'a GpuBuffer>,
    pub impulses: Option<&'a GpuBuffer>,
}

impl<'a> FieldBinding<'a> {
    pub(crate) fn read(
        ctx: &EffectNodeContext<'_, '_>,
        forces: Option<&'a GpuBuffer>,
        impulses: Option<&'a GpuBuffer>,
        atom: &str,
    ) -> Result<Self, String> {
        let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"]
            .map(|name| ctx.scalar_or_param(name, 2.0).round().max(2.0) as i32);
        let spacing = ctx.scalar_or_param("field_spacing", 0.25);
        let force_lattices =
            if forces.is_some() { ctx.scalar_or_param("force_lattices", 0.0).round().max(0.0) as i32 } else { 0 };
        let impulse_tick = if impulses.is_some() { ctx.scalar_or_param("impulse_tick", -1.0).round().max(-1.0) as i32 } else { -1 };
        let first_tick = ctx.scalar_or_param("first_tick", 0.0).round().max(0.0) as i32;
        let lattice_bytes = nodes.iter().map(|&n| n as u64).product::<u64>() * 16;
        for (name, buffer, lattices) in
            [("forces", forces, force_lattices.max(0) as u64), ("impulses", impulses, u64::from(impulse_tick >= 0))]
        {
            if lattices > 0 && buffer.is_some_and(|buffer| buffer.size < lattices * lattice_bytes) {
                return Err(format!(
                    "{atom}: the {name} buffer holds fewer than {lattices} lattice(s) of {} × {} × {} field nodes; wire the liquid domain's {name} and field scalars",
                    nodes[0], nodes[1], nodes[2]
                ));
            }
        }
        if (force_lattices > 0 || impulse_tick >= 0) && !(spacing.is_finite() && spacing > 0.0) {
            return Err(format!("{atom}: field_spacing must be positive"));
        }
        Ok(Self { nodes, spacing, force_lattices, impulse_tick, first_tick, forces, impulses })
    }
}

/// The first tick a frame runs, counted in its epoch.
pub fn first_tick(frame: &ClockFrame) -> u64 {
    (frame.simulation_time / TICK).round() as u64 - u64::from(frame.ticks)
}

/// Scene impulses on a liquid's fixed-tick clock. A hit fired after a frame
/// is stamped at that frame's simulated time, so it lands on the first tick
/// the next frame runs, once. While the clock is held (pause, Speed 0) a hit
/// is discarded with a receipt, so resume never replays it.
///
/// Rigid targets of a coupled domain go to its rigid owner's queue under the
/// same sequence; the liquid owns the pair's clock, so a held liquid discards
/// them too.
///
/// A hit reads "applied" only through [`Self::commit_frame`], which the domain
/// calls once its frame's ticks are encoded. A frame that fails first never
/// commits: its hits are discarded with a receipt at the next drain or frame.
#[derive(Default)]
pub struct LiquidImpulses {
    queue: Option<EventQueue<ResolvedNodeImpulse>>,
    /// Counts restarts; never wraps in practice, unlike the clock's u32.
    epoch: u64,
    held: bool,
    /// Transport of the frame last observed: stamps are taken only there.
    transport: Option<f64>,
    simulation_time: f64,
    last_sequence: Option<u64>,
    /// This frame's hits, taken by its begun ticks, until the frame commits.
    pending: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    /// Receipts of committed frames, undrained.
    applied: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    impulse_tick: Option<u64>,
    discarded: Vec<EventStamp>,
    /// Sequences sent to both queues: the liquid receipt names them, so the
    /// rigid receipt is dropped.
    combined: Vec<u64>,
    failure: Option<String>,
}

impl LiquidImpulses {
    /// Begin this frame's ticks. A restart opens a new epoch and cancels every
    /// unread event of the old one.
    pub fn observe_frame(&mut self, transport: f64, frame: &ClockFrame) -> Result<(), String> {
        if frame.restarted {
            self.epoch += 1;
            match &mut self.queue {
                Some(queue) => {
                    queue
                        .reset(self.epoch, Seconds::ZERO)
                        .map_err(|error| format!("Liquid impulses: {error}"))?;
                }
                None => {
                    self.queue = Some(
                        EventQueue::new(self.epoch, Seconds::ZERO, Seconds(TICK), IMPULSE_CAPACITY)
                            .map_err(|error| format!("Liquid impulses: {error}"))?,
                    );
                }
            }
            self.pending.clear();
            self.applied.clear();
            self.discarded.clear();
            self.combined.clear();
            self.last_sequence = None;
            self.failure = None;
        }
        self.abandon_frame();
        self.held = frame.held;
        self.transport = Some(transport);
        self.simulation_time = frame.simulation_time;
        self.impulse_tick = None;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let queue = self.queue.as_mut().ok_or("Liquid impulses: the clock never started")?;
        let first = first_tick(frame);
        if queue.next_tick().tick != first {
            let error = format!(
                "Liquid impulses: tick {first} does not follow tick {}; restart the simulation",
                queue.next_tick().tick
            );
            self.failure = Some(error.clone());
            return Err(error);
        }
        let pending = &mut self.pending;
        for tick in first..first + u64::from(frame.ticks) {
            if let Err(error) = queue.begin_tick(TickStamp { epoch: self.epoch, tick }, |event| pending.push(event)) {
                let error = format!("Liquid impulses: {error}");
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        // One impulse lattice per frame: every hit of the frame shares a tick.
        let mut ticks = self.pending.iter().map(|event| event.applied.tick);
        self.impulse_tick = ticks.next();
        if ticks.any(|tick| Some(tick) != self.impulse_tick) {
            let error = "Liquid impulses: hits for two ticks of one frame; one frame applies one impulse tick".to_string();
            self.failure = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    /// This frame's ticks are encoded: its hits are applied.
    pub fn commit_frame(&mut self) {
        self.applied.append(&mut self.pending);
    }

    /// This frame's ticks will not run: its hits are discarded, with a receipt.
    pub fn abandon_frame(&mut self) {
        self.discarded.extend(self.pending.drain(..).map(|event| event.source));
    }

    /// The epoch stamps must carry, once the clock has started.
    pub fn epoch(&self) -> Option<u64> {
        self.queue.as_ref().map(|_| self.epoch)
    }

    /// Stamp a hit at the simulated time of the frame last observed.
    pub fn stamp(&self, transport: f64, sequence: u64) -> Result<EventStamp, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.queue.is_none() {
            return Err("Liquid impulses: the clock has not started".into());
        }
        if self.transport != Some(transport) {
            return Err("Liquid impulses: an impulse must be captured at the frame the liquid last ran".into());
        }
        Ok(EventStamp {
            epoch: self.epoch,
            time: Seconds(self.simulation_time),
            sequence,
        })
    }

    /// Admit one resolved hit. Rigid targets need the coupled rigid owner. A
    /// held clock discards the hit (a receipt in `drain_discarded`) and still
    /// succeeds, so the producer rearms.
    pub fn enqueue(
        &mut self,
        stamp: EventStamp,
        impulse: ResolvedNodeImpulse,
        rigid: Option<&mut LiquidRigidOwner>,
    ) -> Result<TickStamp, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let Some(queue) = self.queue.as_ref() else {
            return Err("Liquid impulses: the clock has not started".into());
        };
        if stamp.epoch != self.epoch {
            return Err("Liquid impulses: the hit belongs to a restarted simulation".into());
        }
        if self.last_sequence.is_some_and(|last| stamp.sequence <= last) {
            return Err("Liquid impulses: hit sequences must increase".into());
        }
        let rigid = match (impulse.target.rigid_targets(), rigid) {
            (Some(targets), Some(owner)) => Some((targets, owner)),
            (Some(_), None) => return Err("Liquid impulses: rigid targets need a coupled rigid world".into()),
            (None, _) => None,
        };
        let outstanding =
            queue.len() + self.pending.len() + self.applied.len() + self.discarded.len() + self.combined.len();
        if outstanding >= IMPULSE_CAPACITY {
            let error = "Liquid impulses: impulse history is full; restart the simulation or bake the scene";
            self.failure = Some(error.into());
            return Err(error.into());
        }
        let next = queue.next_tick();
        if self.held {
            self.last_sequence = Some(stamp.sequence);
            self.discarded.push(stamp);
            return Ok(next);
        }
        let mut planned = None;
        if let Some((targets, owner)) = rigid {
            let rigid_stamp = owner.rigid().impulse_stamp(Seconds(owner.completed() as f64 * TICK), stamp.sequence)?;
            planned = Some(owner.rigid_mut().enqueue_impulse(
                rigid_stamp,
                ResolvedRigidImpulse { field: impulse.field.clone(), targets },
            )?);
        }
        self.last_sequence = Some(stamp.sequence);
        if !impulse.target.affects_fluid() {
            return Ok(planned.expect("a rigid-only hit was sent to the rigid owner"));
        }
        if planned.is_some() {
            self.combined.push(stamp.sequence);
        }
        self.queue
            .as_mut()
            .expect("checked above")
            .enqueue(stamp, impulse)
            .map_err(|error| {
                let error = format!("Liquid impulses: {error}");
                self.failure = Some(error.clone());
                error
            })
    }

    /// Receipts of committed frames: the liquid's, then the rigid owner's that
    /// the liquid's did not already name. A frame drained uncommitted failed.
    pub fn drain_applied(
        &mut self,
        consume: &mut dyn FnMut(AppliedEvent<ResolvedNodeImpulse>),
        rigid: Option<&mut LiquidRigidOwner>,
    ) {
        self.abandon_frame();
        for event in self.applied.drain(..) {
            consume(event);
        }
        let Some(owner) = rigid else { return };
        let combined = &mut self.combined;
        for event in owner.rigid_mut().drain_applied_impulses() {
            if let Some(index) = combined.iter().position(|&sequence| sequence == event.source.sequence) {
                combined.swap_remove(index);
                continue;
            }
            map_rigid_receipt(event, &mut *consume);
        }
    }

    /// Stamps of hits discarded while the clock was held or by a failed frame.
    pub fn drain_discarded(&mut self, consume: &mut dyn FnMut(EventStamp)) {
        self.abandon_frame();
        for stamp in self.discarded.drain(..) {
            consume(stamp);
        }
    }

    /// The tick this frame's hits land on, if any.
    pub fn impulse_tick(&self) -> Option<u64> {
        self.impulse_tick
    }

    /// This frame's hits on the liquid.
    fn frame_events(&self) -> &[AppliedEvent<ResolvedNodeImpulse>] {
        &self.pending
    }
}

/// One staging slot: the impulse lattice then the force lattices, written by
/// the CPU, copied into the stable buffers by the GPU.
struct StagingSlot {
    buffer: GpuBuffer,
    /// Frame-clock stamp of the frame that copied from it; 0 never read.
    read_stamp: u64,
}

struct FieldBuffers {
    lattice: FieldLattice,
    /// Force lattices the buffers hold room for.
    force_capacity: usize,
    forces: GpuBuffer,
    impulses: GpuBuffer,
    staging: Vec<StagingSlot>,
}

const NOT_FINITE: &str = "Liquid forces: the acceleration field is not finite inside the domain";

/// The CPU lattices and their GPU copies. CPU storage grows with the lattice
/// and the ticks of one frame; a still field is resampled only when it
/// changes.
#[derive(Default)]
pub struct LiquidFields {
    lattice: Option<FieldLattice>,
    /// `force_lattices` lattices back to back.
    forces: Vec<[f32; 4]>,
    force_lattices: usize,
    impulses: Vec<[f32; 4]>,
    /// The field every lattice holds while it holds still.
    force_source: Option<FieldValue>,
    /// The field at each tick's start, ascending by tick, from the oldest
    /// tick not yet run.
    tick_fields: VecDeque<(u64, FieldValue)>,
    /// Transport times the history replay must sample before the next frame,
    /// with their ticks, ascending.
    requests: Vec<(f64, u64)>,
    forces_dirty: bool,
    impulses_dirty: bool,
    clock: Option<FrameClock>,
    gpu: Option<FieldBuffers>,
}

impl LiquidFields {
    /// Lay each of the frame's ticks' fields onto its lattice and sample the
    /// frame's hits. Call right after `clock` advanced to `frame`; `field` is
    /// this frame's, which belongs to any tick starting exactly now.
    pub fn prepare(
        &mut self,
        lattice: FieldLattice,
        field: Option<&FieldValue>,
        clock: &LiquidClock,
        frame: &ClockFrame,
        impulses: &LiquidImpulses,
    ) -> Result<FieldFrame, String> {
        let count = lattice.node_count();
        if self.lattice != Some(lattice) {
            self.forces.clear();
            self.force_lattices = 0;
            self.impulses.clear();
            self.impulses.resize(count, [0.0; 4]);
            self.lattice = Some(lattice);
            self.force_source = None;
            self.impulses_dirty = true;
        }
        let first = first_tick(frame);
        if frame.restarted {
            self.tick_fields.clear();
        }
        let Some(field) = field else {
            // No field wired: no tick reads forces.
            self.tick_fields.clear();
            self.force_lattices = 0;
            self.force_source = None;
            return self.finish(lattice, impulses);
        };
        // This frame's controls belong to a tick that starts exactly now; every
        // other start was or will be sampled by the history replay. A live drop
        // moves the owed tick's start into the past, where nothing sampled it,
        // so it reads this frame: a tick late, never a stale pre-drop value.
        let dropped = frame.dropped_seconds > 0.0;
        let mut next = clock.ticks_done();
        while let Some(start) = clock.tick_start(next).filter(|&start| start <= clock.transport()) {
            if dropped || start == clock.transport() {
                self.record_tick(next, field);
            }
            next += 1;
        }
        let ticks = frame.ticks as usize;
        let start = self.tick_fields.partition_point(|(recorded, _)| *recorded < first);
        for offset in 0..ticks {
            let tick = first + offset as u64;
            if self.tick_fields.get(start + offset).is_none_or(|(recorded, _)| *recorded != tick) {
                return Err(format!(
                    "Liquid forces: tick {tick} was never sampled; the host must replay physics history before each frame"
                ));
            }
        }
        let sampled = self.tick_fields.range(start..start + ticks).map(|(_, at)| at);
        let head = self.tick_fields.get(start).filter(|_| ticks > 0).map(|(_, at)| at);
        if let Some(head) = head.filter(|head| sampled.clone().all(|at| at == *head)) {
            if self.force_lattices != 1 || self.force_source.as_ref() != Some(head) {
                self.forces.resize(count, [0.0; 4]);
                fill(&lattice, &mut self.forces, |x| head.sample(x)).map_err(|_| NOT_FINITE.to_string())?;
                self.force_lattices = 1;
                self.force_source = Some(head.clone());
                self.forces_dirty = true;
            }
        } else if ticks > 0 {
            // A tick reads the field at its own start, so the lattices match
            // at any frame rate.
            self.forces.resize(ticks * count, [0.0; 4]);
            for (values, at) in self.forces.chunks_exact_mut(count).zip(sampled) {
                fill(&lattice, values, |x| at.sample(x)).map_err(|_| NOT_FINITE.to_string())?;
            }
            self.force_lattices = ticks;
            self.force_source = None;
            self.forces_dirty = true;
        }
        // Keep only ticks not yet run.
        let end = first + ticks as u64;
        while self.tick_fields.front().is_some_and(|(tick, _)| *tick < end) {
            self.tick_fields.pop_front();
        }
        self.finish(lattice, impulses)
    }

    /// The transport times the history replay must sample before the next
    /// frame: each tick's start under `clock`, in `(from, until]`. Replaces
    /// any earlier requests.
    pub fn request_samples(&mut self, clock: &LiquidClock, from: f64, until: f64, out: &mut Vec<f64>) {
        self.requests.clear();
        let requests = &mut self.requests;
        clock.tick_starts(from, until, |transport, tick| {
            requests.push((transport, tick));
            out.push(transport);
        });
    }

    /// A history replay sample at transport `now`: record `field` for every
    /// requested tick whose start has been reached. The replay samples each
    /// request exactly; the closing sample takes one rounding past the frame.
    pub fn observe_sample(&mut self, now: f64, field: Option<&FieldValue>) {
        let reached = self.requests.partition_point(|&(transport, _)| transport <= now);
        if reached == 0 {
            return;
        }
        let mut requests = std::mem::take(&mut self.requests);
        if let Some(field) = field {
            for &(_, tick) in &requests[..reached] {
                self.record_tick(tick, field);
            }
        }
        requests.drain(..reached);
        self.requests = requests;
    }

    /// Later samples of a tick replace earlier ones: a live drop moves a tick
    /// to a later transport time.
    fn record_tick(&mut self, tick: u64, field: &FieldValue) {
        let at = self.tick_fields.partition_point(|(recorded, _)| *recorded < tick);
        match self.tick_fields.get_mut(at) {
            Some((recorded, value)) if *recorded == tick => value.clone_from(field),
            _ => self.tick_fields.insert(at, (tick, field.clone())),
        }
    }

    fn finish(&mut self, lattice: FieldLattice, impulses: &LiquidImpulses) -> Result<FieldFrame, String> {
        let impulse_tick = impulses.impulse_tick();
        if impulse_tick.is_some() {
            let events = impulses.frame_events();
            fill(&lattice, &mut self.impulses, |x| {
                let mut sum = [0.0f32; 3];
                for event in events.iter().filter(|event| event.value.target.affects_fluid()) {
                    let v = event.value.field.sample(x);
                    for axis in 0..3 {
                        sum[axis] += v[axis];
                    }
                }
                sum
            })
            .map_err(|_| "Liquid impulses: the impulse field is not finite inside the domain".to_string())?;
            self.impulses_dirty = true;
        }
        Ok(FieldFrame {
            lattice,
            force_lattices: self.force_lattices as u32,
            impulse_tick,
        })
    }

    /// Every force lattice, back to back.
    pub fn forces(&self) -> &[[f32; 4]] {
        &self.forces[..self.force_lattices * self.lattice.map_or(0, |lattice| lattice.node_count())]
    }

    pub fn impulses(&self) -> &[[f32; 4]] {
        &self.impulses
    }

    pub fn forces_buffer(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.forces)
    }

    pub fn impulses_buffer(&self) -> Option<&GpuBuffer> {
        self.gpu.as_ref().map(|gpu| &gpu.impulses)
    }

    /// Copy what changed into the stable buffers, in encoder order. Live never
    /// waits for a staging slot; offline waits for the oldest.
    pub fn upload(&mut self, gpu: &mut crate::gpu_encoder::GpuEncoder<'_>, offline: bool) -> Result<(), String> {
        let Some(lattice) = self.lattice else { return Ok(()) };
        if self.clock.is_none() {
            self.clock = gpu.device.frame_clock();
        }
        let bytes = lattice.bytes();
        let force_bytes = self.force_lattices as u64 * bytes;
        if self
            .gpu
            .as_ref()
            .is_none_or(|buffers| buffers.lattice != lattice || buffers.force_capacity < self.force_lattices)
        {
            // Room for a live frame's ticks up front; offline catch-up grows it.
            let force_capacity = self.force_lattices.max(MAX_LIVE_TICKS as usize);
            let create = |what: &str, size: u64| {
                gpu.device
                    .try_create_buffer_shared(size)
                    .map_err(|error| format!("Liquid fields: the {what} need {size} bytes: {error}"))
            };
            let forces = create("forces", force_capacity as u64 * bytes)?;
            let impulses = create("impulses", bytes)?;
            forces.zero_fill();
            impulses.zero_fill();
            let staging = (0..STAGING_SLOTS)
                .map(|_| {
                    create("staging lattices", (1 + force_capacity as u64) * bytes)
                        .map(|buffer| StagingSlot { buffer, read_stamp: 0 })
                })
                .collect::<Result<_, _>>()?;
            self.gpu = Some(FieldBuffers { lattice, force_capacity, forces, impulses, staging });
            self.forces_dirty = true;
            self.impulses_dirty = true;
        }
        if !self.forces_dirty && !self.impulses_dirty {
            return Ok(());
        }
        let buffers = self.gpu.as_mut().expect("allocated above");
        let clock = self.clock.as_ref();
        let retired = |slot: &StagingSlot| clock.is_none_or(|clock| clock.is_complete(slot.read_stamp));
        let index = match buffers.staging.iter().position(retired) {
            Some(index) => index,
            None => {
                let (index, stamp) = buffers
                    .staging
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| (index, slot.read_stamp))
                    .min_by_key(|&(_, stamp)| stamp)
                    .expect("the ring has slots");
                if !offline || !clock.is_some_and(|clock| clock.wait(stamp)) {
                    return Err("Liquid fields: every staging slot is still on the GPU".into());
                }
                index
            }
        };
        let slot = &mut buffers.staging[index];
        if self.impulses_dirty {
            // SAFETY: shared storage of (1 + capacity) × `bytes`; its last GPU
            // reader has retired (checked or waited above) and the copy below
            // is the next.
            unsafe { slot.buffer.write(0, bytemuck::cast_slice(&self.impulses)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, 0, &buffers.impulses, 0, bytes);
        }
        if self.forces_dirty && force_bytes > 0 {
            let forces = &self.forces[..self.force_lattices * lattice.node_count()];
            // SAFETY: as above, after the impulse lattice; the capacity holds
            // every force lattice.
            unsafe { slot.buffer.write(bytes, bytemuck::cast_slice(forces)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, bytes, &buffers.forces, 0, force_bytes);
        }
        slot.read_stamp = clock.map_or(0, FrameClock::stamp);
        self.forces_dirty = false;
        self.impulses_dirty = false;
        Ok(())
    }
}

/// Sample `field` at every lattice node; any non-finite value fails.
fn fill(lattice: &FieldLattice, values: &mut [[f32; 4]], field: impl Fn([f32; 3]) -> [f32; 3]) -> Result<(), ()> {
    for (index, value) in values.iter_mut().enumerate() {
        let v = field(lattice.position(index));
        if v.iter().any(|c| !c.is_finite()) {
            return Err(());
        }
        *value = [v[0], v[1], v[2], 0.0];
    }
    Ok(())
}

#[cfg(test)]
mod tests;
