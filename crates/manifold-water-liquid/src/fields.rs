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

use manifold_gpu::GpuBuffer;
use manifold_nodes_physics::force_lattice::{LatticeBuffers, LatticeUpload};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::tick_hits::{Admission, TickHits, HIT_CAPACITY};
use manifold_physics::{FieldValue, PlayedTime, TickStamp, VectorField};

use crate::clock::{ClockFrame, LiquidClock};
use crate::coupling::LiquidRigidOwner;
use manifold_physics::force_lattice::{ForceLattice, TickForces};
use manifold_water_rigid::physics::ResolvedRigidImpulse;
use manifold_water_rigid::physics_events::{ResolvedNodeImpulse, map_rigid_receipt};

/// Hits in flight at once (the shared tick-hit capacity).
pub const IMPULSE_CAPACITY: usize = HIT_CAPACITY;

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

/// Scene impulses on a liquid's fixed-tick clock: the shared [`TickHits`]
/// queue (stamping, held discards, receipts, commit), plus the coupled rigid
/// owner. Rigid targets of a coupled domain go to its rigid owner's queue
/// under the same sequence; the liquid owns the pair's clock, so a held
/// liquid discards them too.
pub struct LiquidImpulses {
    hits: TickHits<ResolvedNodeImpulse>,
    /// Sequences sent to both queues: the liquid receipt names them, so the
    /// rigid receipt is dropped.
    combined: Vec<u64>,
}

impl Default for LiquidImpulses {
    fn default() -> Self {
        Self { hits: TickHits::new("Liquid impulses"), combined: Vec::new() }
    }
}

impl LiquidImpulses {
    /// Begin this frame's ticks. A restart opens a new epoch and cancels every
    /// unread event of the old one.
    pub fn observe_frame(&mut self, played: PlayedTime, frame: &ClockFrame) -> Result<(), String> {
        if frame.restarted {
            self.combined.clear();
        }
        self.hits.observe_frame(played, frame)
    }

    /// This frame's ticks are encoded: its hits are applied.
    pub fn commit_frame(&mut self) {
        self.hits.commit_frame();
    }

    /// This frame's ticks will not run: its hits are discarded, with a receipt.
    pub fn abandon_frame(&mut self) {
        self.hits.abandon_frame();
    }

    /// The epoch stamps must carry, once the clock has started.
    pub fn epoch(&self) -> Option<u64> {
        self.hits.epoch()
    }

    /// Stamp a hit at the simulated time of the frame last observed: it lands
    /// on the next frame's first tick.
    pub fn stamp(&self, played: PlayedTime, sequence: u64) -> Result<EventStamp, String> {
        self.hits.stamp(played, sequence)
    }

    /// Whether hits are refused because the clock broke; cleared by a restart.
    pub fn failure(&self) -> Option<&str> {
        self.hits.failure()
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
        let rigid = match (impulse.target.rigid_targets(), rigid) {
            (Some(targets), Some(owner)) => Some((targets, owner)),
            (Some(_), None) => return Err("Liquid impulses: rigid targets need a coupled rigid world".into()),
            (None, _) => None,
        };
        if let Admission::Discarded(next) = self.hits.admit(stamp, self.combined.len())? {
            return Ok(next);
        }
        let mut planned = None;
        if let Some((targets, owner)) = rigid {
            let rigid_stamp = owner.rigid().worker_impulse_stamp(owner.completed_time(), stamp.sequence)?;
            planned = Some(owner.rigid_mut().enqueue_impulse(
                rigid_stamp,
                ResolvedRigidImpulse { field: impulse.field.clone(), targets },
            )?);
        }
        if !impulse.target.affects_fluid() {
            self.hits.take(stamp, None)?;
            return Ok(planned.expect("a rigid-only hit was sent to the rigid owner"));
        }
        if planned.is_some() {
            self.combined.push(stamp.sequence);
        }
        Ok(self.hits.take(stamp, Some(impulse))?.expect("a liquid hit is queued"))
    }

    /// Receipts of committed frames: the liquid's, then the rigid owner's that
    /// the liquid's did not already name. A frame drained uncommitted failed.
    pub fn drain_applied(
        &mut self,
        consume: &mut dyn FnMut(AppliedEvent<ResolvedNodeImpulse>),
        rigid: Option<&mut LiquidRigidOwner>,
    ) {
        self.hits.drain_applied(consume);
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
        let Some(owner) = rigid else {
            return self.hits.drain_discarded(consume);
        };
        let rigid = owner.rigid_mut();
        for stamp in rigid.drain_discarded_impulses() {
            if let Some(index) = self.combined.iter().position(|&sequence| sequence == stamp.sequence) {
                self.combined.swap_remove(index);
            }
            if !self.hits.discarded().iter().any(|liquid| liquid.sequence == stamp.sequence) {
                consume(stamp);
            }
        }
        self.hits.drain_discarded(consume) + rigid.take_discarded_beyond()
    }

    /// The tick this frame's hits land on, if any.
    pub fn impulse_tick(&self) -> Option<u64> {
        self.hits.impulse_tick()
    }

    /// This frame's hits on the liquid.
    fn frame_events(&self) -> &[AppliedEvent<ResolvedNodeImpulse>] {
        self.hits.frame_events()
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

    /// The played times the history replay must sample before the next
    /// frame: each boundary a tick may start at under `clock`, in `(from, until]`.
    pub fn request_samples(&mut self, clock: &LiquidClock, from: PlayedTime, until: PlayedTime, out: &mut Vec<PlayedTime>) {
        self.forces.request_samples(clock, from, until, out);
    }

    /// A history replay sample of the field at played time `now`, after `clock`
    /// observed this sample's Speed.
    pub fn observe_sample(&mut self, clock: &LiquidClock, now: PlayedTime, field: Option<&FieldValue>) {
        self.forces.observe_sample(clock, now, field);
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
            tick_seconds: &[],
        };
        self.gpu.upload(gpu, offline, data)?;
        self.forces.mark_uploaded();
        self.impulses_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
