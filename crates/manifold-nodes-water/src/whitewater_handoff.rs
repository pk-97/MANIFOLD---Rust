//! The whitewater lifecycle's handoff (`docs/GPU_WHITEWATER_DESIGN.md` D6,
//! D7, D11, section 3.5). A snapshot slot takes one frame's GPU copies of the
//! lifecycle's inputs; once that frame retired, the slot is loaned to the
//! lifecycle worker, which loads and steps it. An output slot is loaned to
//! the worker once every frame that read it retired; the worker writes the
//! population into it and the content thread makes it current. Slots move by
//! value through two one-deep channels, the way FLIP's particle ring loans
//! its slots, so no slot is ever shared: no lock, and live never waits.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use manifold_fluids::{
    WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterLifecycle as NativeLifecycle, WhitewaterParticle, WhitewaterSpawn,
};
use manifold_gpu::{FrameClock, GpuBuffer, GpuDevice};

use manifold_water_liquid::clock::whitewater_fade;
use manifold_physics::clock::TICK;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::grid::face_len;

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

/// Three in flight on the GPU and one on loan to the worker, so live drops
/// ticks only when the GPU is three frames behind.
pub(crate) const SNAPSHOT_SLOTS: usize = 4;
/// The current one, one on loan, and two still being read.
pub(crate) const OUTPUT_SLOTS: usize = 4;
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
    /// Captured and not yet loaned. The GPU writes a slot only when this is
    /// false; the worker reads it only once its stamp retired.
    pub pending: bool,
}

fn shared(device: &GpuDevice, bytes: u64, what: &str) -> Result<GpuBuffer, String> {
    device.try_create_buffer_shared(bytes.max(4)).map_err(|error| format!("Whitewater {what} needs {bytes} bytes: {error}"))
}

impl Snapshot {
    fn allocate(device: &GpuDevice, shape: SnapshotShape) -> Result<Self, String> {
        manifold_node_engine::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), shape.slot_bytes())
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
    /// Free and pending slots; loaned ones are on the worker.
    pub slots: Vec<Snapshot>,
    /// Slots allocated, wherever they are.
    allocated: usize,
}

impl SnapshotRing {
    /// A free slot sized for `shape`, allocating up to [`SNAPSHOT_SLOTS`] and
    /// replacing a free slot of another shape. None when every slot is
    /// pending or on loan.
    pub fn free_slot(&mut self, device: &GpuDevice, shape: SnapshotShape) -> Result<Option<usize>, String> {
        if let Some(index) = self.slots.iter().position(|s| !s.pending && s.shape == shape) {
            return Ok(Some(index));
        }
        if let Some(index) = self.slots.iter().position(|s| !s.pending) {
            self.slots[index] = Snapshot::allocate(device, shape)?;
            return Ok(Some(index));
        }
        if self.allocated < SNAPSHOT_SLOTS {
            self.slots.push(Snapshot::allocate(device, shape)?);
            self.allocated += 1;
            return Ok(Some(self.slots.len() - 1));
        }
        Ok(None)
    }

    /// The pending slot captured first.
    fn oldest_pending(&self) -> Option<usize> {
        (0..self.slots.len()).filter(|&i| self.slots[i].pending).min_by_key(|&i| self.slots[i].stamp)
    }

    pub fn pending_count(&self) -> usize {
        self.slots.iter().filter(|s| s.pending).count()
    }

    /// Move each pending slot whose frame retired into `loan`, oldest first
    /// (I4). Live stops at the first that hasn't retired (I3); offline waits
    /// for it. A slot of another epoch or shape is freed, never loaned (I6).
    pub fn take_retired(
        &mut self,
        fence: &dyn Fence,
        offline: bool,
        epoch: u32,
        shape: SnapshotShape,
        loan: &mut Vec<Snapshot>,
    ) -> Result<(), String> {
        while let Some(index) = self.oldest_pending() {
            let stamp = self.slots[index].stamp;
            if !fence.is_complete(stamp) {
                if !offline {
                    break;
                }
                if !fence.wait(stamp) {
                    return Err("a frame's snapshot did not finish on the GPU within 5 seconds".into());
                }
            }
            if self.slots[index].epoch != epoch || self.slots[index].shape != shape {
                self.slots[index].pending = false;
                continue;
            }
            loan.push(self.slots.swap_remove(index));
        }
        Ok(())
    }

    /// Slots back from the worker, free again.
    pub fn give_back(&mut self, loan: &mut Vec<Snapshot>) {
        for mut slot in loan.drain(..) {
            slot.pending = false;
            self.slots.push(slot);
        }
    }

    /// Slots that went down with a stopped worker.
    pub fn lose(&mut self, count: usize) {
        self.allocated -= count;
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
        manifold_node_engine::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), 3 * bytes)
            .map_err(|error| format!("Whitewater output needs {} bytes: {error}", 3 * bytes))?;
        let buffer = || -> Result<GpuBuffer, String> {
            let buffer = shared(device, bytes, "output")?;
            buffer.zero_fill();
            Ok(buffer)
        };
        Ok(Self { buffers: [buffer()?, buffer()?, buffer()?], records, counts: [0; 3], read_stamp: 0 })
    }

    /// Worker side: write `particles` into this loaned slot, faded as FLIP's
    /// native whitewater is. Err with the records per population it needs
    /// when it holds fewer; the slot is then untouched.
    fn write(&mut self, particles: &[WhitewaterParticle]) -> Result<(), usize> {
        let mut counts = [0usize; 3];
        for particle in particles {
            counts[population_of(particle.kind)] += 1;
        }
        let records = counts.iter().copied().max().unwrap_or(0).max(1).div_ceil(OUTPUT_ROUNDING) * OUTPUT_ROUNDING;
        if self.records < records {
            return Err(records);
        }
        let outs: [&mut [FluidParticle]; 3] = self.buffers.each_ref().map(|buffer| {
            let ptr = buffer.mapped_ptr().expect("output slots are shared storage");
            // SAFETY: shared storage of `self.records` particles; the slot is
            // loaned only once every frame that read it retired, and the
            // three buffers are distinct.
            unsafe { std::slice::from_raw_parts_mut(ptr.cast::<FluidParticle>(), self.records) }
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
        self.counts = counts.map(|n| n as u32);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct OutputRing {
    /// Neither current nor on loan; frames may still read them.
    free: Vec<OutputSlot>,
    /// The slot the outputs provide.
    current: Option<OutputSlot>,
    /// Slots allocated, wherever they are.
    allocated: usize,
    /// Records per population a loaned slot holds.
    records: usize,
}

impl OutputRing {
    /// A slot for the worker to write the population into, every reader of
    /// which retired, grown on this thread to the records the population
    /// last needed. None when none has retired: the current one stays.
    pub fn loan(&mut self, device: &GpuDevice, fence: &dyn Fence) -> Result<Option<OutputSlot>, String> {
        let records = self.records.max(OUTPUT_ROUNDING);
        let retired = |slot: &OutputSlot| fence.is_complete(slot.read_stamp);
        if let Some(index) = self.free.iter().position(|slot| retired(slot) && slot.records >= records) {
            return Ok(Some(self.free.swap_remove(index)));
        }
        if let Some(index) = self.free.iter().position(retired) {
            drop(self.free.swap_remove(index));
            self.allocated -= 1;
        }
        if self.allocated == OUTPUT_SLOTS {
            return Ok(None);
        }
        let slot = OutputSlot::allocate(device, records)?;
        self.allocated += 1;
        Ok(Some(slot))
    }

    /// The worker wrote `slot`: provide it. The slot it replaces is freed;
    /// frames in flight may still read it.
    pub fn accept(&mut self, slot: OutputSlot) {
        if let Some(old) = self.current.replace(slot) {
            self.free.push(old);
        }
    }

    /// A loan that came back unwritten.
    pub fn give_back(&mut self, slot: OutputSlot) {
        self.free.push(slot);
    }

    /// The population needs `records` per population; later loans hold them.
    pub fn grow(&mut self, records: usize) {
        self.records = self.records.max(records);
    }

    /// Provide nothing until the next write: the population started over.
    pub fn clear(&mut self) {
        if let Some(old) = self.current.take() {
            self.free.push(old);
        }
    }

    /// A loan that went down with a stopped worker.
    pub fn lose(&mut self) {
        self.allocated -= 1;
    }

    /// Stamp the current slot with the frame now being encoded, which reads it.
    pub fn mark_read(&mut self, fence: &dyn Fence) {
        if let Some(slot) = self.current.as_mut() {
            slot.read_stamp = fence.stamp();
        }
    }

    pub fn current(&self) -> Option<&OutputSlot> {
        self.current.as_ref()
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn allocated(&self) -> usize {
        self.allocated
    }
}

/// The lifecycle starts over: a new one on this grid and capacity, or the
/// same one cleared for a new epoch (D12, I6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Reset {
    pub grid: WhitewaterGrid,
    pub capacity: u32,
    pub epoch: u32,
}

/// What one frame hands the worker: everything it owns until the reply.
pub(crate) struct Request {
    /// Applied before any snapshot loads.
    pub reset: Option<Reset>,
    /// Retired snapshots of the current epoch and shape, oldest first.
    pub snapshots: Vec<Snapshot>,
    /// A slot every reader of which retired, for the stepped population.
    pub output: Option<OutputSlot>,
    /// Tests only: the worker waits for this before it starts.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub hold: Option<Receiver<()>>,
}

/// The worker's answer: the loans back, and what the work did.
pub(crate) struct Reply {
    pub snapshots: Vec<Snapshot>,
    pub output: Option<OutputSlot>,
    /// `output` holds the population.
    pub written: bool,
    /// `output` was too small: records per population the population needs.
    pub needs: Option<usize>,
    /// Spawns the emitters asked for in these snapshots, before capacity.
    pub emitted: u64,
    pub thinned: u64,
    pub worker_ms: f64,
    pub failure: Option<String>,
    /// Tests only: the thread FLIP's update ran on.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub updated_on: std::thread::ThreadId,
}

/// The lifecycle's thread; FLIP's update runs on no other (D11).
pub(crate) const LIFECYCLE_THREAD: &str = "whitewater-lifecycle";

/// FLIP's lifecycle, owned by the worker thread alone.
#[derive(Default)]
struct Lifecycle {
    native: Option<NativeLifecycle>,
    population: Vec<WhitewaterParticle>,
}

impl Lifecycle {
    /// The one way into FLIP's update.
    fn process(&mut self, mut request: Request) -> Reply {
        assert_eq!(
            std::thread::current().name(),
            Some(LIFECYCLE_THREAD),
            "FLIP's whitewater update runs only on its own thread, never the content thread"
        );
        #[cfg(all(test, feature = "gpu-proofs"))]
        if let Some(hold) = request.hold.take() {
            let _ = hold.recv();
        }
        let start = std::time::Instant::now();
        let mut reply = Reply {
            snapshots: std::mem::take(&mut request.snapshots),
            output: request.output.take(),
            written: false,
            needs: None,
            emitted: 0,
            thinned: 0,
            worker_ms: 0.0,
            failure: None,
            #[cfg(all(test, feature = "gpu-proofs"))]
            updated_on: std::thread::current().id(),
        };
        reply.failure = self.step(request.reset, &mut reply).and_then(|()| self.write(&mut reply)).err();
        reply.worker_ms = start.elapsed().as_secs_f64() * 1000.0;
        reply
    }

    /// Load each snapshot, oldest first, and step it its ticks (I4).
    fn step(&mut self, reset: Option<Reset>, reply: &mut Reply) -> Result<(), String> {
        if let Some(reset) = reset {
            let fits = self.native.as_ref().is_some_and(|l| l.grid() == reset.grid && l.capacity() == reset.capacity);
            if fits {
                self.native.as_mut().expect("lifecycle").clear(u64::from(reset.epoch)).map_err(|e| e.to_string())?;
            } else {
                self.native = None;
                self.native = Some(NativeLifecycle::new(reset.grid, reset.capacity, u64::from(reset.epoch)).map_err(|e| e.to_string())?);
            }
        }
        let native = self.native.as_mut().ok_or("no lifecycle: the first request resets it")?;
        for slot in &reply.snapshots {
            // SAFETY: the slot's frame retired and the slot is on loan to this
            // thread alone.
            let (fields, spawns, emitted) = unsafe { (slot.fields(), slot.spawns(), slot.emitted()) };
            native.set_fields(&fields).map_err(|e| e.to_string())?;
            let (_, load_thinned) = native.load(spawns).map_err(|e| e.to_string())?;
            reply.emitted += u64::from(emitted);
            reply.thinned += u64::from(emitted.saturating_sub(slot.shape.capacity)) + u64::from(load_thinned);
            for _ in 0..slot.ticks {
                native.step(TICK).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn write(&mut self, reply: &mut Reply) -> Result<(), String> {
        let Some(slot) = reply.output.as_mut() else {
            return Ok(());
        };
        let native = self.native.as_mut().ok_or("no lifecycle: the first request resets it")?;
        native.particles(&mut self.population).map_err(|e| e.to_string())?;
        match slot.write(&self.population) {
            Ok(()) => reply.written = true,
            Err(records) => reply.needs = Some(records),
        }
        Ok(())
    }
}

/// The lifecycle's own thread (D11). One request is in flight at most, so
/// neither channel ever blocks a send.
pub(crate) struct Worker {
    requests: SyncSender<Request>,
    replies: Receiver<Reply>,
}

impl Worker {
    pub fn spawn() -> Result<Self, String> {
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        let (sender, replies) = mpsc::sync_channel::<Reply>(1);
        std::thread::Builder::new()
            .name(LIFECYCLE_THREAD.into())
            .spawn(move || {
                let mut lifecycle = Lifecycle::default();
                while let Ok(request) = receiver.recv() {
                    if sender.send(lifecycle.process(request)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("the whitewater worker could not start: {e}"))?;
        Ok(Self { requests, replies })
    }

    /// Only while no request is in flight.
    pub fn send(&self, request: Request) -> Result<(), String> {
        self.requests.try_send(request).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => "a whitewater request was sent while one was in flight".to_owned(),
            mpsc::TrySendError::Disconnected(_) => "the whitewater worker stopped".to_owned(),
        })
    }

    /// The reply, if the worker finished. Never blocks.
    pub fn try_reply(&self) -> Result<Option<Reply>, String> {
        match self.replies.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("the whitewater worker stopped".into()),
        }
    }

    /// Offline only: block until the worker finishes.
    pub fn reply(&self) -> Result<Reply, String> {
        self.replies.recv().map_err(|_| "the whitewater worker stopped".to_owned())
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// D11: FLIP's update refuses any thread but its own, so it can never
    /// run on the content thread.
    #[test]
    #[should_panic(expected = "runs only on its own thread")]
    fn whitewater_update_refuses_other_threads() {
        let grid = WhitewaterGrid { cells: [4; 3], cell_size: 0.1, origin: [0.0; 3] };
        let request = Request { reset: Some(Reset { grid, capacity: 8, epoch: 0 }), snapshots: Vec::new(), output: None, hold: None };
        Lifecycle::default().process(request);
    }
}
