//! The whitewater lifecycle's two rings (`docs/GPU_WHITEWATER_DESIGN.md` D6,
//! D7, section 3.5). A snapshot slot takes one frame's GPU copies of the
//! lifecycle's inputs; the CPU reads it only after that frame retired. An
//! output slot takes the population from the CPU; it is rewritten only after
//! every frame that read it retired. The content thread owns both: no lock,
//! no thread, and live never waits.

use manifold_fluids::{WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterParticle, WhitewaterSpawn};
use manifold_gpu::{FrameClock, GpuBuffer, GpuDevice};

use crate::node_graph::fluid::whitewater_fade;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::grid::face_len;

/// The frame-completion clock the rings stamp and check.
pub(crate) trait Fence {
    /// The stamp the frame now being encoded reaches when it retires.
    fn stamp(&self) -> u64;
    /// Whether every GPU use stamped `stamp` retired. Never blocks.
    fn is_complete(&self, stamp: u64) -> bool;
    /// Offline only: block until `stamp` retires. False on timeout.
    fn wait(&self, stamp: u64) -> bool;
}

impl Fence for FrameClock {
    fn stamp(&self) -> u64 {
        FrameClock::stamp(self)
    }

    fn is_complete(&self, stamp: u64) -> bool {
        FrameClock::is_complete(self, stamp)
    }

    fn wait(&self, stamp: u64) -> bool {
        FrameClock::wait(self, stamp)
    }
}

/// A device with no frame clock commits and waits every frame, so every
/// stamp has retired by the next frame.
pub(crate) struct Retired;

impl Fence for Retired {
    fn stamp(&self) -> u64 {
        0
    }

    fn is_complete(&self, _stamp: u64) -> bool {
        true
    }

    fn wait(&self, _stamp: u64) -> bool {
        true
    }
}

pub(crate) const SNAPSHOT_SLOTS: usize = 3;
pub(crate) const OUTPUT_SLOTS: usize = 3;
/// Output buffers grow in steps of this many records.
const OUTPUT_ROUNDING: usize = 4096;

/// What a snapshot slot is sized for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SnapshotShape {
    pub grid: WhitewaterGrid,
    pub face_cells: [u32; 3],
    pub face_offset: [u32; 3],
    /// Spawn records per frame: the lifecycle's capacity.
    pub capacity: u32,
}

impl SnapshotShape {
    pub fn face_bytes(&self, axis: usize) -> u64 {
        face_len(self.face_cells, axis) * 4
    }

    pub fn level_bytes(&self) -> u64 {
        self.grid.cell_count() as u64 * 4
    }

    pub fn solid_bytes(&self) -> u64 {
        self.grid.node_count() as u64 * 4
    }

    pub fn spawn_bytes(&self) -> u64 {
        u64::from(self.capacity) * std::mem::size_of::<WhitewaterSpawn>() as u64
    }

    /// Every byte one slot holds (9.2 MB at 64 with the default capacity).
    pub fn slot_bytes(&self) -> u64 {
        self.spawn_bytes() + 4 + (0..3).map(|a| self.face_bytes(a)).sum::<u64>() + self.level_bytes() + self.solid_bytes()
    }
}

/// This frame's lifecycle inputs, GPU arrays the capture copies.
pub(crate) struct CaptureInputs<'a> {
    pub spawns: &'a GpuBuffer,
    /// The emitters' running total and how many emitters it covers; its last
    /// entry is the frame's emitted count.
    pub offsets: Option<(&'a GpuBuffer, u32)>,
    pub faces: [&'a GpuBuffer; 3],
    pub level: &'a GpuBuffer,
    pub solid: &'a GpuBuffer,
}

/// One frame's copy of the lifecycle's inputs.
pub(crate) struct Snapshot {
    spawns: GpuBuffer,
    emitted: GpuBuffer,
    faces: [GpuBuffer; 3],
    level: GpuBuffer,
    solid: GpuBuffer,
    pub shape: SnapshotShape,
    pub stamp: u64,
    pub epoch: u32,
    pub ticks: u32,
    pub gravity: [f32; 3],
    /// Spawn records the copy filled; the rest of the slot is stale.
    spawn_records: u32,
    /// Captured and not yet consumed. The GPU writes a slot only when this is
    /// false; the CPU reads it only while it is true and its stamp retired.
    pub pending: bool,
}

fn shared(device: &GpuDevice, bytes: u64, what: &str) -> Result<GpuBuffer, String> {
    device.try_create_buffer_shared(bytes.max(4)).map_err(|error| format!("Whitewater {what} needs {bytes} bytes: {error}"))
}

impl Snapshot {
    fn allocate(device: &GpuDevice, shape: SnapshotShape) -> Result<Self, String> {
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), shape.slot_bytes())
            .map_err(|error| format!("Whitewater snapshot needs {} bytes: {error}", shape.slot_bytes()))?;
        Ok(Self {
            spawns: shared(device, shape.spawn_bytes(), "spawn snapshot")?,
            emitted: shared(device, 4, "emitted count")?,
            faces: [
                shared(device, shape.face_bytes(0), "face snapshot")?,
                shared(device, shape.face_bytes(1), "face snapshot")?,
                shared(device, shape.face_bytes(2), "face snapshot")?,
            ],
            level: shared(device, shape.level_bytes(), "level snapshot")?,
            solid: shared(device, shape.solid_bytes(), "solid snapshot")?,
            shape,
            stamp: 0,
            epoch: 0,
            ticks: 0,
            gravity: [0.0; 3],
            spawn_records: 0,
            pending: false,
        })
    }

    /// Encode this frame's copies into the slot, which must be free, and mark
    /// it pending at `stamp`.
    pub fn capture(
        &mut self,
        enc: &mut manifold_gpu::GpuEncoder,
        inputs: &CaptureInputs<'_>,
        stamp: u64,
        epoch: u32,
        ticks: u32,
        gravity: [f32; 3],
    ) -> Result<(), String> {
        assert!(!self.pending, "a pending snapshot slot is never captured over");
        let shape = self.shape;
        let short = |what: &str, have: u64, need: u64| {
            (have < need).then(|| format!("Whitewater: the {what} array holds {have} bytes; the grid needs {need}"))
        };
        let mut refusal = None;
        for axis in 0..3 {
            refusal = refusal.or_else(|| short("face", inputs.faces[axis].size, shape.face_bytes(axis)));
        }
        refusal = refusal
            .or_else(|| short("level", inputs.level.size, shape.level_bytes()))
            .or_else(|| short("solid", inputs.solid.size, shape.solid_bytes()));
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        let record = std::mem::size_of::<WhitewaterSpawn>() as u64;
        let spawn_bytes = inputs.spawns.size.min(shape.spawn_bytes()) / record * record;
        if spawn_bytes > 0 {
            enc.copy_buffer_to_buffer(inputs.spawns, &self.spawns, spawn_bytes);
        }
        match inputs.offsets {
            Some((offsets, emitters)) if emitters > 0 && u64::from(emitters) * 4 <= offsets.size => {
                enc.copy_buffer_range(offsets, (u64::from(emitters) - 1) * 4, &self.emitted, 0, 4);
            }
            // SAFETY: the slot is free, so no GPU work touches it.
            _ => unsafe { self.emitted.write(0, &0u32.to_ne_bytes()) },
        }
        for axis in 0..3 {
            enc.copy_buffer_to_buffer(inputs.faces[axis], &self.faces[axis], shape.face_bytes(axis));
        }
        enc.copy_buffer_to_buffer(inputs.level, &self.level, shape.level_bytes());
        enc.copy_buffer_to_buffer(inputs.solid, &self.solid, shape.solid_bytes());
        self.spawn_records = (spawn_bytes / record) as u32;
        self.stamp = stamp;
        self.epoch = epoch;
        self.ticks = ticks;
        self.gravity = gravity;
        self.pending = true;
        Ok(())
    }

    /// # Safety
    /// The frame stamped on this slot has retired and no capture since.
    unsafe fn floats(buffer: &GpuBuffer, bytes: u64) -> &[f32] {
        let ptr = buffer.mapped_ptr().expect("snapshot slots are shared storage");
        // SAFETY: shared storage of at least `bytes`, written by a retired frame.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), (bytes / 4) as usize) }
    }

    /// The copied fields.
    ///
    /// # Safety
    /// The frame stamped on this slot has retired and no capture since.
    pub unsafe fn fields(&self) -> WhitewaterFields<'_> {
        let s = self.shape;
        // SAFETY: the caller's contract; each length is the shape's.
        unsafe {
            WhitewaterFields {
                face_u: Self::floats(&self.faces[0], s.face_bytes(0)),
                face_v: Self::floats(&self.faces[1], s.face_bytes(1)),
                face_w: Self::floats(&self.faces[2], s.face_bytes(2)),
                face_cells: s.face_cells,
                face_offset: s.face_offset,
                level: Self::floats(&self.level, s.level_bytes()),
                solid: Self::floats(&self.solid, s.solid_bytes()),
                gravity: self.gravity,
            }
        }
    }

    /// The copied spawn records.
    ///
    /// # Safety
    /// The frame stamped on this slot has retired and no capture since.
    pub unsafe fn spawns(&self) -> &[WhitewaterSpawn] {
        let ptr = self.spawns.mapped_ptr().expect("snapshot slots are shared storage");
        // SAFETY: the copy filled `spawn_records` records; the caller's contract.
        unsafe { std::slice::from_raw_parts(ptr.cast::<WhitewaterSpawn>().cast_const(), self.spawn_records as usize) }
    }

    /// Spawns the emitters asked for this frame, before capacity.
    ///
    /// # Safety
    /// The frame stamped on this slot has retired and no capture since.
    pub unsafe fn emitted(&self) -> u32 {
        let ptr = self.emitted.mapped_ptr().expect("snapshot slots are shared storage");
        // SAFETY: four bytes of shared storage; the caller's contract.
        unsafe { ptr.cast::<u32>().read_unaligned() }
    }
}

#[derive(Default)]
pub(crate) struct SnapshotRing {
    pub slots: Vec<Snapshot>,
}

impl SnapshotRing {
    /// A free slot sized for `shape`, allocating up to [`SNAPSHOT_SLOTS`] and
    /// replacing a free slot of another shape. None when every slot is pending.
    pub fn free_slot(&mut self, device: &GpuDevice, shape: SnapshotShape) -> Result<Option<usize>, String> {
        if let Some(index) = self.slots.iter().position(|s| !s.pending && s.shape == shape) {
            return Ok(Some(index));
        }
        if let Some(index) = self.slots.iter().position(|s| !s.pending) {
            self.slots[index] = Snapshot::allocate(device, shape)?;
            return Ok(Some(index));
        }
        if self.slots.len() < SNAPSHOT_SLOTS {
            self.slots.push(Snapshot::allocate(device, shape)?);
            return Ok(Some(self.slots.len() - 1));
        }
        Ok(None)
    }

    /// The pending slot captured first.
    pub fn oldest_pending(&self) -> Option<usize> {
        (0..self.slots.len()).filter(|&i| self.slots[i].pending).min_by_key(|&i| self.slots[i].stamp)
    }

    pub fn pending_count(&self) -> usize {
        self.slots.iter().filter(|s| s.pending).count()
    }
}

/// One published population: foam, bubbles and spray as FluidParticle.
pub(crate) struct OutputSlot {
    pub buffers: [GpuBuffer; 3],
    records: usize,
    pub counts: [u32; 3],
    /// Frame-clock stamp of the last frame that read this slot.
    pub read_stamp: u64,
}

/// Output order: foam, bubbles, spray.
fn population_of(kind: WhitewaterKind) -> usize {
    match kind {
        WhitewaterKind::Foam => 0,
        WhitewaterKind::Bubble => 1,
        WhitewaterKind::Spray => 2,
    }
}

impl OutputSlot {
    fn allocate(device: &GpuDevice, records: usize) -> Result<Self, String> {
        let bytes = (records * std::mem::size_of::<FluidParticle>()) as u64;
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), 3 * bytes)
            .map_err(|error| format!("Whitewater output needs {} bytes: {error}", 3 * bytes))?;
        let buffer = || -> Result<GpuBuffer, String> {
            let buffer = shared(device, bytes, "output")?;
            buffer.zero_fill();
            Ok(buffer)
        };
        Ok(Self { buffers: [buffer()?, buffer()?, buffer()?], records, counts: [0; 3], read_stamp: 0 })
    }
}

#[derive(Default)]
pub(crate) struct OutputRing {
    pub slots: Vec<OutputSlot>,
    /// The slot the outputs provide.
    pub current: Option<usize>,
}

impl OutputRing {
    /// Write `particles` into a slot every reader of which retired, faded as
    /// FLIP's native whitewater is, and make it current. False when no slot
    /// has retired: the current one stays.
    pub fn publish(&mut self, device: &GpuDevice, fence: &dyn Fence, particles: &[WhitewaterParticle]) -> Result<bool, String> {
        let mut counts = [0usize; 3];
        for particle in particles {
            counts[population_of(particle.kind)] += 1;
        }
        let records = counts.iter().copied().max().unwrap_or(0).max(1).div_ceil(OUTPUT_ROUNDING) * OUTPUT_ROUNDING;
        let index = match self.slots.iter().position(|slot| fence.is_complete(slot.read_stamp)) {
            Some(index) => index,
            None if self.slots.len() < OUTPUT_SLOTS => {
                self.slots.push(OutputSlot::allocate(device, records)?);
                self.slots.len() - 1
            }
            None => return Ok(false),
        };
        if self.slots[index].records < records {
            let read_stamp = self.slots[index].read_stamp;
            self.slots[index] = OutputSlot { read_stamp, ..OutputSlot::allocate(device, records)? };
        }
        let slot = &mut self.slots[index];
        let outs: [&mut [FluidParticle]; 3] = slot.buffers.each_ref().map(|buffer| {
            let ptr = buffer.mapped_ptr().expect("output slots are shared storage");
            // SAFETY: shared storage of at least `records` particles; every
            // frame that read it retired, and the three buffers are distinct.
            unsafe { std::slice::from_raw_parts_mut(ptr.cast::<FluidParticle>(), records) }
        });
        let mut next = [0usize; 3];
        for particle in particles {
            let population = population_of(particle.kind);
            let p = particle.position;
            outs[population][next[population]] = FluidParticle {
                position_radius: [p[0], p[1], p[2], whitewater_fade(particle.lifetime)],
                velocity: particle.velocity,
                id: 0,
            };
            next[population] += 1;
        }
        slot.counts = counts.map(|n| n as u32);
        self.current = Some(index);
        Ok(true)
    }

    /// Stamp the current slot with the frame now being encoded, which reads it.
    pub fn mark_read(&mut self, fence: &dyn Fence) {
        if let Some(index) = self.current {
            self.slots[index].read_stamp = fence.stamp();
        }
    }

    pub fn current(&self) -> Option<&OutputSlot> {
        self.current.map(|index| &self.slots[index])
    }
}
