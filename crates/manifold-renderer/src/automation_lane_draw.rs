//! Automation lane strip emission (P4, `docs/AUTOMATION_LANES_DESIGN.md` section 7).
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
pub fn emit_automation_lanes(
    ui: &mut UIRenderer,
    lanes: &[AutomationLaneScreen],
    tracks: Rect,
    selection: Option<&manifold_ui::UIState>,
) {
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

        for dot in &l.dots {
            let selected = selection.is_some_and(|state| {
                state.automation_point_selected(&l.target, &l.param_id, dot.beat)
            });
            let radius = color::AUTOMATION_DOT_RADIUS + if selected { 1.5 } else { 0.0 };
            let d = radius * 2.0;
            ui.draw_rounded_rect(
                dot.x - d * 0.5,
                dot.y - d * 0.5,
                d,
                d,
                if selected { color::TEXT_WHITE_C32 } else { line_color },
                radius,
            );
        }

        // Parameter name and the visible grip share the viewport's lane geometry.
        ui.draw_text(
            l.strip_rect.x + 4.0,
            l.strip_rect.y + 2.0,
            &l.label,
            color::AUTOMATION_LABEL_FONT as f32,
            color::AUTOMATION_LABEL_COLOR,
        );
        let grip = l.resize_rect();
        let x = grip.x + 4.0;
        let y = grip.y + grip.height * 0.5;
        ui.draw_line(x, y - 1.0, x + 24.0, y - 1.0, 1.0, color::AUTOMATION_LABEL_COLOR);
        ui.draw_line(x, y + 1.0, x + 24.0, y + 1.0, 1.0, color::AUTOMATION_LABEL_COLOR);
    }

    ui.pop_immediate_clip();
}
