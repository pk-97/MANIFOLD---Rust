//! Automation geometry and interaction feedback share the UI hit-test contract.
use crate::ui_renderer::UIRenderer;
use manifold_foundation::{EffectId, ParamId};
use manifold_ui::automation_hit_tester::AutomationOperation;
use manifold_ui::node::{Color32, FontWeight, Rect};
use manifold_ui::panels::param_slider_shared::ParamValueText;
use manifold_ui::panels::viewport::{
    AUTOMATION_TITLE_NAME_H, AUTOMATION_TITLE_VALUE_GAP, AutomationLaneScreen,
    automation_label_prefix, automation_title_has_value_line,
};
use manifold_ui::text::TextMeasure;
use manifold_ui::timing_grid::timing_grid_lines;
use manifold_ui::{UIState, color};
use std::io::Write;

/// Shortens the END of `text` with "…" until it fits `max_w`; returns the
/// text to draw and its measured width. A label that fits is borrowed, so the
/// per-frame paint allocates only when it has to shorten.
fn elide_end<'a>(
    measure: &dyn TextMeasure,
    text: &'a str,
    max_w: f32,
    size: u16,
    weight: FontWeight,
) -> (std::borrow::Cow<'a, str>, f32) {
    let w = measure.measure_text(text, size, weight).x;
    if w <= max_w {
        return (std::borrow::Cow::Borrowed(text), w);
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>() + "…";
        let w = measure.measure_text(&cand, size, weight).x;
        if w <= max_w {
            return (std::borrow::Cow::Owned(cand), w);
        }
    }
    (std::borrow::Cow::Borrowed(""), 0.0)
}

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

/// Empty lane: the flat value line is dashed, like Ableton's.
fn dashed_hline(ui: &mut UIRenderer, x0: f32, x1: f32, y: f32, thickness: f32, tint: Color32) {
    const DASH: f32 = 6.0;
    const GAP: f32 = 4.0;
    let mut x = x0;
    while x < x1 {
        ui.draw_aa_line(x, y, (x + DASH).min(x1), y, thickness, tint);
        x += DASH + GAP;
    }
}

/// Longest lane label kept in a bubble; the rest is cut so the numbers survive.
const READOUT_LABEL_MAX: usize = 200;

/// Stack formatting keeps moving readouts off the allocator. Never panics:
/// output that overflows the buffer is truncated.
/// `value` arrives already formatted in parameter units (the inspector's own
/// formatter), so units and enum names read the same as the slider.
fn format_readout<'b>(
    bytes: &'b mut [u8; 512],
    label: Option<&str>,
    beat: Option<f64>,
    beats_per_bar: f32,
    value: &str,
) -> &'b str {
    let mut text = std::io::Cursor::new(bytes.as_mut_slice());
    // Write errors only mean the buffer is full; the partial text is kept.
    if let Some(label) = label {
        let mut end = label.len().min(READOUT_LABEL_MAX);
        while !label.is_char_boundary(end) {
            end -= 1;
        }
        let _ = write!(text, "{}   ", &label[..end]);
    }
    if let Some(beat) = beat {
        let (bar, beat_in_bar, subbeat) = musical_position(beat, beats_per_bar);
        let _ = write!(text, "{bar}.{beat_in_bar}.{subbeat:03}   {value}");
    } else {
        let _ = write!(text, "{value}");
    }
    let len = text.position() as usize;
    match std::str::from_utf8(&bytes[..len]) {
        Ok(t) => t,
        Err(e) => std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or(""),
    }
}

/// Min/max scale label, in parameter units.
fn readout(ui: &mut UIRenderer, lane: &AutomationLaneScreen, x: f32, y: f32, value: f32) {
    let mut buf = ParamValueText::default();
    let text = lane
        .display
        .format_value(&mut buf, value, lane.param_min, lane.whole_numbers);
    ui.draw_text(
        x,
        y,
        text,
        color::AUTOMATION_LABEL_FONT as f32,
        color::AUTOMATION_LABEL_COLOR,
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

/// Pointer or drag this close to the title fades it out of the way.
const TITLE_FADE_REACH: f32 = 24.0;
const TITLE_FADED_ALPHA: u8 = 64;

/// The lane title, pinned to the strip's right edge: the parameter name large
/// and bold in the layer's colour, and under it the effect prefix and the
/// value at the playhead in parameter units. Fades while editing nears it.
fn draw_title(
    ui: &mut UIRenderer,
    measure: &dyn TextMeasure,
    lane: &AutomationLaneScreen,
    overridden: bool,
    feedback: Option<&manifold_ui::automation_hit_tester::AutomationFeedback>,
) {
    let rect = lane.title_rect;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let near = Rect::new(
        rect.x - TITLE_FADE_REACH,
        rect.y - TITLE_FADE_REACH,
        rect.width + 2.0 * TITLE_FADE_REACH,
        rect.height + 2.0 * TITLE_FADE_REACH,
    );
    let faded = feedback
        .is_some_and(|f| f.operation != AutomationOperation::Title && near.contains(f.position));
    let alpha = |c: Color32| {
        if faded {
            c.with_alpha(TITLE_FADED_ALPHA.min(c.a))
        } else {
            c
        }
    };
    let name_color = if lane.display.layer_color.a == 0 {
        color::TEXT_WHITE_C32
    } else {
        lane.display.layer_color
    };
    // Above the curve and dots, on a see-through backing so a stroke passing
    // behind the text dims instead of striking through it.
    ui.push_depth(ui.current_depth().above(1));
    ui.draw_rounded_rect(
        rect.x - 4.0,
        rect.y,
        rect.width + 8.0,
        rect.height,
        color::AUTOMATION_STRIP_BG.with_alpha(if faded { 40 } else { 170 }),
        3.0,
    );
    let name_font = color::AUTOMATION_TITLE_FONT;
    let small = color::AUTOMATION_TITLE_VALUE_FONT;
    let (name, name_w) = elide_end(
        measure,
        lane.title_name(),
        rect.width,
        name_font,
        FontWeight::Bold,
    );
    let name_x = rect.x_max() - name_w;
    ui.draw_text_weighted(
        name_x,
        rect.y,
        &name,
        name_font as f32,
        alpha(name_color),
        FontWeight::Bold,
    );
    // The effect prefix, small and dim, sits on the name's baseline.
    if let Some(prefix) = automation_label_prefix(&lane.label, &lane.display.param_name) {
        let prefix_w = measure.measure_text(prefix, small, FontWeight::Medium).x;
        let prefix_x = name_x - AUTOMATION_TITLE_VALUE_GAP - prefix_w;
        if prefix_x >= rect.x - 0.5 {
            let baseline_drop = (name_font - small) as f32 * 0.8;
            ui.draw_text(
                prefix_x,
                rect.y + baseline_drop,
                prefix,
                small as f32,
                alpha(color::AUTOMATION_LABEL_COLOR),
            );
        }
    }
    if automation_title_has_value_line(lane.strip_rect) {
        let y = rect.y + AUTOMATION_TITLE_NAME_H;
        let mut buf = ParamValueText::default();
        let value = lane.display.format_value(
            &mut buf,
            lane.playhead_value,
            lane.param_min,
            lane.whole_numbers,
        );
        let value_w = measure.measure_text(value, small, FontWeight::Medium).x;
        let value_color = if overridden {
            color::AUTOMATION_LINE_OVERRIDDEN_COLOR
        } else {
            color::TEXT_WHITE_C32
        };
        ui.draw_text(
            rect.x_max() - value_w,
            y,
            value,
            small as f32,
            alpha(value_color),
        );
        if overridden {
            // Left of the widest value, so it holds still while the value moves.
            const TAG: &str = "overridden";
            let tag_w = measure.measure_text(TAG, small, FontWeight::Medium).x;
            ui.draw_text(
                rect.x_max() - lane.title_value_w.max(value_w) - AUTOMATION_TITLE_VALUE_GAP - tag_w,
                y,
                TAG,
                small as f32,
                alpha(color::AUTOMATION_LINE_OVERRIDDEN_COLOR),
            );
        }
    }
    ui.pop_depth();
}

pub fn emit_automation_lanes(
    ui: &mut UIRenderer,
    measure: &dyn TextMeasure,
    lanes: &[AutomationLaneScreen],
    tracks: Rect,
    selection: Option<&UIState>,
    latched: &[(EffectId, ParamId)],
) {
    if lanes.is_empty() {
        if selection.is_some_and(|s| s.automation_mode_visible()) {
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
        let overridden = lane.is_overridden(latched);
        let accent = if overridden {
            color::AUTOMATION_LINE_OVERRIDDEN_COLOR
        } else {
            color::AUTOMATION_LINE_COLOR
        };
        ui.push_immediate_clip(r.x, r.y, r.width, r.height);
        ui.draw_rect(r.x, r.y, r.width, r.height, color::AUTOMATION_STRIP_BG);
        if let Some(range) = selection.and_then(|state| state.automation_time_selection.as_ref())
            && range
                .lanes
                .iter()
                .any(|key| key.0 == lane.target && key.1 == lane.param_id)
        {
            let x0 = lane.beat_to_pixel(range.start.0 as f32).max(graph.x);
            let x1 = lane.beat_to_pixel(range.end.0 as f32).min(graph.x_max());
            if x1 > x0 {
                ui.draw_rect(
                    x0,
                    graph.y,
                    x1 - x0,
                    graph.height,
                    color::AUTOMATION_LINE_COLOR.with_alpha(38),
                );
            }
        }
        // Lane marker stripe: lit while the lane is selected or under the pointer.
        ui.draw_rect(
            r.x,
            r.y,
            3.0,
            r.height,
            if selected || feedback.is_some() {
                accent
            } else {
                color::AUTOMATION_LINE_OVERRIDDEN_COLOR
            },
        );
        // Range guides at the bottom, middle and top, the ends labelled in
        // parameter units.
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
            if graph.width >= 180.0 && graph.height >= 24.0 && norm != 0.5 {
                let label_y = if norm == 0.0 {
                    y - color::AUTOMATION_LABEL_FONT as f32 - 2.0
                } else {
                    y + 2.0
                };
                readout(
                    ui,
                    lane,
                    graph.x + 6.0,
                    label_y,
                    lane.param_min + norm * (lane.param_max - lane.param_min),
                );
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
        if lane.dots.is_empty()
            && let Some(&(_, y)) = lane.polyline.first()
        {
            dashed_hline(
                ui,
                graph.x,
                graph.x_max(),
                y,
                color::AUTOMATION_LINE_THICKNESS,
                accent,
            );
        }
        for pair in lane.polyline.windows(2).filter(|_| !lane.dots.is_empty()) {
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
            // A value past the slider range sits pinned at the lane's edge;
            // an amber ring marks it, as the row's thumb does.
            if point.past_end {
                dot(ui, point.x, point.y, 5.5, color::SLIDER_PAST_END_C32);
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
        draw_title(ui, measure, lane, overridden, feedback);
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
            // The point tooltip is deliberately compact so long hints never
            // overflow the lane.
            if r.width >= 64.0 && r.height >= 24.0 {
                let show_point_tooltip = matches!(
                    f.operation,
                    AutomationOperation::Point
                        | AutomationOperation::Insert
                        | AutomationOperation::Draw
                        | AutomationOperation::Segment
                        | AutomationOperation::Bend
                );
                let mut readout_bytes = [0_u8; 512];
                let mut value_buf = ParamValueText::default();
                let value_text = lane.display.format_value(
                    &mut value_buf,
                    f.value,
                    lane.param_min,
                    lane.whole_numbers,
                );
                let readout_text = format_readout(
                    &mut readout_bytes,
                    Some(&lane.label),
                    Some(f.beat.0),
                    lane.beats_per_bar,
                    value_text,
                );
                let text_w = measure
                    .measure_text(
                        readout_text,
                        color::AUTOMATION_LABEL_FONT,
                        FontWeight::Medium,
                    )
                    .x;
                let bubble_w = (text_w + 12.0).min(r.width);
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
                    // this local readout above the curve and the lane title.
                    ui.push_depth(ui.current_depth().above(2));
                    ui.draw_rounded_rect(
                        bubble_x,
                        bubble_y,
                        bubble_w,
                        bubble_h,
                        Color32::new(20, 23, 28, 245),
                        3.0,
                    );
                    ui.draw_text(
                        bubble_x + 6.0,
                        bubble_y + 3.0,
                        readout_text,
                        color::AUTOMATION_LABEL_FONT as f32,
                        color::TEXT_WHITE_C32,
                    );
                    ui.pop_depth();
                }
            }
        }
        ui.pop_immediate_clip();
    }
    ui.pop_immediate_clip();
}

#[cfg(test)]
mod tests {
    use super::{format_readout, musical_position};

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

    #[test]
    fn long_lane_label_truncates_without_panicking() {
        let label = "é".repeat(600);
        let mut bytes = [0_u8; 512];
        let text = format_readout(&mut bytes, Some(&label), Some(4.0), 4.0, "0.50 s");
        assert!(text.len() <= 512);
        assert!(text.ends_with("0.50 s"));
    }
}
