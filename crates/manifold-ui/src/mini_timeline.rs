//! [`MiniTimeline`] — the graph editor's bottom scrub strip.
//!
//! A compact, authoring-only transport: a readout + play/pause button, a bar
//! ruler, a clip minimap (every visible layer squashed to a thin row, clips
//! coloured by layer), and a draggable playhead. It is *not* the full track
//! editor — you scrub time to preview the graph's output at a point, and watch
//! motion with play. The main timeline stays the editing surface.
//!
//! Pure geometry + drawing, mirroring [`crate::dock::Dock`]: the render pass
//! and the input pass both derive rects from the same associated functions, so
//! a click can't land somewhere the paint didn't. The scrub-drag state lives on
//! the caller (the editor workspace), not here.
//!
//! Reuses the shared theme (`PANEL_BG`, `GRID_BAR_LINE`, `OVERVIEW_PLAYHEAD`,
//! `TRACK_BG*`) and beat↔pixel is a plain linear map over the whole content
//! length, so it reads as the same instrument as the main ruler.

use crate::color;
use crate::draw::Painter;
use crate::node::{Color32, Rect, Vec2};

/// Height of the top readout row (play button + bar/BPM readout).
pub const TOP_H: f32 = 24.0;
/// Height of the bar ruler beneath the readout row.
pub const RULER_H: f32 = 14.0;
/// Play/pause button edge (a square in the readout row).
const BTN: f32 = 16.0;

const READOUT_TEXT: [u8; 4] = [205, 205, 212, 255];
const TICK_LABEL: [u8; 4] = [128, 128, 138, 255];

/// One clip as the minimap draws it: a coloured bar on a layer row, positioned
/// by its beat span. Built by the caller from the project timeline (colour via
/// the shared `get_clip_color`, so it matches the main timeline).
#[derive(Debug, Clone, Copy)]
pub struct MiniClip {
    /// Row index (0 = top visible layer).
    pub row: usize,
    pub start_beat: f32,
    pub end_beat: f32,
    pub color: Color32,
}

/// Stateless drawer + geometry for the bottom scrub strip.
pub struct MiniTimeline;

impl MiniTimeline {
    // ── Geometry (shared by render + input) ─────────────────────────────────

    /// The scrubbable region — everything below the readout row (ruler +
    /// minimap). A press here begins a scrub.
    pub fn body_rect(area: Rect) -> Rect {
        Rect::new(area.x, area.y + TOP_H, area.width, (area.height - TOP_H).max(0.0))
    }

    /// The bar ruler band (top of the body).
    pub fn ruler_rect(area: Rect) -> Rect {
        Rect::new(area.x, area.y + TOP_H, area.width, RULER_H)
    }

    /// The clip minimap band (below the ruler).
    pub fn minimap_rect(area: Rect) -> Rect {
        Rect::new(
            area.x,
            area.y + TOP_H + RULER_H,
            area.width,
            (area.height - TOP_H - RULER_H).max(0.0),
        )
    }

    /// The play/pause button rect in the readout row's left edge.
    pub fn play_button_rect(area: Rect) -> Rect {
        Rect::new(area.x + 8.0, area.y + (TOP_H - BTN) * 0.5, BTN, BTN)
    }

    /// True when `pos` is over the play/pause button.
    pub fn hit_play(area: Rect, pos: Vec2) -> bool {
        Self::play_button_rect(area).contains(pos)
    }

    /// Content length clamped to a sane floor, so an empty project still draws
    /// a usable ruler instead of dividing by zero.
    fn total(total_beats: f32) -> f32 {
        total_beats.max(4.0)
    }

    /// Screen x of `beat` within `area` (linear over the whole content length).
    pub fn beat_to_x(area: Rect, total_beats: f32, beat: f32) -> f32 {
        let body = Self::body_rect(area);
        let frac = (beat / Self::total(total_beats)).clamp(0.0, 1.0);
        body.x + frac * body.width
    }

    /// Beat under screen x, clamped to `0..=total` — the scrub map.
    pub fn beat_at_x(area: Rect, total_beats: f32, x: f32) -> f32 {
        let body = Self::body_rect(area);
        if body.width <= 0.0 {
            return 0.0;
        }
        let frac = ((x - body.x) / body.width).clamp(0.0, 1.0);
        frac * Self::total(total_beats)
    }

    // ── Draw ────────────────────────────────────────────────────────────────

    /// Paint the whole strip in `area`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        area: Rect,
        total_beats: f32,
        beats_per_bar: f32,
        current_beat: f32,
        row_count: usize,
        clips: &[MiniClip],
        readout: &str,
        is_playing: bool,
        ui: &mut dyn Painter,
    ) {
        let total = Self::total(total_beats);
        let ruler = Self::ruler_rect(area);
        let mm = Self::minimap_rect(area);

        // Panel + darker ruler band.
        ui.draw_rect(area.x, area.y, area.width, area.height, color::PANEL_BG);
        ui.draw_rect(ruler.x, ruler.y, ruler.width, ruler.height, color::BG_1);

        // Top readout row: play/pause button + "Bar x · BPM · sig".
        let pb = Self::play_button_rect(area);
        ui.draw_rounded_rect(pb.x, pb.y, pb.width, pb.height, color::BG_2, 3.0);
        Self::draw_transport_icon(pb, is_playing, ui);
        ui.draw_text(pb.x_max() + 8.0, area.y + (TOP_H - 11.0) * 0.5, readout, 11.0, READOUT_TEXT);
        ui.draw_line(area.x, area.y + TOP_H, area.x_max(), area.y + TOP_H, 1.0, color::DIVIDER_COLOR);

        // Bar ticks + labels. Label density backs off when bars get tight so
        // numbers never overlap.
        let bpb = beats_per_bar.max(1.0);
        let bars = (total / bpb).max(1.0);
        let px_per_bar = mm.width / bars;
        let label_every = if px_per_bar >= 34.0 {
            1
        } else if px_per_bar * 4.0 >= 34.0 {
            4
        } else {
            16
        };
        let mut bar = 0usize;
        let mut beat = 0.0_f32;
        while beat <= total + 0.001 {
            let x = Self::beat_to_x(area, total, beat);
            ui.draw_line(x, ruler.y, x, ruler.y_max(), 1.0, color::GRID_BAR_LINE);
            if bar.is_multiple_of(label_every) {
                ui.draw_text(x + 2.0, ruler.y + 1.0, &format!("{}", bar + 1), 9.0, TICK_LABEL);
            }
            beat += bpb;
            bar += 1;
        }

        // Layer rows: alternating recessed lanes + hairline separators.
        let rows = row_count.max(1);
        let row_h = mm.height / rows as f32;
        for r in 0..rows {
            let y = mm.y + r as f32 * row_h;
            let bg = if r % 2 == 0 { color::TRACK_BG } else { color::TRACK_BG_ALT };
            ui.draw_rect(mm.x, y, mm.width, row_h, bg);
            if r > 0 {
                ui.draw_line(mm.x, y, mm.x_max(), y, 1.0, color::DIVIDER_COLOR);
            }
        }

        // Clips as coloured bars on their layer row.
        for c in clips {
            if c.row >= rows {
                continue;
            }
            let x0 = Self::beat_to_x(area, total, c.start_beat);
            let x1 = Self::beat_to_x(area, total, c.end_beat);
            let y = mm.y + c.row as f32 * row_h;
            ui.draw_rounded_rect(x0, y + 1.0, (x1 - x0).max(1.0), (row_h - 2.0).max(1.0), c.color, 2.0);
        }

        // Playhead — one red line across ruler + minimap, like the main timeline.
        let phx = Self::beat_to_x(area, total, current_beat);
        ui.draw_rect(
            phx - color::PLAYHEAD_WIDTH * 0.5,
            ruler.y,
            color::PLAYHEAD_WIDTH,
            (area.y_max() - ruler.y).max(0.0),
            color::OVERVIEW_PLAYHEAD,
        );
    }

    /// Draw the transport glyph inside `pb`: a right-pointing play triangle
    /// (paused) or two pause bars (playing). Bars are drawn as rects — no font
    /// dependency for the one glyph the UI font might lack.
    fn draw_transport_icon(pb: Rect, is_playing: bool, ui: &mut dyn Painter) {
        let icon: [u8; 4] = [220, 220, 228, 255];
        if is_playing {
            let bar_w = 3.0;
            let bar_h = pb.height * 0.5;
            let cy = pb.y + (pb.height - bar_h) * 0.5;
            let cx = pb.x + pb.width * 0.5;
            let fill = Color32::new(icon[0], icon[1], icon[2], icon[3]);
            ui.draw_rect(cx - bar_w - 1.0, cy, bar_w, bar_h, fill);
            ui.draw_rect(cx + 1.0, cy, bar_w, bar_h, fill);
        } else {
            // "▶" is in the UI font (icon-button chevrons use it); sits centred.
            ui.draw_text(pb.x + 4.0, pb.y + (pb.height - 11.0) * 0.5, "▶", 11.0, icon);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(100.0, 500.0, 800.0, 150.0)
    }

    #[test]
    fn body_sits_below_the_readout_row() {
        let b = MiniTimeline::body_rect(area());
        assert_eq!(b.y, 500.0 + TOP_H);
        assert_eq!(b.height, 150.0 - TOP_H);
        assert_eq!(b.width, 800.0);
    }

    #[test]
    fn beat_maps_to_x_and_back() {
        let a = area();
        let total = 32.0;
        // Endpoints land on the body edges.
        assert!((MiniTimeline::beat_to_x(a, total, 0.0) - a.x).abs() < 0.001);
        assert!((MiniTimeline::beat_to_x(a, total, total) - a.x_max()).abs() < 0.001);
        // Round-trips through the middle.
        let x = MiniTimeline::beat_to_x(a, total, 12.0);
        assert!((MiniTimeline::beat_at_x(a, total, x) - 12.0).abs() < 0.01);
    }

    #[test]
    fn scrub_clamps_outside_the_strip() {
        let a = area();
        assert_eq!(MiniTimeline::beat_at_x(a, 16.0, a.x - 200.0), 0.0);
        assert_eq!(MiniTimeline::beat_at_x(a, 16.0, a.x_max() + 200.0), 16.0);
    }

    #[test]
    fn empty_project_still_gets_a_ruler() {
        // total 0 → clamps to the 4-beat floor, no divide-by-zero.
        let a = area();
        assert_eq!(MiniTimeline::beat_to_x(a, 0.0, 0.0), a.x);
        assert!(MiniTimeline::beat_to_x(a, 0.0, 4.0) <= a.x_max() + 0.001);
    }

    #[test]
    fn play_button_is_inside_the_readout_row() {
        let a = area();
        let pb = MiniTimeline::play_button_rect(a);
        assert!(pb.y >= a.y && pb.y_max() <= a.y + TOP_H);
        assert!(MiniTimeline::hit_play(a, Vec2::new(pb.x + 2.0, pb.y + 2.0)));
        assert!(!MiniTimeline::hit_play(a, Vec2::new(a.x + 400.0, a.y + 8.0)));
    }
}
