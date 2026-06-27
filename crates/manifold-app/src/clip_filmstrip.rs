//! Filmstrip bar→cell math (§24 5c-2), shared by the content-thread capture and
//! the UI draw so the two can never disagree about which cell holds which bar.
//!
//! A clip's filmstrip is `cell_count` cells laid left→right across the clip body.
//! At the base resolution each cell is **one bar**; for long clips the cells are
//! grouped into power-of-two bar runs so the count never exceeds [`FILMSTRIP_MAX_CELLS`]
//! (a 200-bar clip does not store 200 cells). All functions are pure, allocation-
//! free, and derive `cell_count` / `grouping` the same way, so a beat maps to the
//! same cell index on both sides and a cell maps back to the same beat range.

/// Max filmstrip cells a single clip occupies. Bounds atlas use per clip and caps
/// the draw cost for one clip; well under the atlas's total cell capacity so one
/// huge clip can't starve the pool.
pub const FILMSTRIP_MAX_CELLS: u32 = 64;

/// Total bars a clip of `duration_beats` spans at `beats_per_bar` (always ≥ 1).
#[inline]
pub fn clip_bar_count(duration_beats: f64, beats_per_bar: f64) -> u32 {
    if beats_per_bar <= 0.0 || !duration_beats.is_finite() {
        return 1;
    }
    ((duration_beats / beats_per_bar).ceil() as i64).clamp(1, u32::MAX as i64) as u32
}

/// Bars-per-cell grouping: the smallest power of two such that the resulting cell
/// count (`ceil(total_bars / grouping)`) is ≤ [`FILMSTRIP_MAX_CELLS`].
#[inline]
pub fn bar_grouping(total_bars: u32) -> u32 {
    let mut g = 1u32;
    while total_bars.div_ceil(g) > FILMSTRIP_MAX_CELLS {
        g = g.saturating_mul(2);
    }
    g
}

/// Number of filmstrip cells for a clip of `total_bars` (always ≥ 1).
#[inline]
pub fn cell_count(total_bars: u32) -> u32 {
    let g = bar_grouping(total_bars);
    total_bars.div_ceil(g).max(1)
}

/// Filmstrip cell index an absolute `beat` falls into, clamped to the clip's cell
/// range. `start_beat` is the clip's start; `beat` is absolute (project beats).
#[inline]
pub fn cell_index_at_beat(
    beat: f64,
    start_beat: f64,
    duration_beats: f64,
    beats_per_bar: f64,
) -> u32 {
    if beats_per_bar <= 0.0 {
        return 0;
    }
    let total = clip_bar_count(duration_beats, beats_per_bar);
    let g = bar_grouping(total);
    let count = total.div_ceil(g).max(1);
    let bars_in = ((beat - start_beat) / beats_per_bar).floor();
    if bars_in <= 0.0 {
        return 0;
    }
    ((bars_in as u32) / g).min(count - 1)
}

/// Absolute beat range `[start, end)` covered by filmstrip cell `cell`. The final
/// cell is clamped to the clip end (it may be a partial bar). `end ≥ start`.
#[inline]
pub fn cell_beat_range(
    cell: u32,
    start_beat: f64,
    duration_beats: f64,
    beats_per_bar: f64,
) -> (f64, f64) {
    let total = clip_bar_count(duration_beats, beats_per_bar);
    let g = bar_grouping(total);
    let cell_beats = beats_per_bar * g as f64;
    let s = start_beat + cell as f64 * cell_beats;
    let clip_end = start_beat + duration_beats;
    let e = (s + cell_beats).min(clip_end);
    (s, e.max(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BPB: f64 = 4.0;

    #[test]
    fn short_clip_one_cell_per_bar() {
        // 4-bar clip at 4 beats/bar → 4 cells, grouping 1.
        let dur = 16.0;
        assert_eq!(clip_bar_count(dur, BPB), 4);
        assert_eq!(bar_grouping(4), 1);
        assert_eq!(cell_count(4), 4);
    }

    #[test]
    fn partial_bar_rounds_up() {
        // 2.5 bars → 3 bars → 3 cells.
        assert_eq!(clip_bar_count(10.0, BPB), 3);
        assert_eq!(cell_count(clip_bar_count(10.0, BPB)), 3);
    }

    #[test]
    fn long_clip_groups_to_power_of_two() {
        // 200 bars > 64 → group by 4 → 50 cells (≤ 64).
        let total = 200;
        assert_eq!(bar_grouping(total), 4);
        assert_eq!(cell_count(total), 50);
        // 65 bars > 64 → group by 2 → 33 cells.
        assert_eq!(bar_grouping(65), 2);
        assert_eq!(cell_count(65), 33);
        // Exactly 64 → grouping 1.
        assert_eq!(bar_grouping(64), 1);
        assert_eq!(cell_count(64), 64);
    }

    #[test]
    fn beat_maps_to_expected_cell() {
        let (start, dur) = (8.0, 16.0); // bars 0..4 at beats 8,12,16,20
        assert_eq!(cell_index_at_beat(8.0, start, dur, BPB), 0);
        assert_eq!(cell_index_at_beat(11.9, start, dur, BPB), 0);
        assert_eq!(cell_index_at_beat(12.0, start, dur, BPB), 1);
        assert_eq!(cell_index_at_beat(20.0, start, dur, BPB), 3);
        // Before the clip clamps to 0, after the clip clamps to last.
        assert_eq!(cell_index_at_beat(0.0, start, dur, BPB), 0);
        assert_eq!(cell_index_at_beat(999.0, start, dur, BPB), 3);
    }

    #[test]
    fn cell_range_and_index_are_inverse() {
        let (start, dur) = (8.0, 16.0);
        for cell in 0..cell_count(clip_bar_count(dur, BPB)) {
            let (s, e) = cell_beat_range(cell, start, dur, BPB);
            assert!(e > s, "cell {cell} range empty");
            // The midpoint of a cell maps back to that cell.
            let mid = (s + e) * 0.5;
            assert_eq!(cell_index_at_beat(mid, start, dur, BPB), cell);
        }
    }

    #[test]
    fn grouped_cell_range_matches_index() {
        // 200-bar clip, grouping 4: cell 0 covers bars 0..3, cell 1 bars 4..7.
        let (start, dur) = (0.0, 200.0 * BPB);
        assert_eq!(bar_grouping(clip_bar_count(dur, BPB)), 4);
        let (s0, e0) = cell_beat_range(0, start, dur, BPB);
        assert_eq!((s0, e0), (0.0, 16.0)); // 4 bars × 4 beats
        assert_eq!(cell_index_at_beat(15.9, start, dur, BPB), 0);
        assert_eq!(cell_index_at_beat(16.0, start, dur, BPB), 1);
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert_eq!(clip_bar_count(0.0, BPB), 1);
        assert_eq!(clip_bar_count(16.0, 0.0), 1);
        assert_eq!(cell_index_at_beat(5.0, 0.0, 16.0, 0.0), 0);
        let (s, e) = cell_beat_range(0, 0.0, 0.0, BPB);
        assert!(e >= s);
    }
}
