//! Clip-thumbnail atlas cell allocation (§24 5c).
//!
//! Maps each thumbnailed clip to a fixed atlas cell, keyed by `ClipId` (never by
//! position — a timeline reorder keeps a clip's thumbnail, same discipline as the
//! waveform pool and the effect-chain pools). Cells persist after a clip goes
//! off-screen so scrolling back doesn't re-snapshot; when all cells are taken and
//! a new visible clip needs one, the **least-recently-visible off-screen** clip is
//! evicted. If every cell is held by a currently-visible clip (more visible
//! thumbnail clips than the atlas holds), the newcomer gets `None` and the UI
//! falls back to the clip's body colour — graceful, no churn.
//!
//! This is pure bookkeeping (no GPU): the content pipeline drives it each frame
//! and does the actual cell blits + layout publish.

use ahash::AHashMap;
use manifold_core::ClipId;

/// Fixed-capacity ClipId→cell allocator with LRU eviction of off-screen clips.
pub struct ClipAtlasCache {
    capacity: u32,
    cell_of: AHashMap<ClipId, u32>,
    /// Frame index each held clip was last in the visible set (eviction order).
    last_visible_frame: AHashMap<ClipId, u64>,
    /// Cells freed by eviction, ready for reuse (kept ahead of the high-water mark).
    free: Vec<u32>,
    /// Next never-used cell index (grows to `capacity`).
    high_water: u32,
    frame: u64,
    /// Clips visible this frame — protected from eviction.
    visible_now: ahash::AHashSet<ClipId>,
}

impl ClipAtlasCache {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            cell_of: AHashMap::new(),
            last_visible_frame: AHashMap::new(),
            free: Vec::new(),
            high_water: 0,
            frame: 0,
            visible_now: ahash::AHashSet::new(),
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

    /// The cell for `clip`, allocating one if needed. Returns `None` only when the
    /// atlas is full of *currently-visible* clips (nothing evictable).
    pub fn get_or_alloc(&mut self, clip: &ClipId) -> Option<u32> {
        if let Some(&cell) = self.cell_of.get(clip) {
            return Some(cell);
        }
        let cell = self.take_free_cell()?;
        self.cell_of.insert(clip.clone(), cell);
        self.last_visible_frame.insert(clip.clone(), self.frame);
        Some(cell)
    }

    /// Is this clip already holding a cell? (Used by the cold-start pass to skip
    /// clips that already have a thumbnail.)
    pub fn contains(&self, clip: &ClipId) -> bool {
        self.cell_of.contains_key(clip)
    }

    /// Current full layout `(clip, cell)` to publish to the UI.
    pub fn layout(&self) -> Vec<(ClipId, u32)> {
        self.cell_of.iter().map(|(c, &i)| (c.clone(), i)).collect()
    }

    /// Free a cell from the high-water mark or the free list. Evicts the
    /// least-recently-visible off-screen clip when the atlas is full.
    fn take_free_cell(&mut self) -> Option<u32> {
        if let Some(cell) = self.free.pop() {
            return Some(cell);
        }
        if self.high_water < self.capacity {
            let cell = self.high_water;
            self.high_water += 1;
            return Some(cell);
        }
        // Full: evict the least-recently-visible clip that isn't visible now.
        let victim = self
            .cell_of
            .keys()
            .filter(|c| !self.visible_now.contains(*c))
            .min_by_key(|c| self.last_visible_frame.get(*c).copied().unwrap_or(0))
            .cloned()?;
        let cell = self.cell_of.remove(&victim)?;
        self.last_visible_frame.remove(&victim);
        Some(cell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(s: &str) -> ClipId {
        ClipId::new(s)
    }

    #[test]
    fn allocates_distinct_cells_and_is_stable() {
        let mut c = ClipAtlasCache::new(4);
        c.begin_frame(&[cid("a"), cid("b")]);
        let a = c.get_or_alloc(&cid("a")).unwrap();
        let b = c.get_or_alloc(&cid("b")).unwrap();
        assert_ne!(a, b);
        // Stable across frames — same cell for the same clip.
        c.begin_frame(&[cid("a"), cid("b")]);
        assert_eq!(c.get_or_alloc(&cid("a")), Some(a));
        assert_eq!(c.get_or_alloc(&cid("b")), Some(b));
    }

    #[test]
    fn persists_offscreen_then_evicts_least_recently_visible() {
        let mut c = ClipAtlasCache::new(2);
        c.begin_frame(&[cid("a")]);
        let a = c.get_or_alloc(&cid("a")).unwrap();
        c.begin_frame(&[cid("b")]);
        let b = c.get_or_alloc(&cid("b")).unwrap();
        assert_ne!(a, b);
        // 'a' is off-screen but still cached (persisted).
        assert!(c.contains(&cid("a")));
        // New clip 'd' with the atlas full → evicts 'a' (least recently visible).
        c.begin_frame(&[cid("b"), cid("d")]);
        let d = c.get_or_alloc(&cid("d")).unwrap();
        assert_eq!(d, a, "freed cell is reused");
        assert!(!c.contains(&cid("a")), "a was evicted");
        assert!(c.contains(&cid("b")));
    }

    #[test]
    fn never_evicts_a_visible_clip() {
        let mut c = ClipAtlasCache::new(2);
        c.begin_frame(&[cid("a"), cid("b")]);
        c.get_or_alloc(&cid("a")).unwrap();
        c.get_or_alloc(&cid("b")).unwrap();
        // Three visible clips into a 2-cell atlas: the third gets None, but neither
        // visible incumbent is evicted.
        c.begin_frame(&[cid("a"), cid("b"), cid("e")]);
        assert_eq!(c.get_or_alloc(&cid("e")), None);
        assert!(c.contains(&cid("a")));
        assert!(c.contains(&cid("b")));
    }

    #[test]
    fn reuses_evicted_cells_without_growing() {
        let mut c = ClipAtlasCache::new(3);
        for name in ["a", "b", "c"] {
            c.begin_frame(&[cid(name)]);
            c.get_or_alloc(&cid(name)).unwrap();
        }
        // All three cells used once; cycle a fourth/fifth through — cells reused,
        // never exceeding capacity.
        for name in ["d", "e", "f"] {
            c.begin_frame(&[cid(name)]);
            let cell = c.get_or_alloc(&cid(name)).unwrap();
            assert!(cell < 3, "cell {cell} within capacity");
        }
    }
}
