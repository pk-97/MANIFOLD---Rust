//! Forces and impulses for every GPU liquid domain
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P8; GPU_MPM_SOLVER_DESIGN.md D13).
//!
//! A domain calls the same things whatever its solver:
//! - [`LiquidImpulses`] keeps scene impulses on the domain's accepted clock.
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
//!   [`ForceLattice`], a quarter of the solver's resolution per axis: one
//!   lattice while the ticks agree, one per tick otherwise, so a tick reads
//!   the same forces at any frame rate. A tick nobody sampled is an error.
//!   Matter impulses use one lattice per accepted interval. GPU FLIP hits
//!   retain distinct timestamps and use one lattice per timestamp; ties sum
//!   in source order.
//! - [`LiquidFields::upload`] copies what changed into the stable `forces` and
//!   `impulses` buffers in encoder order. The solver's atoms read them with
//!   `FORCE_LATTICE` (manifold-nodes-physics): forces every substep from
//!   their tick's lattice, impulses once, on the first substep of
//!   [`FieldFrame::impulse_tick`].

use manifold_core::Seconds;
use manifold_gpu::GpuBuffer;
use manifold_nodes_physics::force_lattice::{LatticeBuffers, LatticeUpload};
use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{FieldValue, TickStamp, VectorField};

use manifold_physics::clock::TICK;
use crate::clock::{ClockFrame, LiquidClock};
use crate::coupling::LiquidRigidOwner;
use manifold_physics::force_lattice::{ForceLattice, TickForces};
use manifold_water_rigid::physics::ResolvedRigidImpulse;
use manifold_water_rigid::physics_events::{ResolvedNodeImpulse, map_rigid_receipt};

/// Hits in flight at once: queued, this frame's and undrained applied
/// receipts. Discard receipts hold no slot. A hit past it is refused on its
/// own; the next one is tried again.
pub const IMPULSE_CAPACITY: usize = 256;

/// What a domain publishes for its atoms about this frame's fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldFrame {
    pub lattice: ForceLattice,
    /// Force lattices in `forces`: 0 without a field, 1 while it holds still
    /// (every tick reads it), else one per tick from the frame's first tick.
    pub force_lattices: u32,
    /// The tick (counted in the epoch) whose first substep adds the impulses.
    pub impulse_tick: Option<u64>,
}

impl FieldFrame {
    /// The scalar outputs, by name. A frame without impulses publishes −1.
    pub fn outputs(&self) -> [(&'static str, f32); 6] {
        let [x, y, z] = self.lattice.nodes().map(|n| n as f32);
        [
            ("field_nodes_x", x),
            ("field_nodes_y", y),
            ("field_nodes_z", z),
            ("field_spacing", self.lattice.spacing()),
            ("force_lattices", self.force_lattices as f32),
            ("impulse_tick", self.impulse_tick.map_or(-1.0, |tick| tick as f32)),
        ]
    }
}

/// The first tick a frame runs, counted in its epoch.
pub fn first_tick(frame: &ClockFrame) -> u64 {
    frame.first_sequence
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
    /// Transport of the frame last observed: no older hit is stamped.
    transport: Option<f64>,
    simulation_time: f64,
    last_sequence: Option<u64>,
    /// This frame's hits, taken by its begun ticks, until the frame commits.
    pending: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    /// Receipts of committed frames, undrained.
    applied: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    impulse_tick: Option<u64>,
    /// Discard receipts, undrained, at most `IMPULSE_CAPACITY`; sized when
    /// the clock starts so a held fire never allocates.
    discarded: Vec<EventStamp>,
    /// Discards past that bound, counted without a stamp.
    discarded_beyond: u64,
    /// Sequences sent to both queues: the liquid receipt names them, so the
    /// rigid receipt is dropped.
    combined: Vec<u64>,
    /// The clock broke (a tick out of order): every hit is refused until a
    /// restart. A full history is never this; it refuses one hit at a time.
    failure: Option<String>,
}

/// Keep one discard receipt within the bound, else count it without a stamp.
fn record_discard(discarded: &mut Vec<EventStamp>, beyond: &mut u64, stamp: EventStamp) {
    if discarded.len() < IMPULSE_CAPACITY {
        discarded.push(stamp);
    } else {
        *beyond += 1;
    }
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
                    self.discarded.reserve_exact(IMPULSE_CAPACITY);
                }
            }
            self.pending.clear();
            self.applied.clear();
            self.discarded.clear();
            self.discarded_beyond = 0;
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
            let stamp = TickStamp { epoch: self.epoch, tick };
            let result = if let Some(interval) = frame.interval(tick - first) {
                queue.begin_interval(stamp, interval, |event| pending.push(event))
            } else {
                Ok(())
            };
            if let Err(error) = result {
                let error = format!("Liquid impulses: {error}");
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        // The timestamped GPU path carries each event's accepted interval.
        self.impulse_tick = self.pending.first().map(|event| event.applied.tick);
        Ok(())
    }

    /// This frame's ticks are encoded: its hits are applied.
    pub fn commit_frame(&mut self) {
        self.applied.append(&mut self.pending);
    }

    /// This frame's ticks will not run: its hits are discarded, with a receipt.
    pub fn abandon_frame(&mut self) {
        let Self { pending, discarded, discarded_beyond, .. } = self;
        for event in pending.drain(..) {
            record_discard(discarded, discarded_beyond, event.source);
        }
    }

    /// The epoch stamps must carry, once the clock has started.
    pub fn epoch(&self) -> Option<u64> {
        self.queue.as_ref().map(|_| self.epoch)
    }

    /// Stamp a hit at the simulated time of the frame last observed. Every hit
    /// has no tick yet whatever its transport, so it lands on the next
    /// frame's first tick. A hit older than that frame is late, never
    /// skipped: triggers are the performance.
    pub fn stamp(&self, transport: f64, sequence: u64) -> Result<EventStamp, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.queue.is_none() || self.transport.is_none() {
            return Err("Liquid impulses: the clock has not started".into());
        }
        if !transport.is_finite() {
            return Err("Liquid impulses: the hit time is not finite".into());
        }
        Ok(EventStamp {
            epoch: self.epoch,
            time: Seconds(self.simulation_time),
            sequence,
        })
    }

    /// Whether hits are refused because the clock broke; cleared by a restart.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Admit one resolved hit. Rigid targets need the coupled rigid owner. A
    /// held clock discards the hit (a receipt in `drain_discarded`) and still
    /// succeeds, so the producer rearms. A full history refuses this hit only.
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
        let next = queue.next_tick();
        if self.held {
            self.last_sequence = Some(stamp.sequence);
            record_discard(&mut self.discarded, &mut self.discarded_beyond, stamp);
            return Ok(next);
        }
        let outstanding = queue.len() + self.pending.len() + self.applied.len() + self.combined.len();
        if outstanding >= IMPULSE_CAPACITY {
            return Err(format!(
                "Liquid impulses: {IMPULSE_CAPACITY} hits are already in flight; this hit is refused"
            ));
        }
        let mut planned = None;
        if let Some((targets, owner)) = rigid {
            let rigid_stamp = owner.rigid().impulse_stamp(owner.completed_time(), stamp.sequence)?;
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
            .map_err(|error| format!("Liquid impulses: {error}"))
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

    /// Stamps of hits discarded while the clock was held or by a failed frame,
    /// then the rigid owner's. A hit both halves discarded reads once.
    /// Returns the count of further discards past either bounded receipt
    /// list: discarded hits with no stamp.
    pub fn drain_discarded(&mut self, consume: &mut dyn FnMut(EventStamp), rigid: Option<&mut LiquidRigidOwner>) -> u64 {
        self.abandon_frame();
        let Self { discarded, discarded_beyond, combined, .. } = self;
        let beyond = std::mem::take(discarded_beyond);
        let Some(owner) = rigid else {
            discarded.drain(..).for_each(&mut *consume);
            return beyond;
        };
        let rigid = owner.rigid_mut();
        for stamp in rigid.drain_discarded_impulses() {
            if let Some(index) = combined.iter().position(|&sequence| sequence == stamp.sequence) {
                combined.swap_remove(index);
            }
            if !discarded.iter().any(|liquid| liquid.sequence == stamp.sequence) {
                consume(stamp);
            }
        }
        discarded.drain(..).for_each(&mut *consume);
        beyond + rigid.take_discarded_beyond()
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

/// The CPU lattices and their GPU copies. CPU storage grows with the lattice
/// and the ticks of one frame; a still field is resampled only when it
/// changes.
#[derive(Default)]
pub struct LiquidFields {
    lattice: Option<ForceLattice>,
    forces: TickForces,
    impulses: Vec<[f32; 4]>,
    live_hits: Vec<[u32; 4]>,
    live_frame: Option<ClockFrame>,
    frame_first: u64,
    frame_ticks: u32,
    split_live_hits: bool,
    impulses_dirty: bool,
    gpu: LatticeBuffers,
}

impl LiquidFields {
    pub fn split_live_hits(&mut self) { self.split_live_hits = true; }

    /// Lay each of the frame's ticks' fields onto its lattice and sample the
    /// frame's hits. Call right after `clock` advanced to `frame`; `field` is
    /// this frame's, which belongs to any tick starting exactly now.
    pub fn prepare(
        &mut self,
        lattice: ForceLattice,
        field: Option<&FieldValue>,
        clock: &LiquidClock,
        frame: &ClockFrame,
        impulses: &LiquidImpulses,
    ) -> Result<FieldFrame, String> {
        self.live_frame = self.split_live_hits.then(|| frame.clone());
        self.frame_first = frame.first_sequence;
        self.frame_ticks = frame.ticks;
        if self.lattice != Some(lattice) {
            self.impulses.clear();
            self.impulses.resize(lattice.node_count(), [0.0; 4]);
            self.lattice = Some(lattice);
            self.impulses_dirty = true;
        }
        self.forces.prepare(lattice, field, clock, frame)?;
        self.finish(lattice, impulses)
    }

    /// The transport times the history replay must sample before the next
    /// frame: each tick's start under `clock`, in `(from, until]`.
    pub fn request_samples(&mut self, clock: &LiquidClock, from: f64, until: f64, out: &mut Vec<f64>) {
        self.forces.request_samples(clock, from, until, out);
    }

    /// A history replay sample of the field at transport `now`.
    pub fn observe_sample(&mut self, now: f64, field: Option<&FieldValue>) {
        self.forces.observe_sample(now, field);
    }

    fn finish(&mut self, lattice: ForceLattice, impulses: &LiquidImpulses) -> Result<FieldFrame, String> {
        let mut impulse_tick = impulses.impulse_tick();
        self.live_hits.clear();
        let events = impulses.frame_events();
        let count = lattice.node_count();
        let lattice_count = if self.live_frame.is_some() { events.len().max(1) } else { self.frame_ticks.max(1) as usize };
        self.impulses.resize(count * lattice_count, [0.0; 4]);
        if impulse_tick.is_some() {
            if let Some(frame) = &self.live_frame {
                let mut first = 0;
                while first < events.len() {
                    let tick = events[first].applied.tick;
                    let interval = frame.interval(tick - frame.first_sequence).expect("event belongs to accepted interval");
                    let start = interval.start.0;
                    let relative = (events[first].source.time.0 - start).max(0.0) as f32;
                    let mut end = first + 1;
                    while end < events.len() && events[end].applied.tick == tick && ((events[end].source.time.0 - start).max(0.0) as f32).to_bits() == relative.to_bits() {
                        end += 1;
                    }
                    let index = self.live_hits.len();
                    self.live_hits.push([relative.to_bits(), index as u32, tick as u32, 0]);
                    lattice.fill(&mut self.impulses[index * count..(index + 1) * count], |x| {
                        let mut sum = [0.0; 3];
                        for event in &events[first..end] {
                            if event.value.target.affects_fluid() {
                                let value = event.value.field.sample(x);
                                for axis in 0..3 { sum[axis] += value[axis]; }
                            }
                        }
                        sum
                    }).map_err(|_| "Liquid impulses: the impulse field is not finite inside the domain".to_string())?;
                    first = end;
                }
            } else {
                impulse_tick = Some(self.frame_first);
                for (ordinal, values) in self.impulses.chunks_exact_mut(count).enumerate() {
                let tick = self.frame_first + ordinal as u64;
                lattice.fill(values, |x| {
                    let mut sum = [0.0f32; 3];
                    for event in events.iter().filter(|event| event.applied.tick == tick && event.value.target.affects_fluid()) {
                        let v = event.value.field.sample(x);
                        for axis in 0..3 { sum[axis] += v[axis]; }
                    }
                    sum
                }).map_err(|_| "Liquid impulses: the impulse field is not finite inside the domain".to_string())?;
                }
            }
            self.impulses_dirty = true;
        }
        Ok(FieldFrame {
            lattice,
            force_lattices: self.forces.lattices() as u32,
            impulse_tick,
        })
    }

    /// Every force lattice, back to back.
    pub fn forces(&self) -> &[[f32; 4]] {
        self.forces.forces()
    }

    pub fn impulses(&self) -> &[[f32; 4]] {
        &self.impulses
    }

    pub fn live_hit_count(&self) -> usize { self.live_hits.len() }

    pub fn live_hits_buffer(&self) -> Option<&GpuBuffer> {
        self.gpu.hits()
    }

    pub fn forces_buffer(&self) -> Option<&GpuBuffer> {
        self.gpu.forces()
    }

    pub fn impulses_buffer(&self) -> Option<&GpuBuffer> {
        self.gpu.impulses()
    }

    /// Copy what changed into the stable buffers, in encoder order. Live never
    /// waits for a staging slot; offline waits for the oldest.
    pub fn upload(&mut self, gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>, offline: bool) -> Result<(), String> {
        let Some(lattice) = self.lattice else { return Ok(()) };
        let data = LatticeUpload {
            lattice,
            forces: self.forces.forces(),
            forces_dirty: self.forces.is_dirty(),
            impulses: &self.impulses,
            impulses_dirty: self.impulses_dirty,
            hits: &self.live_hits,
        };
        self.gpu.upload(gpu, offline, data)?;
        self.forces.mark_uploaded();
        self.impulses_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
