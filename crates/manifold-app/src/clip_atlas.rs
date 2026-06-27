//! Clip-thumbnail **filmstrip** atlas cell allocation (§24 5c / 5c-2).
//!
//! Each clip owns a *strip* of atlas cells — one per filmstrip cell index (bar or
//! bar-group, see [`crate::clip_filmstrip`]). Cells are keyed by `(ClipId, cell
//! index)`, never by position — a timeline reorder keeps a clip's strip, the same
//! discipline as the waveform pool and the effect-chain pools. A clip's whole strip
//! persists after it goes off-screen so scrolling back doesn't re-snapshot; when the
//! atlas is full and a visible clip needs a cell, the **least-recently-visible
//! off-screen** clip is evicted *as a unit* (all its cells freed together, so a
//! strip is never left half-populated). If every cell is held by a currently-visible
//! clip, the newcomer cell gets `None` and the UI falls back to the clip's body
//! colour for that bar — graceful, no churn.
//!
//! Pure bookkeeping (no GPU): the content pipeline drives it each frame and does the
//! actual cell blits + layout publish.

use ahash::{AHashMap, AHashSet};
use manifold_core::ClipId;

/// Fixed-capacity `(ClipId, cell index) → atlas cell` allocator with whole-clip LRU
/// eviction of off-screen clips.
pub struct ClipAtlasCache {
    capacity: u32,
    /// clip → (filmstrip cell index → atlas cell index).
    strips: AHashMap<ClipId, AHashMap<u32, u32>>,
    /// Frame index each held clip was last in the visible set (eviction order).
    last_visible_frame: AHashMap<ClipId, u64>,
    /// Cells freed by eviction, ready for reuse (kept ahead of the high-water mark).
    free: Vec<u32>,
    /// Next never-used cell index (grows to `capacity`).
    high_water: u32,
    frame: u64,
    /// Clips visible this frame — protected from eviction.
    visible_now: AHashSet<ClipId>,
}

impl ClipAtlasCache {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            strips: AHashMap::new(),
            last_visible_frame: AHashMap::new(),
            free: Vec::new(),
            high_water: 0,
            frame: 0,
            visible_now: AHashSet::new(),
        }
    }

    /// Begin a frame: record the visible set so those clips are eviction-protected
    /// and their recency is refreshed.
    pub fn begin_frame(&mut self, visible: &[ClipId]) {
        self.frame = self.frame.wrapping_add(1);
        self.visible_now.clear();
        for c in visible {
            self.visible_now.insert(c.clone());
            self.last_visible_frame.insert(c.clone(), self.frame);
        }
    }

    /// The atlas cell holding `clip`'s filmstrip cell `idx`, without allocating.
    pub fn cell_for(&self, clip: &ClipId, idx: u32) -> Option<u32> {
        self.strips.get(clip).and_then(|s| s.get(&idx).copied())
    }

    /// The atlas cell for `clip`'s filmstrip cell `idx`, allocating one if needed.
    /// Returns `None` only when the atlas is full of *currently-visible* clips
    /// (nothing evictable).
    pub fn get_or_alloc(&mut self, clip: &ClipId, idx: u32) -> Option<u32> {
        if let Some(&cell) = self.strips.get(clip).and_then(|s| s.get(&idx)) {
            return Some(cell);
        }
        let cell = self.take_free_cell(clip)?;
        self.strips.entry(clip.clone()).or_default().insert(idx, cell);
        self.last_visible_frame.insert(clip.clone(), self.frame);
        Some(cell)
    }

    /// Does this clip hold any filmstrip cell? (Used to skip already-thumbnailed
    /// clips in the parked-clip fill pass.)
    pub fn contains_any(&self, clip: &ClipId) -> bool {
        self.strips.get(clip).is_some_and(|s| !s.is_empty())
    }

    /// Current full layout `(clip, cell index, atlas cell)` to publish to the UI.
    pub fn layout(&self) -> Vec<(ClipId, u32, u32)> {
        let mut out = Vec::with_capacity(self.strips.values().map(|s| s.len()).sum());
        for (clip, strip) in &self.strips {
            for (&idx, &cell) in strip {
                out.push((clip.clone(), idx, cell));
            }
        }
        out
    }

    /// Free a cell from the free list or the high-water mark, evicting the
    /// least-recently-visible off-screen clip (whole strip) when the atlas is full.
    /// `requester` is excluded from eviction so a clip can never evict itself
    /// mid-strip.
    fn take_free_cell(&mut self, requester: &ClipId) -> Option<u32> {
        if let Some(cell) = self.free.pop() {
            return Some(cell);
        }
        if self.high_water < self.capacity {
            let cell = self.high_water;
            self.high_water += 1;
            return Some(cell);
        }
        // Full: evict the least-recently-visible clip that isn't visible now and
        // isn't the requester. Freeing its whole strip refills `free`.
        let victim = self
            .strips
            .keys()
            .filter(|c| !self.visible_now.contains(*c) && *c != requester)
            .min_by_key(|c| self.last_visible_frame.get(*c).copied().unwrap_or(0))
            .cloned()?;
        self.evict_clip(&victim);
        self.free.pop()
    }

    /// Remove a clip's whole strip, returning its cells to the free list.
    fn evict_clip(&mut self, clip: &ClipId) {
        if let Some(strip) = self.strips.remove(clip) {
            self.free.extend(strip.into_values());
        }
        self.last_visible_frame.remove(clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(s: &str) -> ClipId {
        ClipId::new(s)
    }

    #[test]
    fn allocates_distinct_cells_per_bar_and_is_stable() {
        let mut c = ClipAtlasCache::new(8);
        c.begin_frame(&[cid("a")]);
        let a0 = c.get_or_alloc(&cid("a"), 0).unwrap();
        let a1 = c.get_or_alloc(&cid("a"), 1).unwrap();
        assert_ne!(a0, a1, "distinct bars get distinct cells");
        // Stable across frames.
        c.begin_frame(&[cid("a")]);
        assert_eq!(c.get_or_alloc(&cid("a"), 0), Some(a0));
        assert_eq!(c.get_or_alloc(&cid("a"), 1), Some(a1));
        assert_eq!(c.cell_for(&cid("a"), 0), Some(a0));
        assert_eq!(c.cell_for(&cid("a"), 2), None, "unallocated bar");
    }

    #[test]
    fn evicts_whole_clip_strip_when_full() {
        let mut c = ClipAtlasCache::new(2);
        c.begin_frame(&[cid("a")]);
        let a0 = c.get_or_alloc(&cid("a"), 0).unwrap();
        let a1 = c.get_or_alloc(&cid("a"), 1).unwrap();
        assert_ne!(a0, a1);
        // 'a' off-screen but still cached.
        c.begin_frame(&[cid("b")]);
        assert!(c.contains_any(&cid("a")));
        // 'b' needs a cell, atlas full → evict the whole 'a' strip, freeing 2 cells.
        let b0 = c.get_or_alloc(&cid("b"), 0).unwrap();
        assert!(!c.contains_any(&cid("a")), "a evicted as a unit");
        let b1 = c.get_or_alloc(&cid("b"), 1).unwrap();
        assert_ne!(b0, b1, "both freed cells reused");
    }

    #[test]
    fn never_evicts_a_visible_clip_or_the_requester() {
        let mut c = ClipAtlasCache::new(2);
        c.begin_frame(&[cid("a"), cid("b")]);
        c.get_or_alloc(&cid("a"), 0).unwrap();
        c.get_or_alloc(&cid("b"), 0).unwrap();
        // A third visible clip into a full 2-cell atlas: None, neither incumbent evicted.
        c.begin_frame(&[cid("a"), cid("b"), cid("e")]);
        assert_eq!(c.get_or_alloc(&cid("e"), 0), None);
        assert!(c.contains_any(&cid("a")));
        assert!(c.contains_any(&cid("b")));
        // A clip cannot evict itself to grow its own strip past capacity.
        c.begin_frame(&[cid("a"), cid("b")]);
        assert_eq!(c.get_or_alloc(&cid("a"), 5), None);
        assert!(c.contains_any(&cid("b")), "requester didn't evict a peer it shares with");
    }

    #[test]
    fn reuses_evicted_cells_without_growing() {
        let mut c = ClipAtlasCache::new(3);
        for name in ["a", "b", "c"] {
            c.begin_frame(&[cid(name)]);
            c.get_or_alloc(&cid(name), 0).unwrap();
        }
        for name in ["d", "e", "f"] {
            c.begin_frame(&[cid(name)]);
            let cell = c.get_or_alloc(&cid(name), 0).unwrap();
            assert!(cell < 3, "cell {cell} within capacity");
        }
    }

    #[test]
    fn layout_lists_every_allocated_cell() {
        let mut c = ClipAtlasCache::new(8);
        c.begin_frame(&[cid("a"), cid("b")]);
        c.get_or_alloc(&cid("a"), 0).unwrap();
        c.get_or_alloc(&cid("a"), 1).unwrap();
        c.get_or_alloc(&cid("b"), 0).unwrap();
        let layout = c.layout();
        assert_eq!(layout.len(), 3);
        assert!(layout.iter().any(|&(ref c, i, _)| c == &cid("a") && i == 0));
        assert!(layout.iter().any(|&(ref c, i, _)| c == &cid("a") && i == 1));
        assert!(layout.iter().any(|&(ref c, i, _)| c == &cid("b") && i == 0));
    }
}
