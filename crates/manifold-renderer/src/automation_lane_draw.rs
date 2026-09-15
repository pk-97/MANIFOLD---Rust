//! Automation geometry and interaction feedback share the UI hit-test contract.
use crate::ui_renderer::UIRenderer;
use manifold_ui::automation_hit_tester::AutomationOperation;
use manifold_ui::bitmap_renderer::timing_grid_lines;
use manifold_ui::node::{Color32, Rect};
use manifold_ui::panels::viewport::AutomationLaneScreen;
use manifold_ui::{UIState, color};
use std::io::Write;

fn dot(ui: &mut UIRenderer, x: f32, y: f32, radius: f32, tint: Color32) {
    ui.draw_rounded_rect(
        x - radius,
        y - radius,
        radius * 2.0,
        radius * 2.0,
        tint,
        radius,
    );
}

/// Stack formatting keeps moving readouts off the allocator.
fn readout(
    ui: &mut UIRenderer,
    x: f32,
    y: f32,
    beat: Option<f64>,
    beats_per_bar: f32,
    value: f32,
    whole_numbers: bool,
) {
    let mut bytes = [0_u8; 512];
    let mut text = std::io::Cursor::new(bytes.as_mut_slice());
    if let Some(beat) = beat {
        let (bar, beat_in_bar, subbeat) = musical_position(beat, beats_per_bar);
        if whole_numbers {
            write!(text, "{bar}.{beat_in_bar}.{subbeat:03}   Value {value:.0}")
        } else {
            write!(text, "{bar}.{beat_in_bar}.{subbeat:03}   Value {value:.3}")
        }
        .expect("numeric readout fits");
    } else {
        if whole_numbers {
            write!(text, "{value:.0}").expect("numeric value fits");
        } else {
            write!(text, "{value:.3}").expect("numeric value fits");
        }
    }
    let len = text.position() as usize;
    let text = std::str::from_utf8(&bytes[..len]).expect("numeric readout is UTF-8");
    ui.draw_text(
        x,
        y,
        text,
        color::AUTOMATION_LABEL_FONT as f32,
        color::TEXT_WHITE_C32,
    );
}

/// Format the same one-based bar/beat convention used by the timeline ruler.
/// The final field is thousandths of a beat, matching the point time editor.
fn musical_position(beat: f64, beats_per_bar: f32) -> (i64, u32, u32) {
    let beat = if beat.is_finite() { beat.max(0.0) } else { 0.0 };
    let beats_per_bar = if beats_per_bar.is_finite() {
        beats_per_bar.max(1.0) as f64
    } else {
        1.0
    };
    let rounded = (beat * 1000.0).round() / 1000.0;
    let bar = (rounded / beats_per_bar).floor() as i64 + 1;
    let in_bar = rounded % beats_per_bar;
    (
        bar,
        in_bar.floor() as u32 + 1,
        (in_bar.fract() * 1000.0).round() as u32,
    )
}

fn feedback_matches_point(
    feedback: &manifold_ui::automation_hit_tester::AutomationFeedback,
    lane: &AutomationLaneScreen,
    point: manifold_ui::panels::viewport::AutomationDotScreen,
) -> bool {
    if feedback.point_beat != Some(point.beat) {
        return false;
    }
    if feedback.operation != AutomationOperation::Point {
        return true;
    }
    let range = lane.param_max - lane.param_min;
    range.is_finite()
        && range.abs() > f32::EPSILON
        && ((feedback.value - lane.param_min) / range - point.value_norm).abs() <= 0.001
}

const AUTOMATION_DOT_CLIP_PAD: f32 = 8.0;

fn draw_timing_grid(ui: &mut UIRenderer, lane: &AutomationLaneScreen, graph: Rect) {
    let ppb = lane.pixels_per_beat;
    let visible_start = lane.visible_beat_start;
    let bpb = lane.beats_per_bar;
    if !ppb.is_finite()
        || ppb <= 0.0
        || !visible_start.is_finite()
        || !bpb.is_finite()
        || bpb <= 0.0
        || graph.width <= 0.0
        || graph.height <= 0.0
    {
        return;
    }

    let visible_end = visible_start + graph.width / ppb;
    for (beat, kind) in timing_grid_lines(visible_start, visible_end, ppb, bpb) {
        let x = lane.beat_to_pixel(beat);
        if x >= graph.x {
            ui.draw_grid_line(x, graph.y, x, graph.y_max(), kind);
        }
    }
}

pub fn emit_automation_lanes(
    ui: &mut UIRenderer,
    lanes: &[AutomationLaneScreen],
    tracks: Rect,
    selection: Option<&UIState>,
) {
    if lanes.is_empty() {
        if selection.is_some_and(|s| s.automation_mode_visible) {
            ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
            ui.draw_text(
                tracks.x + 12.0,
                tracks.y + 12.0,
                "Click AUTO beside a parameter to open its automation lane",
                color::FONT_LABEL as f32,
                color::AUTOMATION_LABEL_COLOR,
            );
            ui.pop_immediate_clip();
        }
        return;
    }
    ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
    for lane in lanes {
        let r = lane.strip_rect;
        let graph = lane.curve_rect();
        let header = lane.header_rect();
        let feedback = selection
            .and_then(|s| s.automation_feedback.as_ref())
            .filter(|f| f.target == lane.target && f.param_id == lane.param_id);
        let selected = selection.is_some_and(|s| {
            s.selected_automation_point
                .as_ref()
                .is_some_and(|p| p.target == lane.target && p.param_id == lane.param_id)
                || s.selected_automation_points
                    .iter()
                    .any(|p| p.target == lane.target && p.param_id == lane.param_id)
        });
        let accent = if lane.overridden {
            color::AUTOMATION_LINE_OVERRIDDEN_COLOR
        } else {
            color::AUTOMATION_LINE_COLOR
        };
        ui.push_immediate_clip(r.x, r.y, r.width, r.height);
        ui.draw_rect(r.x, r.y, r.width, r.height, color::AUTOMATION_STRIP_BG);
        if header.height > 0.0 {
            ui.draw_rect(
                header.x,
                header.y,
                header.width,
                header.height,
                Color32::new(31, 34, 40, 255),
            );
            ui.draw_rect(
                header.x,
                header.y,
                3.0,
                header.height,
                if selected || feedback.is_some() {
                    accent
                } else {
                    color::AUTOMATION_LINE_OVERRIDDEN_COLOR
                },
            );
            ui.draw_text(
                r.x + 8.0,
                r.y + 3.0,
                &lane.label,
                color::FONT_LABEL as f32,
                color::TEXT_WHITE_C32,
            );
            let state = if lane.overridden {
                "LAYER · ARRANGEMENT · OVERRIDDEN"
            } else {
                "LAYER · ARRANGEMENT AUTOMATION"
            };
            if r.width > 380.0 {
                ui.draw_text(
                    r.x + r.width - 330.0,
                    r.y + 4.0,
                    state,
                    color::AUTOMATION_LABEL_FONT as f32,
                    color::AUTOMATION_LABEL_COLOR,
                );
            }
            for norm in [0.0, 0.5, 1.0] {
                let y = lane.y_at_norm(norm);
                ui.draw_aa_line(
                    graph.x,
                    y,
                    graph.x_max(),
                    y,
                    1.0,
                    Color32::new(47, 50, 57, 255),
                );
                if graph.width >= 180.0 {
                    readout(
                        ui,
                        graph.x_max() - 65.0,
                        (y + 2.0).min(r.y_max() - 16.0),
                        None,
                        lane.beats_per_bar,
                        lane.param_min + norm * (lane.param_max - lane.param_min),
                        lane.whole_numbers,
                    );
                }
            }
        }
        draw_timing_grid(ui, lane, graph);
        let hot_segment = feedback
            .filter(|f| {
                matches!(
                    f.operation,
                    AutomationOperation::Segment | AutomationOperation::Bend
                )
            })
            .and_then(|f| f.point_beat.zip(f.segment_end_beat))
            .and_then(|(a, b)| {
                lane.dots
                    .iter()
                    .find(|d| d.beat == a)
                    .zip(lane.dots.iter().find(|d| d.beat == b))
            })
            .map(|(a, b)| (a.x, b.x));
        for pair in lane.polyline.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            let segment_hot = hot_segment.is_some_and(|(left, right)| x0 >= left && x1 <= right);
            ui.draw_aa_line(
                x0,
                y0,
                x1,
                y1,
                if segment_hot {
                    3.0
                } else {
                    color::AUTOMATION_LINE_THICKNESS
                },
                if segment_hot {
                    color::TEXT_WHITE_C32
                } else {
                    accent
                },
            );
        }
        // Pad only the point pass so the dot at the viewport's true beat-zero
        // edge keeps its full radius. The curve/grid x mapping remains exact.
        ui.push_immediate_clip(
            r.x - AUTOMATION_DOT_CLIP_PAD,
            r.y,
            r.width + AUTOMATION_DOT_CLIP_PAD * 2.0,
            r.height,
        );
        for point in &lane.dots {
            if point.x < graph.x - AUTOMATION_DOT_CLIP_PAD
                || point.x > graph.x_max() + AUTOMATION_DOT_CLIP_PAD
            {
                continue;
            }
            let selected = selection.is_some_and(|s| {
                s.automation_point_selected(
                    &lane.target,
                    &lane.param_id,
                    point.beat,
                    point.value_norm,
                )
            });
            let hot = feedback.is_some_and(|f| {
                matches!(
                    f.operation,
                    AutomationOperation::Point
                        | AutomationOperation::Segment
                        | AutomationOperation::Bend
                ) && feedback_matches_point(f, lane, *point)
            });
            if hot {
                dot(ui, point.x, point.y, 8.0, Color32::new(91, 190, 220, 75));
            }
            dot(
                ui,
                point.x,
                point.y,
                if selected || hot {
                    4.5
                } else {
                    color::AUTOMATION_DOT_RADIUS
                },
                if selected || hot {
                    color::TEXT_WHITE_C32
                } else {
                    accent
                },
            );
        }
        ui.pop_immediate_clip();
        let grip = lane.resize_rect();
        let hot_resize = feedback.is_some_and(|f| f.operation == AutomationOperation::Resize);
        let grip_color = if hot_resize {
            color::TEXT_WHITE_C32
        } else {
            color::AUTOMATION_LABEL_COLOR
        };
        for offset in [-1.0, 1.0] {
            ui.draw_aa_line(
                grip.x + 4.0,
                grip.y + grip.height * 0.5 + offset,
                grip.x + 28.0,
                grip.y + grip.height * 0.5 + offset,
                1.0,
                grip_color,
            );
        }
        if let Some(f) = feedback {
            if matches!(
                f.operation,
                AutomationOperation::Insert | AutomationOperation::Draw
            ) {
                dot(ui, f.position.x, f.position.y, 6.0, accent);
                dot(
                    ui,
                    f.position.x,
                    f.position.y,
                    3.0,
                    color::AUTOMATION_STRIP_BG,
                );
            }
            if matches!(
                f.operation,
                AutomationOperation::Point
                    | AutomationOperation::Insert
                    | AutomationOperation::Draw
            ) {
                ui.draw_aa_line(
                    f.position.x,
                    graph.y,
                    f.position.x,
                    graph.y_max(),
                    1.0,
                    Color32::new(91, 190, 220, 85),
                );
                ui.draw_aa_line(
                    graph.x,
                    f.position.y,
                    graph.x_max(),
                    f.position.y,
                    1.0,
                    Color32::new(91, 190, 220, 55),
                );
            }
            if r.height >= 64.0 {
                let footer_y = r.y_max() - 20.0;
                ui.draw_rect(
                    r.x + 34.0,
                    footer_y,
                    (r.width - 34.0).max(0.0),
                    20.0,
                    Color32::new(20, 23, 28, 245),
                );
                ui.draw_text(
                    r.x + 40.0,
                    footer_y + 3.0,
                    f.hint,
                    color::AUTOMATION_LABEL_FONT as f32,
                    color::TEXT_WHITE_C32,
                );
            }
            // The operation hint stays in the footer; the nearby point tooltip
            // is deliberately compact so long hints never overflow the lane.
            if r.width >= 64.0 && r.height >= 24.0 {
                let show_point_tooltip = matches!(
                    f.operation,
                    AutomationOperation::Point
                        | AutomationOperation::Insert
                        | AutomationOperation::Draw
                        | AutomationOperation::Segment
                        | AutomationOperation::Bend
                );
                let bubble_w = r.width.min(190.0);
                let bubble_h = 20.0;
                let gap = 8.0;
                let min_x = r.x;
                let max_x = (r.x_max() - bubble_w).max(min_x);
                let min_y = r.y;
                let max_y = (r.y_max() - bubble_h).max(min_y);
                let candidates = [
                    (f.position.x + gap, f.position.y - bubble_h - gap),
                    (f.position.x - bubble_w - gap, f.position.y - bubble_h - gap),
                    (f.position.x + gap, f.position.y + gap),
                    (f.position.x - bubble_w - gap, f.position.y + gap),
                ];
                let mut bubble = None;
                if show_point_tooltip {
                    for (x, y) in candidates {
                        let x = x.clamp(min_x, max_x);
                        let y = y.clamp(min_y, max_y);
                        let overlaps = f.position.x >= x
                            && f.position.x <= x + bubble_w
                            && f.position.y >= y
                            && f.position.y <= y + bubble_h;
                        if !overlaps {
                            bubble = Some((x, y));
                            break;
                        }
                    }
                }
                if let Some((bubble_x, bubble_y)) = bubble {
                    // Lines are batched after rectangles at each depth. Lift
                    // this local readout so the curve cannot cross its text.
                    ui.push_depth(ui.current_depth().above(1));
                    ui.draw_rounded_rect(
                        bubble_x,
                        bubble_y,
                        bubble_w,
                        bubble_h,
                        Color32::new(20, 23, 28, 245),
                        3.0,
                    );
                    readout(
                        ui,
                        bubble_x + 6.0,
                        bubble_y + 3.0,
                        Some(f.beat.0),
                        lane.beats_per_bar,
                        f.value,
                        lane.whole_numbers,
                    );
                    ui.pop_depth();
                }
            }
        }
        if lane.dots.is_empty() && feedback.is_none() && graph.height >= 20.0 {
            ui.draw_text(
                graph.x + 8.0,
                graph.y_max() - 16.0,
                "Click to add an automation point",
                color::FONT_LABEL as f32,
                color::AUTOMATION_LABEL_COLOR,
            );
        }
        ui.pop_immediate_clip();
    }
    ui.pop_immediate_clip();
}

#[cfg(test)]
mod tests {
    use super::musical_position;

    #[test]
    fn readout_position_uses_runtime_time_signature() {
        assert_eq!(musical_position(6.25, 3.0), (3, 1, 250));
        assert_eq!(musical_position(5.0, 5.0), (2, 1, 0));
    }

    #[test]
    fn readout_position_stays_finite_for_invalid_inputs() {
        assert_eq!(musical_position(f64::NAN, 0.0), (1, 1, 0));
        assert_eq!(musical_position(f64::INFINITY, f32::NAN), (1, 1, 0));
    }
}
