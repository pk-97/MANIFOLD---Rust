//! Automation geometry and interaction feedback share the UI hit-test contract.
use crate::ui_renderer::UIRenderer;
use manifold_ui::automation_hit_tester::AutomationOperation;
use manifold_ui::{color, UIState};
use manifold_ui::node::{Color32, Rect};
use manifold_ui::panels::viewport::AutomationLaneScreen;
use std::io::Write;

fn dot(ui: &mut UIRenderer, x: f32, y: f32, radius: f32, tint: Color32) {
    ui.draw_rounded_rect(x - radius, y - radius, radius * 2.0, radius * 2.0, tint, radius);
}

/// Stack formatting keeps moving readouts off the allocator.
fn readout(ui: &mut UIRenderer, x: f32, y: f32, beat: Option<f64>, value: f32) {
    let mut bytes = [0_u8; 512];
    let mut text = std::io::Cursor::new(bytes.as_mut_slice());
    if let Some(beat) = beat {
        write!(text, "Beat {beat:.3}   Value {value:.3}").expect("numeric readout fits");
    } else {
        write!(text, "{value:.3}").expect("numeric value fits");
    }
    let len = text.position() as usize;
    let text = std::str::from_utf8(&bytes[..len]).expect("numeric readout is UTF-8");
    ui.draw_text(x, y, text, color::AUTOMATION_LABEL_FONT as f32, color::TEXT_WHITE_C32);
}

pub fn emit_automation_lanes(
    ui: &mut UIRenderer,
    lanes: &[AutomationLaneScreen],
    tracks: Rect,
    selection: Option<&UIState>,
) {
    if lanes.is_empty() { return; }
    ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
    for lane in lanes {
        let r = lane.strip_rect;
        let graph = lane.curve_rect();
        let header = lane.header_rect();
        let feedback = selection.and_then(|s| s.automation_feedback.as_ref())
            .filter(|f| f.target == lane.target && f.param_id == lane.param_id);
        let selected = selection.is_some_and(|s| s.selected_automation_point.as_ref()
            .is_some_and(|p| p.target == lane.target && p.param_id == lane.param_id)
            || s.selected_automation_points.iter().any(|p| p.target == lane.target && p.param_id == lane.param_id));
        let accent = if lane.overridden { color::AUTOMATION_LINE_OVERRIDDEN_COLOR } else { color::AUTOMATION_LINE_COLOR };
        ui.push_immediate_clip(r.x, r.y, r.width, r.height);
        ui.draw_rect(r.x, r.y, r.width, r.height, color::AUTOMATION_STRIP_BG);
        if header.height > 0.0 {
            ui.draw_rect(header.x, header.y, header.width, header.height, Color32::new(31, 34, 40, 255));
            ui.draw_rect(header.x, header.y, 3.0, header.height, if selected || feedback.is_some() { accent } else { color::AUTOMATION_LINE_OVERRIDDEN_COLOR });
            ui.draw_text(r.x + 8.0, r.y + 4.0, &lane.label, color::AUTOMATION_LABEL_FONT as f32, color::TEXT_WHITE_C32);
            let state = if lane.overridden { "LAYER · ARRANGEMENT · OVERRIDDEN" }
                else { "LAYER · ARRANGEMENT AUTOMATION" };
            if r.width > 600.0 {
                ui.draw_text(r.x + r.width - 330.0, r.y + 4.0, state, color::AUTOMATION_LABEL_FONT as f32, color::AUTOMATION_LABEL_COLOR);
            }
            for norm in [0.0, 0.5, 1.0] {
                let y = lane.y_at_norm(norm);
                ui.draw_line(graph.x, y, graph.x_max(), y, 1.0, Color32::new(47, 50, 57, 255));
                readout(ui, graph.x_max() - 65.0, (y + 2.0).min(r.y_max() - 16.0), None, lane.param_min + norm * (lane.param_max - lane.param_min));
            }
        }
        let hot_segment = feedback.filter(|f| matches!(f.operation, AutomationOperation::Segment | AutomationOperation::Bend))
            .and_then(|f| f.point_beat.zip(f.segment_end_beat))
            .and_then(|(a, b)| lane.dots.iter().find(|d| d.beat == a)
                .zip(lane.dots.iter().find(|d| d.beat == b)))
            .map(|(a, b)| (a.x, b.x));
        for pair in lane.polyline.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            let segment_hot = hot_segment.is_some_and(|(left, right)| x0 >= left && x1 <= right);
            ui.draw_line(x0, y0, x1, y1, if segment_hot { 3.0 } else { color::AUTOMATION_LINE_THICKNESS }, if segment_hot { color::TEXT_WHITE_C32 } else { accent });
        }
        for point in &lane.dots {
            if point.x < graph.x || point.x > graph.x_max() { continue; }
            let selected = selection.is_some_and(|s| s.automation_point_selected(&lane.target, &lane.param_id, point.beat));
            let hot = feedback.is_some_and(|f| f.point_beat == Some(point.beat) && matches!(f.operation, AutomationOperation::Point | AutomationOperation::Segment | AutomationOperation::Bend));
            if hot { dot(ui, point.x, point.y, 8.0, Color32::new(91, 190, 220, 75)); }
            dot(ui, point.x, point.y, if selected || hot { 4.5 } else { color::AUTOMATION_DOT_RADIUS }, if selected || hot { color::TEXT_WHITE_C32 } else { accent });
        }
        let grip = lane.resize_rect();
        let hot_resize = feedback.is_some_and(|f| f.operation == AutomationOperation::Resize);
        let grip_color = if hot_resize { color::TEXT_WHITE_C32 } else { color::AUTOMATION_LABEL_COLOR };
        for offset in [-1.0, 1.0] {
            ui.draw_line(grip.x + 4.0, grip.y + grip.height * 0.5 + offset, grip.x + 28.0, grip.y + grip.height * 0.5 + offset, 1.0, grip_color);
        }
        if let Some(f) = feedback {
            if matches!(f.operation, AutomationOperation::Insert | AutomationOperation::Draw) {
                dot(ui, f.position.x, f.position.y, 6.0, accent);
                dot(ui, f.position.x, f.position.y, 3.0, color::AUTOMATION_STRIP_BG);
            }
            if matches!(f.operation, AutomationOperation::Point | AutomationOperation::Insert | AutomationOperation::Draw) {
                ui.draw_line(f.position.x, graph.y, f.position.x, graph.y_max(), 1.0, Color32::new(91, 190, 220, 85));
                ui.draw_line(graph.x, f.position.y, graph.x_max(), f.position.y, 1.0, Color32::new(91, 190, 220, 55));
            }
            // Fixed lane footer avoids chasing the pointer or obscuring its hit target.
            if r.height >= 64.0 {
                let y = r.y_max() - 20.0;
                ui.draw_rect(r.x + 34.0, y, (r.width - 34.0).max(0.0), 20.0, Color32::new(20, 23, 28, 245));
                ui.draw_text(r.x + 40.0, y + 3.0, f.hint, color::AUTOMATION_LABEL_FONT as f32, color::TEXT_WHITE_C32);
                if r.width > 750.0 && !matches!(f.operation, AutomationOperation::Header | AutomationOperation::Resize | AutomationOperation::Blocked) {
                    readout(ui, r.x_max() - 240.0, y + 3.0, Some(f.beat.0), f.value);
                }
            }
        }
        ui.pop_immediate_clip();
    }
    ui.pop_immediate_clip();
}
