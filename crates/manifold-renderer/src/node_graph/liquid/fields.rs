//! Forces and impulses for every GPU liquid domain
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P8; GPU_MPM_SOLVER_DESIGN.md D13).
//!
//! A domain calls the same three things whatever its solver:
//! - [`LiquidImpulses`] keeps scene impulses on the domain's fixed-tick clock.
//!   Call [`LiquidImpulses::observe_frame`] right after `LiquidClock::advance`;
//!   the impulse hooks call `stamp`, `enqueue`, `drain_applied` and
//!   `drain_discarded`.
//! - [`LiquidFields::prepare`] samples the scene's acceleration field (when it
//!   changed) and this frame's impulses onto one coarse [`FieldLattice`], a
//!   quarter of the solver's resolution per axis, on the CPU once per frame.
//! - [`LiquidFields::upload`] copies what changed into the stable `forces` and
//!   `impulses` buffers in encoder order. The solver's atoms read them with
//!   `LIQUID_FIELD` (`shaders/liquid_field.wgsl`): forces every substep,
//!   impulses once, on the first substep of [`FieldFrame::impulse_tick`].

use manifold_core::Seconds;
use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{FieldValue, TickStamp, VectorField};

use crate::node_graph::fluid::TICK;
use crate::node_graph::liquid::clock::ClockFrame;
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
    pub forces_on: bool,
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
            ("forces_on", f32::from(u8::from(self.forces_on))),
            ("impulse_tick", self.impulse_tick.map_or(-1.0, |tick| tick as f32)),
        ]
    }
}

/// Scene impulses on a liquid's fixed-tick clock. A hit fired after a frame
/// is stamped at that frame's simulated time, so it lands on the first tick
/// the next frame runs, once. While the clock is held (pause, Speed 0) a hit
/// is discarded with a receipt, so resume never replays it.
///
/// Rigid targets of a coupled domain go to its rigid owner's queue under the
/// same sequence; the liquid owns the pair's clock, so a held liquid discards
/// them too.
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
    /// Receipts of begun ticks, undrained. `frame_start..` are this frame's.
    applied: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    frame_start: usize,
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
            self.applied.clear();
            self.discarded.clear();
            self.combined.clear();
            self.last_sequence = None;
            self.failure = None;
        }
        self.held = frame.held;
        self.transport = Some(transport);
        self.simulation_time = frame.simulation_time;
        self.frame_start = self.applied.len();
        self.impulse_tick = None;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let queue = self.queue.as_mut().ok_or("Liquid impulses: the clock never started")?;
        let first = (frame.simulation_time / TICK).round() as u64 - u64::from(frame.ticks);
        if queue.next_tick().tick != first {
            let error = format!(
                "Liquid impulses: tick {first} does not follow tick {}; restart the simulation",
                queue.next_tick().tick
            );
            self.failure = Some(error.clone());
            return Err(error);
        }
        let applied = &mut self.applied;
        for tick in first..first + u64::from(frame.ticks) {
            if let Err(error) = queue.begin_tick(TickStamp { epoch: self.epoch, tick }, |event| applied.push(event)) {
                let error = format!("Liquid impulses: {error}");
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        // One impulse lattice per frame: every hit of the frame shares a tick.
        let mut ticks = self.applied[self.frame_start..].iter().map(|event| event.applied.tick);
        self.impulse_tick = ticks.next();
        if ticks.any(|tick| Some(tick) != self.impulse_tick) {
            let error = "Liquid impulses: hits for two ticks of one frame; one frame applies one impulse tick".to_string();
            self.failure = Some(error.clone());
            return Err(error);
        }
        Ok(())
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
        let outstanding = queue.len() + self.applied.len() + self.discarded.len() + self.combined.len();
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

    /// Receipts of begun ticks: the liquid's, then the rigid owner's that the
    /// liquid's did not already name.
    pub fn drain_applied(
        &mut self,
        consume: &mut dyn FnMut(AppliedEvent<ResolvedNodeImpulse>),
        rigid: Option<&mut LiquidRigidOwner>,
    ) {
        for event in self.applied.drain(..) {
            consume(event);
        }
        self.frame_start = 0;
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

    /// Stamps of hits discarded while the clock was held.
    pub fn drain_discarded(&mut self, consume: &mut dyn FnMut(EventStamp)) {
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
        &self.applied[self.frame_start.min(self.applied.len())..]
    }
}

/// One staging slot: both lattices back to back, written by the CPU, copied
/// into the stable buffers by the GPU.
struct StagingSlot {
    buffer: GpuBuffer,
    /// Frame-clock stamp of the frame that copied from it; 0 never read.
    read_stamp: u64,
}

struct FieldBuffers {
    lattice: FieldLattice,
    forces: GpuBuffer,
    impulses: GpuBuffer,
    staging: Vec<StagingSlot>,
}

/// The CPU lattices and their GPU copies. CPU storage changes size only with
/// the lattice; the force lattice is resampled only when the field changes.
#[derive(Default)]
pub struct LiquidFields {
    lattice: Option<FieldLattice>,
    forces: Vec<[f32; 4]>,
    impulses: Vec<[f32; 4]>,
    /// The field the force lattice holds.
    force_source: Option<FieldValue>,
    forces_dirty: bool,
    impulses_dirty: bool,
    clock: Option<FrameClock>,
    gpu: Option<FieldBuffers>,
}

impl LiquidFields {
    /// Sample this frame's field and hits onto the lattice.
    pub fn prepare(
        &mut self,
        lattice: FieldLattice,
        field: Option<&FieldValue>,
        impulses: &LiquidImpulses,
    ) -> Result<FieldFrame, String> {
        if self.lattice != Some(lattice) {
            let count = lattice.node_count();
            self.forces.clear();
            self.forces.resize(count, [0.0; 4]);
            self.impulses.clear();
            self.impulses.resize(count, [0.0; 4]);
            self.lattice = Some(lattice);
            self.force_source = None;
            self.forces_dirty = true;
            self.impulses_dirty = true;
        }
        if let Some(field) = field
            && self.force_source.as_ref() != Some(field)
        {
            fill(&lattice, &mut self.forces, |x| field.sample(x))
                .map_err(|_| "Liquid forces: the acceleration field is not finite inside the domain".to_string())?;
            self.force_source = Some(field.clone());
            self.forces_dirty = true;
        }
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
            forces_on: field.is_some(),
            impulse_tick,
        })
    }

    pub fn forces(&self) -> &[[f32; 4]] {
        &self.forces
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
        if self.gpu.as_ref().is_none_or(|buffers| buffers.lattice != lattice) {
            let create = |what: &str, size: u64| {
                gpu.device
                    .try_create_buffer_shared(size)
                    .map_err(|error| format!("Liquid fields: the {what} need {size} bytes: {error}"))
            };
            let forces = create("forces", bytes)?;
            let impulses = create("impulses", bytes)?;
            forces.zero_fill();
            impulses.zero_fill();
            let staging = (0..STAGING_SLOTS)
                .map(|_| create("staging lattices", 2 * bytes).map(|buffer| StagingSlot { buffer, read_stamp: 0 }))
                .collect::<Result<_, _>>()?;
            self.gpu = Some(FieldBuffers { lattice, forces, impulses, staging });
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
        if self.forces_dirty {
            // SAFETY: shared storage of 2 × `bytes`; its last GPU reader has
            // retired (checked or waited above) and the copy below is the next.
            unsafe { slot.buffer.write(0, bytemuck::cast_slice(&self.forces)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, 0, &buffers.forces, 0, bytes);
        }
        if self.impulses_dirty {
            // SAFETY: as above, the second half of the slot.
            unsafe { slot.buffer.write(bytes, bytemuck::cast_slice(&self.impulses)) };
            gpu.native_enc.copy_buffer_range(&slot.buffer, bytes, &buffers.impulses, 0, bytes);
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
