//! The A/B frame ring of the particle-frame seam
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.1, amendment 1): after
//! each simulated tick the new frame becomes B and the previous B becomes A;
//! the display blends between them one tick behind. A restart or a regrown
//! ring collapses A onto the new frame.

use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::node_graph::fluid::display_blend;

/// Frames in the ring: A, B and the one being written.
pub const RING: usize = 3;

/// Where one tick's frame goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingWrite {
    /// The slot this tick writes.
    pub write: usize,
    /// The slot holding the previous tick (B before this write).
    pub previous: usize,
    /// Records in the previous slot; 0 when the ring was just regrown.
    pub previous_count: u32,
    /// The ring was reallocated for this tick.
    pub grown: bool,
    pub restarted: bool,
}

#[derive(Default)]
pub struct FrameRing {
    slots: Vec<GpuBuffer>,
    counts: [u32; RING],
    a: usize,
    b: usize,
    t_a: f64,
    t_b: f64,
    epoch: Option<u32>,
}

impl FrameRing {
    /// Whether this frame carries a tick to publish: a new epoch, or
    /// simulated time past the newest frame.
    pub fn wants_tick(&self, epoch: u32, simulation_time: f64) -> bool {
        self.epoch != Some(epoch) || simulation_time > self.t_b
    }

    /// Force the next publication to collapse A onto its newly-latticed B.
    /// A lattice change makes an older particle frame incomparable even when
    /// the simulation clock did not advance.
    pub fn invalidate(&mut self) {
        self.epoch = None;
    }

    /// Start writing one tick of `bytes` per slot. Slots too small for it are
    /// replaced with fresh shared storage (capture and look metrics read
    /// frames back), and the previous frame then counts as empty. A ring the
    /// device cannot give is refused and the old slots stay.
    pub fn begin(&mut self, device: &GpuDevice, bytes: u64, epoch: u32) -> Result<RingWrite, String> {
        let mut grown = false;
        if self.slots.len() < RING || self.slots.iter().any(|s| s.size < bytes) {
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), RING as u64 * bytes)
                .map_err(|error| error.to_string())?;
            self.slots = (0..RING).map(|_| device.try_create_buffer_shared(bytes)).collect::<Result<_, _>>()?;
            self.counts = [0; RING];
            grown = true;
        }
        let write = (0..RING).find(|&i| i != self.a && i != self.b).unwrap_or(0);
        let write = if self.a == self.b { (self.b + 1) % RING } else { write };
        let previous = self.b;
        Ok(RingWrite {
            write,
            previous,
            previous_count: if grown { 0 } else { self.counts[previous] },
            grown,
            restarted: self.epoch != Some(epoch),
        })
    }

    /// The slot at `index` (a [`RingWrite`] slot, or A or B).
    pub fn slot(&self, index: usize) -> &GpuBuffer {
        &self.slots[index]
    }

    /// The written tick becomes B with `count` records at `simulation_time`.
    pub fn finish(&mut self, write: RingWrite, count: u32, epoch: u32, simulation_time: f64) {
        self.counts[write.write] = count;
        if write.restarted || write.grown {
            self.a = write.write;
            self.t_a = simulation_time;
        } else {
            self.a = self.b;
            self.t_a = self.t_b;
        }
        self.b = write.write;
        self.t_b = simulation_time;
        self.epoch = Some(epoch);
    }

    /// Slot indices of frames A and B.
    pub fn a(&self) -> usize {
        self.a
    }

    pub fn b(&self) -> usize {
        self.b
    }

    pub fn buffer_a(&self) -> Option<&GpuBuffer> {
        self.slots.get(self.a)
    }

    pub fn buffer_b(&self) -> Option<&GpuBuffer> {
        self.slots.get(self.b)
    }

    pub fn count_a(&self) -> u32 {
        self.counts[self.a]
    }

    pub fn count_b(&self) -> u32 {
        self.counts[self.b]
    }

    /// The display blend and span at `display_time` (surface design D10).
    pub fn blend(&self, display_time: f64) -> (f32, f32) {
        display_blend(display_time, self.t_a, self.t_b)
    }
}
