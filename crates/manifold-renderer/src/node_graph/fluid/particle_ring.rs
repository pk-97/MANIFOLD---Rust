//! Ring of CPU-written, GPU-read particle-frame slots
//! (GPU_FLUID_SURFACE_DESIGN.md D19). The content thread owns every slot
//! except the one loaned to the worker inside a `Request`. A slot is loaned
//! only after the last display frame that read it has retired on the GPU, a
//! non-blocking check against the content frame-completion clock. Live never
//! waits for a slot; offline may.

use manifold_fluids::{CaptureError, FluidWorld, ParticleFrameInfo};
use manifold_gpu::{FrameClock, GpuBuffer, GpuDevice};

use crate::node_graph::fluid_particles::{FluidParticle, as_records};

pub(crate) const RING_SLOTS: usize = 4;
/// First allocation before any frame reports its size. Growth re-captures.
const FIRST_PARTICLES: usize = 65_536;
const PARTICLE_ROUNDING: usize = 4_096;

/// One accepted capture: what the slot holds and for which tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotFrame {
    pub info: ParticleFrameInfo,
    pub tick: u64,
}

pub(crate) struct ParticleSlot {
    particles: GpuBuffer,
    solid: GpuBuffer,
    particle_capacity: usize,
    solid_capacity: usize,
    /// Frame-clock stamp of the last display frame that read this slot.
    read_stamp: u64,
    frame: Option<SlotFrame>,
}

impl ParticleSlot {
    fn allocate(device: &GpuDevice, particles: usize, solid: usize) -> Result<Self, String> {
        let particle_bytes = (particles.max(1) * std::mem::size_of::<FluidParticle>()) as u64;
        let solid_bytes = (solid.max(1) * std::mem::size_of::<f32>()) as u64;
        let admit = |bytes| {
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                bytes,
            )
            .map_err(|error| error.to_string())
        };
        admit(particle_bytes + solid_bytes)?;
        let particles_buffer = device
            .try_create_buffer_shared(particle_bytes)
            .map_err(|error| format!("Fluid particle frame needs {particle_bytes} bytes: {error}"))?;
        let solid_buffer = device
            .try_create_buffer_shared(solid_bytes)
            .map_err(|error| format!("Fluid solid lattice needs {solid_bytes} bytes: {error}"))?;
        Ok(Self {
            particles: particles_buffer,
            solid: solid_buffer,
            particle_capacity: particles.max(1),
            solid_capacity: solid.max(1),
            read_stamp: 0,
            frame: None,
        })
    }

    pub(crate) fn frame(&self) -> Option<SlotFrame> {
        self.frame
    }

    pub(crate) fn particles(&self) -> &GpuBuffer {
        &self.particles
    }

    pub(crate) fn solid(&self) -> &GpuBuffer {
        &self.solid
    }

    /// Worker side: write the world's last completed tick into this slot.
    /// `Capacity` leaves the slot empty with the required counts reported.
    pub(crate) fn capture(
        &mut self,
        world: &mut FluidWorld,
        offset: [f32; 3],
        tick: u64,
    ) -> Result<(), CaptureError> {
        self.frame = None;
        let particle_ptr = self.particles.mapped_ptr().expect("particle slots are shared storage");
        let solid_ptr = self.solid.mapped_ptr().expect("particle slots are shared storage");
        // SAFETY: both buffers are CPU-visible shared storage of exactly
        // these capacities. The ring loans a slot to one worker only after
        // every GPU read of it retired, and the content thread does not
        // touch a loaned slot, so these are the only live references.
        let (particles, solid) = unsafe {
            (
                std::slice::from_raw_parts_mut(particle_ptr.cast::<FluidParticle>(), self.particle_capacity),
                std::slice::from_raw_parts_mut(solid_ptr.cast::<f32>(), self.solid_capacity),
            )
        };
        let info = world.capture_particle_frame(offset, as_records(particles), solid)?;
        self.frame = Some(SlotFrame { info, tick });
        Ok(())
    }

    fn fits(&self, particles: usize, solid: usize) -> bool {
        self.particle_capacity >= particles && self.solid_capacity >= solid
    }
}

#[derive(Default)]
pub(crate) struct ParticleRing {
    clock: Option<FrameClock>,
    free: Vec<ParticleSlot>,
    /// Slots allocated for this ring, wherever they are (free, A, B, loaned).
    allocated: usize,
    a: Option<ParticleSlot>,
    b: Option<ParticleSlot>,
    particle_target: usize,
    solid_target: usize,
    /// Bumped whenever the published pair changes.
    pub version: u64,
}

impl ParticleRing {
    /// Allocate up to [`RING_SLOTS`] and regrow free slots below the targets
    /// once their last reader retired. GPU allocation happens only here, on
    /// the content thread.
    pub(crate) fn prepare(&mut self, device: &GpuDevice, solid_nodes: usize) -> Result<(), String> {
        if self.clock.is_none() {
            self.clock = device.frame_clock();
        }
        self.solid_target = self.solid_target.max(solid_nodes);
        if self.particle_target == 0 {
            self.particle_target = FIRST_PARTICLES;
        }
        for index in 0..self.free.len() {
            let regrow = {
                let slot = &self.free[index];
                !slot.fits(self.particle_target, self.solid_target) && self.retired(slot)
            };
            if regrow {
                self.free[index] = ParticleSlot::allocate(device, self.particle_target, self.solid_target)?;
            }
        }
        while self.allocated < RING_SLOTS {
            self.free.push(ParticleSlot::allocate(device, self.particle_target, self.solid_target)?);
            self.allocated += 1;
        }
        Ok(())
    }

    fn retired(&self, slot: &ParticleSlot) -> bool {
        self.clock.as_ref().is_none_or(|clock| clock.is_complete(slot.read_stamp))
    }

    /// Loan a retired slot large enough for the current targets, if any.
    /// Live: never waits. Offline (`wait`): when every fitting slot is still
    /// being read, block on the oldest reader — an earlier, committed frame.
    pub(crate) fn take(&mut self, wait: bool) -> Option<ParticleSlot> {
        let fitting = |slot: &ParticleSlot| slot.fits(self.particle_target, self.solid_target);
        if let Some(index) = self.free.iter().position(|slot| fitting(slot) && self.retired(slot)) {
            return Some(self.free.swap_remove(index));
        }
        if !wait {
            return None;
        }
        let (index, stamp) = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, slot)| fitting(slot))
            .map(|(index, slot)| (index, slot.read_stamp))
            .min_by_key(|(_, stamp)| *stamp)?;
        let clock = self.clock.as_ref()?;
        clock.wait(stamp).then(|| self.free.swap_remove(index))
    }

    /// A slot came back from the worker. Publish it when it holds a frame
    /// for the current epoch; otherwise return it to the free list.
    pub(crate) fn accept(&mut self, mut slot: ParticleSlot, publish: bool) {
        if !publish || slot.frame.is_none() {
            slot.frame = None;
            self.free.push(slot);
            return;
        }
        if let Some(mut oldest) = self.a.take() {
            oldest.frame = None;
            self.free.push(oldest);
        }
        self.a = self.b.take();
        self.b = Some(slot);
        self.version = self.version.wrapping_add(1);
    }

    /// The worker needed more room. Later loans use the grown size; the
    /// runtime re-captures the same tick.
    pub(crate) fn grow(&mut self, particles: u32, solid: usize) {
        let wanted = (particles as usize).saturating_mul(5) / 4;
        self.particle_target = self
            .particle_target
            .max(wanted.div_ceil(PARTICLE_ROUNDING) * PARTICLE_ROUNDING);
        self.solid_target = self.solid_target.max(solid);
    }

    /// Stamp the published pair with the frame now being encoded.
    pub(crate) fn mark_read(&mut self) {
        let Some(stamp) = self.clock.as_ref().map(FrameClock::stamp) else {
            return;
        };
        for slot in [self.a.as_mut(), self.b.as_mut()].into_iter().flatten() {
            slot.read_stamp = stamp;
        }
    }

    /// Drop the published frames (reset, epoch change). Slots keep their
    /// read stamps: in-flight frames may still be reading them.
    pub(crate) fn clear(&mut self) {
        for mut slot in [self.a.take(), self.b.take()].into_iter().flatten() {
            slot.frame = None;
            self.free.push(slot);
        }
        self.version = self.version.wrapping_add(1);
    }

    /// The newest frame (B) and the one before it (A). With one frame both
    /// are that frame.
    pub(crate) fn pair(&self) -> Option<(&ParticleSlot, &ParticleSlot)> {
        let b = self.b.as_ref()?;
        Some((self.a.as_ref().unwrap_or(b), b))
    }

    pub(crate) fn newest_tick(&self) -> Option<u64> {
        self.b.as_ref().and_then(|slot| slot.frame).map(|frame| frame.tick)
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn set_clock_for_test(&mut self, clock: FrameClock) {
        self.clock = Some(clock);
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn set_particle_target_for_test(&mut self, particles: usize) {
        self.particle_target = particles;
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(crate) fn free_len(&self) -> usize {
        self.free.len()
    }
}
