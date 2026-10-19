//! Numeric type-in for automation breakpoints.
//!
//! The text field is still owned by [`crate::text_input::TextInputState`]; this
//! module keeps the point lookup, notation parsing, and command boundary out of
//! the application frame loop.

use manifold_core::effects::{AutomationPoint, ParamId};
use manifold_core::{Beats, GraphTarget};
use manifold_editing::commands::automation::MoveAutomationPointCommand;
use manifold_ui::view::{UiGraphTarget, UiLaneDisplay};
use manifold_ui::slider::BitmapSlider;

use crate::app::Application;
use crate::text_input::{AnchorRect, AutomationPointEditCtx, TextInputField, TextInputState};
use crate::ui_root::UIRoot;

const INPUT_WIDTH: f32 = 128.0;
const INPUT_HEIGHT: f32 = 21.0;

/// Open a value editor for the exact point addressed by the action.
pub(crate) fn begin_value(
    app: &mut Application,
    target: &UiGraphTarget,
    param_id: &ParamId,
    beat: Beats,
    value_norm: f32,
) {
    let Some((point, param)) = point_and_param(app, target, param_id, beat, value_norm) else {
        invalid(&mut app.ws.ui_root, "Automation point is no longer available");
        return;
    };
    let ctx = AutomationPointEditCtx {
        target: target.clone(),
        param_id: param_id.clone(),
        original_beat: beat,
        original_value_norm: value_norm,
        param_min: param.slider_span().0,
        param_max: param.slider_span().1,
        whole_numbers: param.whole_numbers(),
        beats_per_bar: beats_per_bar(app),
    };
    // The editor opens on the value as the inspector slider prints it.
    let mut buf = manifold_ui::panels::param_slider_shared::ParamValueText::default();
    let initial = lane_display(&app.ws.ui_root, target, param_id)
        .format_value(&mut buf, point.value, ctx.param_min, ctx.whole_numbers)
        .to_owned();
    let Some(anchor) = point_anchor(app, target, param_id, beat, value_norm) else {
        invalid(&mut app.ws.ui_root, "Automation point is off-screen");
        return;
    };
    app.text_input.begin(
        TextInputField::AutomationPointValue,
        &initial,
        anchor,
        11.0,
    );
    app.text_input.automation_point_edit = Some(ctx);
}

/// Open a time editor for the exact point addressed by the action.
pub(crate) fn begin_time(
    app: &mut Application,
    target: &UiGraphTarget,
    param_id: &ParamId,
    beat: Beats,
    value_norm: f32,
) {
    let Some((_, param)) = point_and_param(app, target, param_id, beat, value_norm) else {
        invalid(&mut app.ws.ui_root, "Automation point is no longer available");
        return;
    };
    let bpb = beats_per_bar(app);
    let ctx = AutomationPointEditCtx {
        target: target.clone(),
        param_id: param_id.clone(),
        original_beat: beat,
        original_value_norm: value_norm,
        param_min: param.slider_span().0,
        param_max: param.slider_span().1,
        whole_numbers: param.whole_numbers(),
        beats_per_bar: bpb,
    };
    let initial = format_bar_beat(beat, bpb);
    let Some(anchor) = point_anchor(app, target, param_id, beat, value_norm) else {
        invalid(&mut app.ws.ui_root, "Automation point is off-screen");
        return;
    };
    app.text_input.begin(
        TextInputField::AutomationPointTime,
        &initial,
        anchor,
        11.0,
    );
    app.text_input.automation_point_edit = Some(ctx);
}

/// Commit either automation point field. Invalid text leaves the project and
/// selection untouched while the existing toast overlay reports the reason.
pub(crate) fn commit(
    text_input: &mut TextInputState,
    cx: &mut crate::ui_bridge::DispatchCtx<'_>,
    field: TextInputField,
    text: &str,
) {
    let Some(ctx) = text_input.automation_point_edit.take() else {
        invalid(cx.ui, "Automation point edit context expired");
        return;
    };
    let new_value = if field == TextInputField::AutomationPointValue {
        let display = lane_display(cx.ui, &ctx.target, &ctx.param_id);
        match parse_value(text, ctx.param_min, ctx.param_max, ctx.whole_numbers, &display) {
            Ok(value) => value,
            Err(reason) => {
                reject_edit(text_input, cx.ui, field, text, ctx, reason);
                return;
            }
        }
    } else {
        // Time edits preserve the point's value and only replace its beat.
        match parse_bar_beat(text, ctx.beats_per_bar) {
            Ok(_) => 0.0,
            Err(reason) => {
                reject_edit(text_input, cx.ui, field, text, ctx, reason);
                return;
            }
        }
    };

    let graph_target = crate::editing_host::to_graph_target(&ctx.target);
    let Some(old_point) = find_point(&cx.project, &graph_target, &ctx.param_id, ctx.original_beat, ctx.original_value_norm) else {
        invalid(cx.ui, "Automation point moved before the edit was committed");
        return;
    };
    let new_beat = if field == TextInputField::AutomationPointValue {
        ctx.original_beat
    } else {
        // Parsing above is repeated only to keep the invalid branch before any
        // project access; it is a small, bounded text-input operation.
        parse_bar_beat(text, ctx.beats_per_bar).expect("validated automation time")
    };
    if !new_beat.0.is_finite() || new_beat.0 < 0.0 {
        invalid(cx.ui, "Automation time must be finite and non-negative");
        return;
    }
    let value = if field == TextInputField::AutomationPointValue {
        new_value
    } else {
        old_point.value
    };
    let new_point = AutomationPoint {
        beat: new_beat,
        value,
        shape: old_point.shape,
    };
    if old_point.beat == new_point.beat && old_point.value == new_point.value {
        return;
    }
    cx.execute(Box::new(MoveAutomationPointCommand::new(
        graph_target, ctx.param_id.as_ref(), old_point, new_point,
    )));
    let new_value_norm = BitmapSlider::value_to_normalized(value, ctx.param_min, ctx.param_max);
    cx.selection.selected_automation_point = Some(manifold_ui::view::UiAutomationPointRef {
        target: ctx.target,
        param_id: ctx.param_id,
        beat: new_beat,
        value_norm: new_value_norm,
    });
    cx.selection.selected_automation_points.clear();
    cx.ui.mark_overlay_dirty();
}

fn point_and_param<'a>(
    app: &'a Application,
    target: &UiGraphTarget,
    param_id: &ParamId,
    beat: Beats,
    value_norm: f32,
) -> Option<(&'a AutomationPoint, &'a manifold_core::params::Param)> {
    let graph_target = crate::editing_host::to_graph_target(target);
    let inst = app.local_project.preset_instance(&graph_target)?;
    let lane = inst
        .automation_lanes
        .as_ref()?
        .iter()
        .find(|lane| lane.param_id.as_ref() == param_id.as_ref())?;
    let param = inst.params.get(param_id.as_ref())?;
    let point = lane.points.iter().find(|point| {
        point.beat == beat
            && BitmapSlider::value_to_normalized(point.value, param.slider_span().0, param.slider_span().1) == value_norm
    })?;
    Some((point, param))
}

fn find_point(
    project: &manifold_core::project::Project,
    target: &GraphTarget,
    param_id: &ParamId,
    beat: Beats,
    value_norm: f32,
) -> Option<AutomationPoint> {
    let inst = project.preset_instance(target)?;
    let param = inst.params.get(param_id.as_ref())?;
    inst
        .automation_lanes
        .as_ref()?
        .iter()
        .find(|lane| lane.param_id.as_ref() == param_id.as_ref())?
        .points
        .iter()
        .find(|point| point.beat == beat && BitmapSlider::value_to_normalized(point.value, param.slider_span().0, param.slider_span().1) == value_norm)
        .copied()
}

fn point_anchor(app: &Application, target: &UiGraphTarget, param_id: &ParamId, beat: Beats, value_norm: f32) -> Option<AnchorRect> {
    let dot = app.ws.ui_root.viewport.automation_lane_screens()
        .iter()
        .find(|lane| lane.target == *target && lane.param_id == *param_id)
        .and_then(|lane| lane.dots.iter().find(|dot| dot.beat == beat && dot.value_norm == value_norm).copied())?;
    let layout = &app.ws.ui_root.layout;
    Some(anchor_next_to_point(dot.x, dot.y, layout.screen_width, layout.screen_height))
}

fn anchor_next_to_point(point_x: f32, point_y: f32, screen_width: f32, screen_height: f32) -> AnchorRect {
    let x = if point_x + 8.0 + INPUT_WIDTH <= screen_width {
        (point_x + 8.0).max(0.0)
    } else {
        (point_x - 8.0 - INPUT_WIDTH).clamp(0.0, (screen_width - INPUT_WIDTH).max(0.0))
    };
    let y = (point_y - INPUT_HEIGHT * 0.5).clamp(0.0, (screen_height - INPUT_HEIGHT).max(0.0));
    AnchorRect::new(x, y, INPUT_WIDTH, INPUT_HEIGHT)
}

fn beats_per_bar(app: &Application) -> u32 {
    app.local_project.settings.time_signature_numerator.max(1) as u32
}

/// Convert a point beat to the user-facing `bar.beat.fraction` form. Bars and
/// beats are one-based; fraction is thousandths of the beat.
pub(crate) fn format_bar_beat(beat: Beats, beats_per_bar: u32) -> String {
    let raw = beat.0.max(0.0);
    let mut whole = raw.floor() as u64;
    let mut fraction = ((raw - whole as f64) * 1000.0).round() as u64;
    if fraction >= 1000 {
        whole += 1;
        fraction = 0;
    }
    let bar = whole / beats_per_bar.max(1) as u64 + 1;
    let in_bar = whole % beats_per_bar.max(1) as u64 + 1;
    format!("{bar}.{in_bar}.{fraction:03}")
}

/// Parse one-based `bar.beat.fraction` notation into absolute beats.
pub(crate) fn parse_bar_beat(text: &str, beats_per_bar: u32) -> Result<Beats, &'static str> {
    let mut parts = text.trim().split('.');
    let (Some(bar), Some(beat), Some(fraction), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("Invalid automation time; expected bar.beat.fraction");
    };
    if bar.is_empty()
        || beat.is_empty()
        || fraction.is_empty()
        || !bar.chars().all(|c| c.is_ascii_digit())
        || !beat.chars().all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return Err("Invalid automation time; expected bar.beat.fraction");
    }
    let bar = bar
        .parse::<u64>()
        .map_err(|_| "Automation bar is out of range")?;
    let beat = beat
        .parse::<u64>()
        .map_err(|_| "Automation beat is out of range")?;
    let fraction = fraction
        .parse::<u64>()
        .map_err(|_| "Automation fraction is out of range")?;
    let bpb = u64::from(beats_per_bar.max(1));
    if bar == 0 || beat == 0 || beat > bpb || fraction > 999 {
        return Err("Automation time must use a valid bar, beat, and 0..999 fraction");
    }
    let absolute = (bar - 1) as f64 * bpb as f64 + (beat - 1) as f64 + fraction as f64 / 1000.0;
    if !absolute.is_finite() {
        return Err("Automation time must be finite");
    }
    Ok(Beats(absolute))
}

/// How the lane prints this parameter; plain numbers when the lane is gone.
fn lane_display(ui: &UIRoot, target: &UiGraphTarget, param_id: &ParamId) -> UiLaneDisplay {
    ui.viewport.automation_lane_screens()
        .iter()
        .find(|lane| &lane.target == target && lane.param_id == *param_id)
        .map(|lane| lane.display.clone())
        .unwrap_or_default()
}

/// Parse, clamp, and round a point value in parameter units. Reads what the
/// editor opened on: an enum name, an angle in degrees, or a number with its
/// unit suffix.
pub(crate) fn parse_value(
    text: &str,
    min: f32,
    max: f32,
    whole_numbers: bool,
    display: &UiLaneDisplay,
) -> Result<f32, &'static str> {
    if !min.is_finite() || !max.is_finite() || min > max {
        return Err("Automation parameter range is invalid");
    }
    let text = text.trim();
    if let Some(index) = display
        .value_labels
        .as_deref()
        .and_then(|labels| labels.iter().position(|label| label.eq_ignore_ascii_case(text)))
    {
        return Ok((min + index as f32).clamp(min, max));
    }
    let mut number = text;
    if let Some(unit) = display.unit {
        number = number.strip_suffix(unit.suffix().trim()).unwrap_or(number).trim();
    }
    if display.is_angle {
        number = number.strip_suffix('°').unwrap_or(number).trim();
    }
    let parsed = number
        .parse::<f32>()
        .map_err(|_| "Invalid automation value")?;
    if !parsed.is_finite() {
        return Err("Automation value must be finite");
    }
    // Angles are shown and typed in degrees, stored in radians.
    let parsed = if display.is_angle { parsed.to_radians() } else { parsed };
    let mut value = parsed.clamp(min, max);
    if whole_numbers {
        value = value.round().clamp(min, max);
    }
    Ok(value)
}

fn invalid(ui: &mut UIRoot, message: impl Into<String>) {
    ui.toast.show_with_accent(message.into(), manifold_ui::color::RED_BASE);
    ui.mark_overlay_dirty();
}

/// Reopen the same session with the rejected text so the field stays visible
/// and the user can correct it in place.
fn reject_edit(
    text_input: &mut TextInputState,
    ui: &mut UIRoot,
    field: TextInputField,
    text: &str,
    ctx: AutomationPointEditCtx,
    message: &str,
) {
    let anchor = text_input.anchor;
    let font_size = text_input.font_size;
    text_input.begin(field, text, anchor, font_size);
    text_input.automation_point_edit = Some(ctx);
    invalid(ui, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_editor_stays_adjacent_and_inside_window_edges() {
        let middle = anchor_next_to_point(300.0, 200.0, 800.0, 600.0);
        assert_eq!((middle.x, middle.y), (308.0, 189.5));
        let bottom_right = anchor_next_to_point(790.0, 598.0, 800.0, 600.0);
        assert_eq!((bottom_right.x, bottom_right.y), (654.0, 579.0));
        let top = anchor_next_to_point(100.0, 2.0, 800.0, 600.0);
        assert_eq!(top.y, 0.0);
    }

    #[test]
    fn bar_beat_uses_non_four_signature() {
        assert_eq!(parse_bar_beat("2.3.125", 3).unwrap(), Beats(5.125));
        assert_eq!(format_bar_beat(Beats(5.125), 3), "2.3.125");
    }

    #[test]
    fn bar_beat_rejects_invalid_and_negative_input() {
        for text in ["0.1.0", "1.0.0", "1.4.0", "1.1.1000", "-1.1.0", "nan"] {
            assert!(parse_bar_beat(text, 3).is_err(), "{text}");
        }
    }

    #[test]
    fn value_rejects_nonfinite_and_clamps_rounds() {
        let plain = UiLaneDisplay::default();
        assert_eq!(parse_value("99", -2.0, 5.0, false, &plain).unwrap(), 5.0);
        assert_eq!(parse_value("2.6", 0.0, 5.0, true, &plain).unwrap(), 3.0);
        assert!(parse_value("NaN", 0.0, 1.0, false, &plain).is_err());
        assert!(parse_value("inf", 0.0, 1.0, false, &plain).is_err());
    }

    #[test]
    fn value_reads_back_what_the_editor_opened_on() {
        use manifold_foundation::scene_surface::Unit;
        let mut buf = manifold_ui::panels::param_slider_shared::ParamValueText::default();
        let labels = UiLaneDisplay {
            value_labels: Some(vec!["Identity".to_string(), "Mirror".to_string(), "FoldX".to_string()].into()),
            ..UiLaneDisplay::default()
        };
        assert_eq!(labels.format_value(&mut buf, 2.0, 0.0, true), "FoldX");
        assert_eq!(parse_value("FoldX", 0.0, 2.0, true, &labels).unwrap(), 2.0);
        assert_eq!(parse_value("mirror", 0.0, 2.0, true, &labels).unwrap(), 1.0);
        let seconds = UiLaneDisplay { unit: Some(Unit::Seconds), ..UiLaneDisplay::default() };
        let shown = seconds.format_value(&mut buf, 1.25, 0.0, false).to_owned();
        assert_eq!(shown, "1.25 s");
        assert_eq!(parse_value(&shown, 0.0, 4.0, false, &seconds).unwrap(), 1.25);
        let angle = UiLaneDisplay { is_angle: true, ..UiLaneDisplay::default() };
        let shown = angle.format_value(&mut buf, std::f32::consts::FRAC_PI_2, 0.0, false).to_owned();
        assert_eq!(shown, "90°");
        let parsed = parse_value(&shown, 0.0, 4.0, false, &angle).unwrap();
        assert!((parsed - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn time_has_no_ambiguous_decimal_absolute_form() {
        assert!(parse_bar_beat("2.1", 4).is_err());
    }
}
