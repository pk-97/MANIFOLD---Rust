//! Trim/target bar geometry shared by driver, Ableton, and audio-mod trims.
//! Split out of `param_slider_shared` (P-S1, UI funnel decomposition).

use super::*;


/// The ONE trim-geometry source (BUG-258): the fill + bar rects for a trim
/// range on a track. Build, reposition, and hit-zone math all derive from
/// this, so the grabbable zone can never drift from the drawn handle.
pub(crate) struct TrimBarRects {
    pub fill: Rect,
    pub min_bar: Rect,
    pub max_bar: Rect,
}


pub(crate) fn trim_bar_rects(track_rect: Rect, min: f32, max: f32) -> TrimBarRects {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    TrimBarRects {
        fill: Rect::new(
            base_x + min * usable,
            track_rect.y + OVERLAY_INSET,
            (max - min) * usable,
            track_rect.height - OVERLAY_INSET * 2.0,
        ),
        min_bar: Rect::new(base_x + min * usable - TRIM_BAR_W * 0.5, track_rect.y, TRIM_BAR_W, track_rect.height),
        max_bar: Rect::new(base_x + max * usable - TRIM_BAR_W * 0.5, track_rect.y, TRIM_BAR_W, track_rect.height),
    }
}


/// The envelope target bar's rect for a depth `norm` on a track — the
/// single geometry source for build, drag-reposition, and hit-zone math
/// (same anti-drift contract as [`trim_bar_rects`], BUG-258).
pub(crate) fn target_bar_rect(track_rect: Rect, norm: f32) -> Rect {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    Rect::new(
        base_x + norm * usable - TARGET_BAR_W * 0.5,
        track_rect.y - 2.0,
        TARGET_BAR_W,
        track_rect.height + 4.0,
    )
}


/// Reposition a trim overlay's three nodes (fill + min/max bars) along a slider
/// track for a new `[min, max]`. The pixel math is identical for driver,
/// Ableton, and audio trims — this is the single copy they all share, so a
/// layout tweak lands once instead of drifting across three near-identical
/// blocks.
pub(crate) fn reposition_trim_bars(
    tree: &mut UITree,
    track_rect: Rect,
    ids: &TrimHandleIds,
    new_min: f32,
    new_max: f32,
) {
    let r = trim_bar_rects(track_rect, new_min, new_max);
    tree.set_bounds(ids.fill_id, r.fill);
    tree.set_bounds(ids.min_bar_id, r.min_bar);
    tree.set_bounds(ids.max_bar_id, r.max_bar);
}

