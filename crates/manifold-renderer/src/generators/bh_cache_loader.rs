// Async loader for the Black Hole deflection cache.
//
// Follows the MRI volume generator pattern: file I/O and LZ4 decompression
// happen on a background thread; the render loop polls a channel for results
// and uploads completed entries to GPU textures.
//
// At any time the loader holds up to 4 entries in CPU memory (one per
// neighbor slot). Slots are reused as the camera moves through the grid.

use super::bh_cache::{BhCacheHeader, BhCacheReader, GridNeighbors, find_neighbors};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};

/// Result delivered from the I/O thread back to the render loop.
struct LoadResult {
    grid_index: usize,
    slot: usize,
    data: Result<Vec<u8>, String>,
}

/// Per-slot CPU-side state.
#[derive(Default)]
struct SlotState {
    /// Grid index currently held in this slot, or `usize::MAX` if empty.
    grid_index: usize,
    /// Raw decompressed bytes ready for GPU upload, or `None` if already uploaded.
    pending_upload: Option<Vec<u8>>,
}

impl SlotState {
    fn new_empty() -> Self {
        Self {
            grid_index: usize::MAX,
            pending_upload: None,
        }
    }
}

pub struct BhCacheLoader {
    reader: BhCacheReader,
    /// 4 slots, indexed [tl, tr, bl, br] in the order returned by `find_neighbors`.
    slots: [SlotState; 4],
    /// One channel kept for the lifetime of the loader; results stream in.
    rx: Receiver<LoadResult>,
    tx: std::sync::mpsc::Sender<LoadResult>,
    /// Number of loads currently in flight (slots being filled by background threads).
    in_flight: u32,
    /// Tracks whether each slot has an outstanding load — prevents double-spawning.
    slot_loading: [bool; 4],
    /// Cached neighbors from the last `update_for()` call.
    last_neighbors: Option<GridNeighbors>,
}

impl BhCacheLoader {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let reader = BhCacheReader::open(path)?;
        let (tx, rx) = channel();
        Ok(Self {
            reader,
            slots: [
                SlotState::new_empty(),
                SlotState::new_empty(),
                SlotState::new_empty(),
                SlotState::new_empty(),
            ],
            rx,
            tx,
            in_flight: 0,
            slot_loading: [false; 4],
            last_neighbors: None,
        })
    }

    pub fn header(&self) -> &BhCacheHeader {
        self.reader.header()
    }

    pub fn last_neighbors(&self) -> Option<&GridNeighbors> {
        self.last_neighbors.as_ref()
    }

    /// Returns true if all 4 neighbor slots currently hold the entries
    /// matching `last_neighbors` (i.e. the GPU side has valid data to blend).
    pub fn neighbors_ready(&self) -> bool {
        let Some(n) = &self.last_neighbors else {
            return false;
        };
        for slot in 0..4 {
            if self.slots[slot].grid_index != n.indices[slot] {
                return false;
            }
            if self.slots[slot].pending_upload.is_some() {
                // Still waiting for the consumer to upload to GPU.
                return false;
            }
        }
        true
    }

    /// Pull pending bytes for slot `slot`, if any. Caller must upload to GPU
    /// and the slot will be marked as resident on next call to `neighbors_ready`.
    pub fn take_pending_upload(&mut self, slot: usize) -> Option<Vec<u8>> {
        self.slots[slot].pending_upload.take()
    }

    /// Return the grid index currently resident in slot `slot`, or `usize::MAX` if empty.
    pub fn slot_index(&self, slot: usize) -> usize {
        self.slots[slot].grid_index
    }

    /// Update the loader for a new query (cam_dist, tilt). Spawns background
    /// loads for any neighbors not already resident. Non-blocking.
    pub fn update_for(&mut self, cam_dist: f32, tilt_deg: f32) {
        let neighbors = find_neighbors(self.reader.header(), cam_dist, tilt_deg);

        // Drain completed loads first so we have an accurate picture of slot state.
        self.poll();

        // Determine which slots need new loads. We use a simple matching:
        // for each desired neighbor index, find a slot already holding it,
        // otherwise pick a slot that isn't currently loading and spawn.
        let desired = neighbors.indices;
        let mut new_slots: [usize; 4] = [usize::MAX; 4];

        // First pass: match slots that already hold desired indices.
        let mut taken = [false; 4];
        for (target_slot, want) in desired.iter().enumerate() {
            for (src_slot, slot) in self.slots.iter().enumerate() {
                if !taken[src_slot] && slot.grid_index == *want {
                    new_slots[target_slot] = src_slot;
                    taken[src_slot] = true;
                    break;
                }
            }
        }

        // Second pass: assign remaining target slots to free source slots.
        // We need to remap slots so that `slots[target] = data for indices[target]`.
        // Build a destination layout: for each target, either reuse the matched
        // source slot or pick a free one to load into.
        let mut new_state: [SlotState; 4] = [
            SlotState::new_empty(),
            SlotState::new_empty(),
            SlotState::new_empty(),
            SlotState::new_empty(),
        ];
        let mut new_loading = [false; 4];

        // First, copy matched slots into their new positions.
        for (target_slot, src) in new_slots.iter().enumerate() {
            if *src != usize::MAX {
                let s = std::mem::replace(&mut self.slots[*src], SlotState::new_empty());
                let was_loading = self.slot_loading[*src];
                new_state[target_slot] = s;
                new_loading[target_slot] = was_loading;
            }
        }

        // Now spawn loads for any unmatched targets.
        for target_slot in 0..4 {
            if new_slots[target_slot] != usize::MAX {
                continue;
            }
            let want = desired[target_slot];
            new_state[target_slot] = SlotState {
                grid_index: usize::MAX,
                pending_upload: None,
            };
            new_loading[target_slot] = true;
            self.spawn_load(want, target_slot);
        }

        self.slots = new_state;
        self.slot_loading = new_loading;
        self.last_neighbors = Some(neighbors);
    }

    fn spawn_load(&mut self, grid_index: usize, slot: usize) {
        let tx = self.tx.clone();
        // Cheap Arc clone — fd is shared, reads are positioned (pread).
        let reader = self.reader.clone();
        self.in_flight += 1;
        std::thread::spawn(move || {
            let res = reader
                .read_entry(grid_index)
                .map_err(|e| format!("read_entry({grid_index}): {e}"));
            let _ = tx.send(LoadResult {
                grid_index,
                slot,
                data: res,
            });
        });
    }

    /// Drain any completed loads from the channel into slot state. Non-blocking.
    pub fn poll(&mut self) {
        while let Ok(result) = self.rx.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            if result.slot >= 4 {
                continue;
            }
            self.slot_loading[result.slot] = false;
            match result.data {
                Ok(data) => {
                    self.slots[result.slot] = SlotState {
                        grid_index: result.grid_index,
                        pending_upload: Some(data),
                    };
                }
                Err(e) => {
                    log::error!("bhcache load failed: {e}");
                }
            }
        }
    }

    /// Block until at least one pending load completes. Used during startup
    /// to populate initial neighbors before the first frame.
    pub fn block_until_any(&mut self) {
        if self.in_flight == 0 {
            return;
        }
        if let Ok(result) = self.rx.recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            if result.slot < 4 {
                self.slot_loading[result.slot] = false;
                match result.data {
                    Ok(data) => {
                        self.slots[result.slot] = SlotState {
                            grid_index: result.grid_index,
                            pending_upload: Some(data),
                        };
                    }
                    Err(e) => {
                        log::error!("bhcache load failed: {e}");
                    }
                }
            }
        }
        // Drain any others that completed in the meantime.
        self.poll();
    }

    /// True if any loads are still in progress.
    pub fn loads_in_flight(&self) -> bool {
        self.in_flight > 0
    }
}
