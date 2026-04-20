//! Lock-free SPSC ring of raw FFT frames.
//!
//! Each slot carries two parallel `num_bins`-length arrays — the dB
//! spectrum and the per-bin instantaneous frequency (Hz). The spectrogram
//! needs both: the dB tells us *how much* energy, the freq tells us
//! *where* it really is (for spectral reassignment).
//!
//! The audio thread produces frames at the FFT hop rate (~117 Hz at
//! 16384 / 97.5 % / 48 kHz); the GUI thread drains at render rate
//! (~60 Hz). A single-slot mailbox drops every other frame; this ring
//! preserves all frames up to its capacity so every FFT hop becomes a
//! spectrogram column.
//!
//! Safety: classic Lamport-style SPSC queue. One producer, one consumer,
//! bounded. If the reader falls behind by `capacity` frames the producer
//! returns `false` and the frame is dropped (audio thread never blocks).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One spectrogram frame: dB spectrum + per-bin instantaneous frequency.
pub struct Frame {
    pub db: Vec<f32>,
    pub inst_freqs: Vec<f32>,
}

pub struct RawFrameRing {
    // Pre-allocated, never re-grown.
    slots: Vec<UnsafeCell<Frame>>,
    capacity: usize,
    write_idx: AtomicUsize,
    read_idx: AtomicUsize,
}

// SAFETY: we partition access between producer (writes to `write_idx`
// slot) and consumer (reads from `read_idx` slot) via atomic indices,
// and the two never touch the same slot simultaneously.
unsafe impl Sync for RawFrameRing {}

impl RawFrameRing {
    pub fn new(capacity: usize, num_bins: usize, db_fill: f32, freq_fill: f32) -> Self {
        assert!(capacity >= 2, "SPSC ring needs at least 2 slots");
        let slots = (0..capacity)
            .map(|_| {
                UnsafeCell::new(Frame {
                    db: vec![db_fill; num_bins],
                    inst_freqs: vec![freq_fill; num_bins],
                })
            })
            .collect();
        Self {
            slots,
            capacity,
            write_idx: AtomicUsize::new(0),
            read_idx: AtomicUsize::new(0),
        }
    }

    /// Producer: copy `db` + `inst_freqs` into the next slot. Returns
    /// `false` if the ring is full (one slot is always kept empty to
    /// distinguish full from empty).
    pub fn push(&self, db: &[f32], inst_freqs: &[f32]) -> bool {
        let write = self.write_idx.load(Ordering::Relaxed);
        let next = (write + 1) % self.capacity;
        if next == self.read_idx.load(Ordering::Acquire) {
            return false;
        }
        // SAFETY: slot at `write` is only touched by this producer while
        // `write_idx` points here. The consumer's window is `[read, write)`
        // in ring order, so this slot is outside it.
        unsafe {
            let slot = &mut *self.slots[write].get();
            debug_assert_eq!(slot.db.len(), db.len());
            debug_assert_eq!(slot.inst_freqs.len(), inst_freqs.len());
            slot.db.copy_from_slice(db);
            slot.inst_freqs.copy_from_slice(inst_freqs);
        }
        self.write_idx.store(next, Ordering::Release);
        true
    }

    /// Consumer: call `f` with every pending frame in order, advancing
    /// the read head after each successful call.
    pub fn drain<F: FnMut(&[f32], &[f32])>(&self, mut f: F) {
        let write = self.write_idx.load(Ordering::Acquire);
        let mut read = self.read_idx.load(Ordering::Relaxed);
        while read != write {
            // SAFETY: slot at `read` is only touched by this consumer
            // while `read_idx` points here; producer is at `write`.
            let slot = unsafe { &*self.slots[read].get() };
            f(&slot.db, &slot.inst_freqs);
            read = (read + 1) % self.capacity;
        }
        self.read_idx.store(read, Ordering::Release);
    }
}
