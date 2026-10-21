//! The track grid as GPU lines, drawn once per frame under the clip bodies.
//!
//! The grid is a pure function of the viewport, the same for every layer, so
//! it is one set of vertical lines across the visible layer rows, not a
//! bitmap per layer. The walker is `timing_grid_lines`, shared with the ruler
//! and the automation lanes. Nothing here is cached.

use manifold_ui::color;
use manifold_ui::timing_grid::{TimelineGridView, TimingGridLineKind, timing_grid_lines};

use crate::ui_renderer::UIRenderer;

/// Screen x and kind of every grid line the view shows, left to right. The
/// renderer snaps each line to a whole physical pixel column (`draw_grid_line`).
/// A line that would start left of the tracks is dropped whole rather than clipped
/// to its second pixel: the grid has always started at the first column.
fn grid_line_xs(view: &TimelineGridView) -> impl Iterator<Item = (f32, TimingGridLineKind)> {
    let TimelineGridView { tracks, min_beat, logical_ppb: ppb, beats_per_bar: bpb, render_scale: scale, .. } = *view;
    let valid = ppb.is_finite()
        && ppb > 0.0
        && scale.is_finite()
        && scale >= 1.0
        && min_beat.is_finite()
        && bpb.is_finite()
        && bpb >= 1.0
        && tracks.width > 0.0;
    let visible_end = if valid { min_beat + tracks.width / ppb } else { min_beat };
    // An invalid view walks from NaN, which yields nothing.
    let walk_start = if valid { min_beat } else { f32::NAN };
    let scaled_ppb = ppb * scale;
    timing_grid_lines(walk_start, visible_end, ppb, bpb)
        .map(move |(beat, kind)| ((beat - min_beat) * scaled_ppb, kind))
        .filter(|(col, _)| col.round() >= 0.0)
        .map(move |(col, kind)| (tracks.x + col / scale, kind))
}

/// Queue the grid lines and the layer-0 top separator. The caller wraps this
/// in `lane_content_scissor(view.tracks)` and runs its own prepare/render
/// cycle before the clip bodies.
pub fn emit_timeline_grid(ui: &mut UIRenderer, view: &TimelineGridView, rows: impl Iterator<Item = (f32, f32)>) {
    for (top, bottom) in rows {
        for (x, kind) in grid_line_xs(view) {
            ui.draw_grid_line(x, top, x, bottom, kind);
        }
    }
    if let Some(top) = view.first_layer_top
        && view.tracks.width > 0.0
    {
        // Above the lines: a rect and a line at one depth draw rect first.
        ui.push_depth(ui.current_depth().above(1));
        ui.draw_rect(view.tracks.x, top, view.tracks.width, color::TRACK_SEPARATOR_HEIGHT, color::SEPARATOR_COLOR);
        ui.pop_depth();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_ui::node::Rect;

    fn view() -> TimelineGridView {
        TimelineGridView {
            tracks: Rect::new(10.0, 20.0, 400.0, 200.0),
            min_beat: 0.5,
            logical_ppb: 100.0,
            beats_per_bar: 4.0,
            render_scale: 2.0,
            first_layer_top: Some(20.0),
        }
    }

    #[test]
    fn lines_start_at_the_first_column_and_never_left_of_it() {
        let xs: Vec<f32> = grid_line_xs(&view()).map(|(x, _)| x).collect();
        assert!(!xs.is_empty());
        assert!(xs.iter().all(|x| *x >= 10.0));
        // Beat 1.0 sits half a beat (50 px) in from the tracks edge.
        assert!(xs.iter().any(|x| (*x - 60.0).abs() < 0.01), "{xs:?}");
    }

    #[test]
    fn degenerate_views_walk_no_lines() {
        for bad in [
            TimelineGridView { logical_ppb: f32::NAN, ..view() },
            TimelineGridView { logical_ppb: 0.0, ..view() },
            TimelineGridView { beats_per_bar: 0.0, ..view() },
            TimelineGridView { render_scale: 0.0, ..view() },
            TimelineGridView { min_beat: f32::NAN, ..view() },
        ] {
            assert_eq!(grid_line_xs(&bad).count(), 0, "{bad:?}");
        }
    }
}
