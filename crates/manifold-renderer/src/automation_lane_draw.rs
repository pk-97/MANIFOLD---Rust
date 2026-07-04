//! Automation lane strip emission (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7).
//! Turns the viewport's resolved `AutomationLaneScreen` geometry into
//! `UIRenderer` draws — the same "geometry in manifold-ui, GPU draw here"
//! split as `clip_draw.rs`. A strip is: a subtle background band, the sampled
//! breakpoint line (a polyline of `draw_line` segments), a dot at each
//! breakpoint, and the param label. Grayed instead of red when the lane's
//! param is currently latched/overridden (Live's affordance).

use crate::ui_renderer::UIRenderer;
use manifold_ui::color;
use manifold_ui::node::Rect;
use manifold_ui::panels::viewport::AutomationLaneScreen;

/// Emit every visible lane strip: background bands first (so the line/dots of
/// one lane never get occluded by a neighbouring strip's band), then the
/// lines + dots + labels on top. Scissored to `tracks` so a lane scrolled
/// under the header column never draws over the layer controls (mirrors
/// `clip_draw::emit_clip_names`'s tracks-rect clip).
pub fn emit_automation_lanes(ui: &mut UIRenderer, lanes: &[AutomationLaneScreen], tracks: Rect) {
    if lanes.is_empty() {
        return;
    }
    ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);

    for l in lanes {
        ui.draw_rect(
            l.strip_rect.x,
            l.strip_rect.y,
            l.strip_rect.width,
            l.strip_rect.height,
            color::AUTOMATION_STRIP_BG,
        );
    }

    for l in lanes {
        let line_color = if l.overridden {
            color::AUTOMATION_LINE_OVERRIDDEN_COLOR
        } else {
            color::AUTOMATION_LINE_COLOR
        };

        for pair in l.polyline.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            ui.draw_line(x0, y0, x1, y1, color::AUTOMATION_LINE_THICKNESS, line_color);
        }

        let d = color::AUTOMATION_DOT_RADIUS * 2.0;
        for &(x, y) in &l.dots {
            ui.draw_rounded_rect(x - d * 0.5, y - d * 0.5, d, d, line_color, color::AUTOMATION_DOT_RADIUS);
        }

        // Label, left-anchored inside the strip — the read-only stand-in for
        // Live's param-chooser dropdown (breakpoint editing / the chooser
        // itself are a later phase; see docs/AUTOMATION_LANES_DESIGN.md §7).
        ui.draw_text(
            l.strip_rect.x + 4.0,
            l.strip_rect.y + 2.0,
            &l.label,
            color::AUTOMATION_LABEL_FONT as f32,
            color::AUTOMATION_LABEL_COLOR,
        );
    }

    ui.pop_immediate_clip();
}
