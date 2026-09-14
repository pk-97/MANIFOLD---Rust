//! Numeric type-in for automation breakpoints.
//!
//! The text field is still owned by [`crate::text_input::TextInputState`]; this
//! module keeps the point lookup, notation parsing, and command boundary out of
//! the application frame loop.

use manifold_core::effects::{AutomationPoint, ParamId};
use manifold_core::{Beats, GraphTarget};
use manifold_editing::command::Command;
use manifold_editing::commands::automation::MoveAutomationPointCommand;
use manifold_ui::view::UiGraphTarget;

use crate::app::Application;
use crate::text_input::{AnchorRect, AutomationPointEditCtx, TextInputField};

const INPUT_WIDTH: f32 = 128.0;
const INPUT_HEIGHT: f32 = 21.0;

/// Open a value editor for the exact point addressed by the action.
pub(crate) fn begin_value(
    app: &mut Application,
    target: &UiGraphTarget,
    param_id: &ParamId,
    beat: Beats,
) {
    let Some((point, param)) = point_and_param(app, target, param_id, beat) else {
        invalid(app, "Automation point is no longer available");
        return;
    };
    let ctx = AutomationPointEditCtx {
        target: target.clone(),
        param_id: param_id.clone(),
        original_beat: beat,
        param_min: param.spec.min,
        param_max: param.spec.max,
        whole_numbers: param.whole_numbers(),
        beats_per_bar: beats_per_bar(app),
    };
    let initial = if ctx.whole_numbers {
        format!("{}", point.value.round() as i64)
    } else {
        format!("{:.4}", point.value)
    };
    app.text_input.begin(
        TextInputField::AutomationPointValue,
        &initial,
        point_anchor(app),
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
) {
    let Some((_, param)) = point_and_param(app, target, param_id, beat) else {
        invalid(app, "Automation point is no longer available");
        return;
    };
    let bpb = beats_per_bar(app);
    let ctx = AutomationPointEditCtx {
        target: target.clone(),
        param_id: param_id.clone(),
        original_beat: beat,
        param_min: param.spec.min,
        param_max: param.spec.max,
        whole_numbers: param.whole_numbers(),
        beats_per_bar: bpb,
    };
    let initial = format_bar_beat(beat, bpb);
    app.text_input.begin(
        TextInputField::AutomationPointTime,
        &initial,
        point_anchor(app),
        11.0,
    );
    app.text_input.automation_point_edit = Some(ctx);
}

/// Commit either automation point field. Invalid text leaves the project and
/// selection untouched while the existing toast overlay reports the reason.
pub(crate) fn commit(app: &mut Application, field: TextInputField, text: &str) {
    let Some(ctx) = app.text_input.automation_point_edit.take() else {
        invalid(app, "Automation point edit context expired");
        return;
    };
    let new_value = if field == TextInputField::AutomationPointValue {
        match parse_value(text, ctx.param_min, ctx.param_max, ctx.whole_numbers) {
            Ok(value) => value,
            Err(reason) => {
                reject_edit(app, field, text, ctx, reason);
                return;
            }
        }
    } else {
        // Time edits preserve the point's value and only replace its beat.
        match parse_bar_beat(text, ctx.beats_per_bar) {
            Ok(_) => 0.0,
            Err(reason) => {
                reject_edit(app, field, text, ctx, reason);
                return;
            }
        }
    };

    let graph_target = crate::editing_host::to_graph_target(&ctx.target);
    let Some(old_point) = find_point(app, &graph_target, &ctx.param_id, ctx.original_beat) else {
        invalid(app, "Automation point moved before the edit was committed");
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
        invalid(app, "Automation time must be finite and non-negative");
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
    let mut command =
        MoveAutomationPointCommand::new(graph_target, ctx.param_id.as_ref(), old_point, new_point);
    command.execute(&mut app.local_project);
    app.send_content_cmd(crate::content_command::ContentCommand::Execute(Box::new(
        command,
    )));
    if field == TextInputField::AutomationPointTime {
        app.selection.selected_automation_point = Some(manifold_ui::view::UiAutomationPointRef {
            target: ctx.target,
            param_id: ctx.param_id,
            beat: new_beat,
        });
        app.selection.selected_automation_points.clear();
    }
    app.needs_structural_sync = true;
    app.ws.ui_root.overlay_dirty = true;
}

fn point_and_param<'a>(
    app: &'a Application,
    target: &UiGraphTarget,
    param_id: &ParamId,
    beat: Beats,
) -> Option<(&'a AutomationPoint, &'a manifold_core::params::Param)> {
    let graph_target = crate::editing_host::to_graph_target(target);
    let inst = app.local_project.preset_instance(&graph_target)?;
    let lane = inst
        .automation_lanes
        .as_ref()?
        .iter()
        .find(|lane| lane.param_id.as_ref() == param_id.as_ref())?;
    let point = lane.points.iter().find(|point| point.beat == beat)?;
    let param = inst.params.get(param_id.as_ref())?;
    Some((point, param))
}

fn find_point(
    app: &Application,
    target: &GraphTarget,
    param_id: &ParamId,
    beat: Beats,
) -> Option<AutomationPoint> {
    app.local_project
        .preset_instance(target)?
        .automation_lanes
        .as_ref()?
        .iter()
        .find(|lane| lane.param_id.as_ref() == param_id.as_ref())?
        .points
        .iter()
        .find(|point| point.beat == beat)
        .copied()
}

fn point_anchor(app: &Application) -> AnchorRect {
    let pos = app.cursor_pos;
    let layout = &app.ws.ui_root.layout;
    AnchorRect::new(
        pos.x.clamp(0.0, (layout.screen_width - INPUT_WIDTH).max(0.0)),
        pos.y.clamp(0.0, (layout.screen_height - INPUT_HEIGHT).max(0.0)),
        INPUT_WIDTH, INPUT_HEIGHT,
    )
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

/// Parse, clamp, and round a point value in parameter units.
pub(crate) fn parse_value(
    text: &str,
    min: f32,
    max: f32,
    whole_numbers: bool,
) -> Result<f32, &'static str> {
    if !min.is_finite() || !max.is_finite() || min > max {
        return Err("Automation parameter range is invalid");
    }
    let parsed = text
        .trim()
        .parse::<f32>()
        .map_err(|_| "Invalid automation value")?;
    if !parsed.is_finite() {
        return Err("Automation value must be finite");
    }
    let mut value = parsed.clamp(min, max);
    if whole_numbers {
        value = value.round().clamp(min, max);
    }
    Ok(value)
}

fn invalid(app: &mut Application, message: impl Into<String>) {
    app.ws
        .ui_root
        .toast
        .show_with_accent(message.into(), manifold_ui::color::RED_BASE);
    app.ws.ui_root.overlay_dirty = true;
}

fn reject_edit(
    app: &mut Application,
    field: TextInputField,
    text: &str,
    ctx: AutomationPointEditCtx,
    message: &str,
) {
    // `window_input` has already called the shared commit method by the time
    // validation runs. Reopen the same session with the rejected text so the
    // field remains visible and the user can correct it in place.
    let anchor = app.text_input.anchor;
    let font_size = app.text_input.font_size;
    app.text_input.begin(field, text, anchor, font_size);
    app.text_input.automation_point_edit = Some(ctx);
    invalid(app, message);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(parse_value("99", -2.0, 5.0, false).unwrap(), 5.0);
        assert_eq!(parse_value("2.6", 0.0, 5.0, true).unwrap(), 3.0);
        assert!(parse_value("NaN", 0.0, 1.0, false).is_err());
        assert!(parse_value("inf", 0.0, 1.0, false).is_err());
    }

    #[test]
    fn time_has_no_ambiguous_decimal_absolute_form() {
        assert!(parse_bar_beat("2.1", 4).is_err());
    }
}
