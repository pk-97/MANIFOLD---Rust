//! Lock-free SPSC ring of raw audio samples (f32).
//!
//! Classic Lamport-style SPSC queue. The audio thread pushes mono samples
//! (already M/S-decoded); the GUI thread drains them into its rolling
//! FFT/CQT buffer. The ring is sized to tolerate ~1 s of GUI stall before
//! the producer starts dropping samples (which shows up as a brief time
//! gap in the spectrogram — never an audio-thread stall).
//!
//! Capacity is rounded up to a power of two so the wrap mask is a single
//! bitwise AND. One slot is kept empty to disambiguate full from empty.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct SampleRing {
    buf: UnsafeCell<Vec<f32>>,
    capacity: usize, // always a power of two
    mask: usize,
    write_idx: AtomicUsize,
    read_idx: AtomicUsize,
}

// SAFETY: producer/consumer writes are partitioned by atomic indices;
// the two never touch the same byte simultaneously.
unsafe impl Sync for SampleRing {}

impl SampleRing {
    pub fn new(min_capacity: usize) -> Self {
        let capacity = min_capacity.max(2).next_power_of_two();
        Self {
            buf: UnsafeCell::new(vec![0.0; capacity]),
            capacity,
            mask: capacity - 1,
            write_idx: AtomicUsize::new(0),
            read_idx: AtomicUsize::new(0),
        }
    }

    /// Producer: copy up to `samples.len()` values into the ring.
    /// Returns the number actually written (< len if the ring was
    /// running out of space). Audio thread never blocks; surplus
    /// samples are dropped and read as a brief time gap in the
    /// spectrogram.
    pub fn push(&self, samples: &[f32]) -> usize {
        let write = self.write_idx.load(Ordering::Relaxed);
        let read = self.read_idx.load(Ordering::Acquire);
        // Keep one slot empty so write == read only means "empty".
        let free = self.capacity - 1 - write.wrapping_sub(read);
        let to_write = samples.len().min(free);
        if to_write == 0 {
            return 0;
        }
        // SAFETY: producer has exclusive access to indices `[write, write+to_write)`
        // because consumer is at `read` and the gap-preservation check above
        // ensures we never cross into the consumer's active range.
        unsafe {
            let buf = &mut *self.buf.get();
            let start = write & self.mask;
            let first_len = (self.capacity - start).min(to_write);
            buf[start..start + first_len]
                .copy_from_slice(&samples[..first_len]);
            if first_len < to_write {
                let remainder = to_write - first_len;
                buf[..remainder].copy_from_slice(&samples[first_len..to_write]);
            }
        }
        self.write_idx.store(write + to_write, Ordering::Release);
        to_write
    }

    /// Consumer: drain all pending samples into `dst` (append). Returns
    /// the number of samples drained. Uses `extend_from_slice` so `dst`
    /// may grow — this is the GUI thread only; audio thread never calls.
    pub fn drain_into(&self, dst: &mut Vec<f32>) -> usize {
        let write = self.write_idx.load(Ordering::Acquire);
        let read = self.read_idx.load(Ordering::Relaxed);
        let count = write - read;
        if count == 0 {
            return 0;
        }
        // SAFETY: consumer has exclusive access to indices `[read, write)`
        // because producer is at `write`.
        unsafe {
            let buf = &*self.buf.get();
            let start = read & self.mask;
            let first_len = (self.capacity - start).min(count);
            dst.extend_from_slice(&buf[start..start + first_len]);
            if first_len < count {
                dst.extend_from_slice(&buf[..count - first_len]);
            }
        }
        self.read_idx.store(write, Ordering::Release);
        count
    }
}
