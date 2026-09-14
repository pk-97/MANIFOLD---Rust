//! Shared automation clipboard/editing operations.
//!
//! The keyboard path and the typed lane context menu both call these helpers.
//! Keeping command construction here ensures that menu edits retain the same
//! range conversion, collision handling, and undo grouping as shortcuts.

use manifold_core::effects::{AutomationPoint, SegmentShape};
use manifold_core::{Beats, GraphTarget};
use manifold_editing::command::Command;
use manifold_editing::commands::automation::{
    AddAutomationPointCommand, CommitRecordedGestureCommand, RemoveAutomationPointCommand,
};
use manifold_ui::panels::actions::AutomationShape;
use manifold_ui::ui_state::{AutomationClipboard, AutomationClipboardPoint, UIState};
use manifold_ui::view::{UiAutomationPointRef, UiGraphTarget, UiSegmentShape};

use crate::content_command::ContentCommand;

fn selected_refs(selection: &UIState) -> Vec<UiAutomationPointRef> {
    let mut refs = selection.selected_automation_points.clone();
    if let Some(point) = selection.selected_automation_point.clone()
        && !refs.contains(&point)
    {
        refs.push(point);
    }
    refs
}

/// Make a context-menu lane the only automation context used by an action.
/// This is deliberately a projection of UI selection, never a project write.
pub(crate) fn set_lane_context(
    selection: &mut UIState,
    target: &UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
) {
    let before = selection.selected_automation_points.len()
        + usize::from(selection.selected_automation_point.is_some());
    selection
        .selected_automation_points
        .retain(|point| point.target == *target && point.param_id == *param_id);
    if selection
        .selected_automation_point
        .as_ref()
        .is_some_and(|point| point.target != *target || point.param_id != *param_id)
    {
        selection.selected_automation_point = None;
    }
    selection.automation_paste_context = Some((target.clone(), param_id.clone()));
    let after = selection.selected_automation_points.len()
        + usize::from(selection.selected_automation_point.is_some());
    if before != after {
        selection.selection_version = selection.selection_version.wrapping_add(1);
    }
}

pub(crate) fn select_all_in_lane(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    target: &UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
) {
    set_lane_context(selection, target, param_id);
    let graph_target = graph_target(target);
    let Some(instance) = project.preset_instance(&graph_target) else {
        return;
    };
    let lane_points = instance
        .automation_lanes
        .as_ref()
        .and_then(|lanes| lanes.iter().find(|lane| lane.param_id == *param_id))
        .map(|lane| lane.points.clone());
    selection.selected_automation_points = lane_points
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|point| UiAutomationPointRef {
            target: target.clone(),
            param_id: param_id.clone(),
            beat: point.beat,
        })
        .collect();
    selection.selected_automation_point = selection.selected_automation_points.first().cloned();
    selection.selection_version = selection.selection_version.wrapping_add(1);
}

fn destination(selection: &UIState) -> Option<(UiGraphTarget, manifold_core::effects::ParamId)> {
    if let Some(point) = &selection.selected_automation_point {
        return Some((point.target.clone(), point.param_id.clone()));
    }
    if selection.selected_automation_points.len() == 1 {
        let point = &selection.selected_automation_points[0];
        return Some((point.target.clone(), point.param_id.clone()));
    }
    selection.automation_paste_context.clone()
}

fn lane_count(clipboard: &AutomationClipboard) -> usize {
    let mut lanes: Vec<(&UiGraphTarget, &manifold_core::effects::ParamId)> = Vec::new();
    for point in &clipboard.points {
        if !lanes
            .iter()
            .any(|(target, param)| **target == point.target && **param == point.param_id)
        {
            lanes.push((&point.target, &point.param_id));
        }
    }
    lanes.len()
}

fn from_core_shape(shape: SegmentShape) -> UiSegmentShape {
    match shape {
        SegmentShape::Linear => UiSegmentShape::Linear,
        SegmentShape::Hold => UiSegmentShape::Hold,
        SegmentShape::Curved(bend) => UiSegmentShape::Curved(bend),
        SegmentShape::CurvedRange { bend, start, end } => {
            UiSegmentShape::CurvedRange { bend, start, end }
        }
    }
}

fn to_core_shape(shape: UiSegmentShape) -> SegmentShape {
    match shape {
        UiSegmentShape::Linear => SegmentShape::Linear,
        UiSegmentShape::Hold => SegmentShape::Hold,
        UiSegmentShape::Curved(bend) => SegmentShape::Curved(bend),
        UiSegmentShape::CurvedRange { bend, start, end } => {
            SegmentShape::CurvedRange { bend, start, end }
        }
    }
}

fn graph_target(target: &UiGraphTarget) -> GraphTarget {
    crate::editing_host::to_graph_target(target)
}

fn shape_range(selected_beats: &[f64], click_beat: Beats, beats_per_bar: f64) -> (Beats, Beats) {
    if selected_beats.len() >= 2 {
        (
            Beats(selected_beats[0]),
            Beats(*selected_beats.last().expect("at least two selected beats")),
        )
    } else {
        let beats_per_bar = beats_per_bar.max(1.0);
        (click_beat, click_beat + Beats(beats_per_bar))
    }
}

/// Copy the currently selected points into the shared UI clipboard.
pub(crate) fn copy_selected(project: &manifold_core::project::Project, selection: &mut UIState) {
    let mut found = Vec::new();
    let mut min_beat = f64::INFINITY;
    let mut max_beat = f64::NEG_INFINITY;

    for point_ref in selected_refs(selection) {
        let target = graph_target(&point_ref.target);
        let Some(instance) = project.preset_instance(&target) else {
            continue;
        };
        let Some(lane) = instance.automation_lanes.as_ref().and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.param_id == point_ref.param_id)
        }) else {
            continue;
        };
        let Some(point) = lane
            .points
            .iter()
            .find(|point| point.beat == point_ref.beat)
        else {
            continue;
        };
        let Some(param) = instance.params.get(point_ref.param_id.as_ref()) else {
            continue;
        };
        let range = param.spec.max - param.spec.min;
        let value_norm = if range.abs() > f32::EPSILON {
            (point.value - param.spec.min) / range
        } else {
            0.0
        };
        min_beat = min_beat.min(point.beat.0);
        max_beat = max_beat.max(point.beat.0);
        found.push(AutomationClipboardPoint {
            target: point_ref.target,
            param_id: point_ref.param_id,
            beat_offset: point.beat,
            value_norm: value_norm.clamp(0.0, 1.0),
            value: point.value,
            source_min: param.spec.min,
            source_max: param.spec.max,
            shape: from_core_shape(point.shape),
        });
    }

    if found.is_empty() {
        return;
    }
    let origin = Beats(min_beat);
    for point in &mut found {
        point.beat_offset -= origin;
    }
    selection.automation_paste_context = found
        .first()
        .map(|point| (point.target.clone(), point.param_id.clone()));
    selection.automation_clipboard = Some(AutomationClipboard {
        points: found,
        span: Beats((max_beat - min_beat).max(0.0)),
    });
}

/// Delete selected points as one undoable batch. Missing targets/points are
/// skipped without changing the project.
pub(crate) fn delete_selected(
    project: &mut manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    needs_rebuild: &mut bool,
) {
    use std::collections::HashMap;

    let refs = selected_refs(selection);
    selection.selected_automation_points.clear();
    selection.selected_automation_point = None;
    if refs.is_empty() {
        return;
    }
    let mut by_lane: HashMap<(GraphTarget, String), Vec<f64>> = HashMap::new();
    for point in &refs {
        by_lane
            .entry((
                graph_target(&point.target),
                point.param_id.as_ref().to_string(),
            ))
            .or_default()
            .push(point.beat.0);
    }
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for ((target, param_id), beats) in by_lane {
        let Some(inst) = project.preset_instance(&target) else {
            continue;
        };
        let Some(lane) = inst
            .automation_lanes
            .as_ref()
            .and_then(|lanes| lanes.iter().find(|lane| lane.param_id.as_ref() == param_id))
        else {
            continue;
        };
        let mut indices: Vec<usize> = beats
            .iter()
            .filter_map(|beat| lane.points.iter().position(|point| point.beat.0 == *beat))
            .collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.dedup();
        for index in indices {
            commands.push(Box::new(RemoveAutomationPointCommand::new(
                target.clone(),
                param_id.clone(),
                index,
            )));
        }
    }
    if commands.is_empty() {
        *needs_rebuild = true;
        return;
    }
    for command in &mut commands {
        command.execute(project);
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Delete Automation Points".to_string()),
    );
    *needs_rebuild = true;
}

pub(crate) fn cut_selected(
    project: &mut manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    needs_rebuild: &mut bool,
) {
    let refs = selected_refs(selection);
    if refs.is_empty() {
        return;
    }
    copy_selected(project, selection);
    selection.selected_automation_point = None;
    selection.selected_automation_points = refs;
    delete_selected(project, selection, content_tx, needs_rebuild);
    if let Some(point) = selection
        .automation_clipboard
        .as_ref()
        .and_then(|clipboard| clipboard.points.first())
    {
        selection.automation_paste_context = Some((point.target.clone(), point.param_id.clone()));
    }
}

pub(crate) fn paste(
    project: &mut manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    target_beat: Beats,
    needs_rebuild: &mut bool,
) {
    let Some(clipboard) = selection.automation_clipboard.clone() else {
        return;
    };
    let destination = destination(selection);
    let single_lane = lane_count(&clipboard) == 1;
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let mut inserted = Vec::new();
    for point in clipboard.points {
        let (ui_target, param_id) = if single_lane {
            destination
                .clone()
                .unwrap_or((point.target.clone(), point.param_id.clone()))
        } else {
            (point.target.clone(), point.param_id.clone())
        };
        let target = graph_target(&ui_target);
        let Some(instance) = project.preset_instance(&target) else {
            continue;
        };
        let Some(param) = instance.params.get(param_id.as_ref()) else {
            continue;
        };
        let same_range = (point.source_min - param.spec.min).abs() <= f32::EPSILON
            && (point.source_max - param.spec.max).abs() <= f32::EPSILON;
        let mut value = if same_range {
            point.value
        } else {
            param.spec.min + point.value_norm * (param.spec.max - param.spec.min)
        };
        value = value.clamp(param.spec.min, param.spec.max);
        if param.whole_numbers() {
            value = value.round().clamp(param.spec.min, param.spec.max);
        }
        let beat = target_beat + point.beat_offset;
        let point = AutomationPoint {
            beat,
            value,
            shape: if param.whole_numbers() {
                SegmentShape::Hold
            } else {
                to_core_shape(point.shape)
            },
        };
        let mut command = AddAutomationPointCommand::new(target, param_id.as_ref(), point);
        command.execute(project);
        commands.push(Box::new(command));
        inserted.push(UiAutomationPointRef {
            target: ui_target,
            param_id,
            beat,
        });
    }
    if commands.is_empty() {
        return;
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Paste Automation".to_string()),
    );
    selection.selected_automation_point = inserted.first().cloned();
    selection.selected_automation_points = inserted;
    *needs_rebuild = true;
}

pub(crate) fn duplicate_selected(
    project: &mut manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    grid_step: f32,
    needs_rebuild: &mut bool,
) {
    let refs = selected_refs(selection);
    if refs.is_empty() {
        return;
    }
    let min_beat = refs
        .iter()
        .map(|point| point.beat.0)
        .fold(f64::INFINITY, f64::min);
    let max_beat = refs
        .iter()
        .map(|point| point.beat.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let destination = Beats(max_beat + f64::from(grid_step.max(f32::EPSILON)));
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let mut inserted = Vec::new();
    for point_ref in refs {
        let target = graph_target(&point_ref.target);
        let Some(instance) = project.preset_instance(&target) else {
            continue;
        };
        let Some(lane) = instance.automation_lanes.as_ref().and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.param_id == point_ref.param_id)
        }) else {
            continue;
        };
        let Some(point) = lane
            .points
            .iter()
            .find(|point| point.beat == point_ref.beat)
        else {
            continue;
        };
        let beat = destination + (point.beat - Beats(min_beat));
        let mut command = AddAutomationPointCommand::new(
            target,
            point_ref.param_id.as_ref(),
            AutomationPoint {
                beat,
                value: point.value,
                shape: point.shape,
            },
        );
        command.execute(project);
        commands.push(Box::new(command));
        inserted.push(UiAutomationPointRef {
            target: point_ref.target,
            param_id: point_ref.param_id,
            beat,
        });
    }
    if commands.is_empty() {
        return;
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Duplicate Automation".to_string()),
    );
    selection.selected_automation_point = inserted.first().cloned();
    selection.selected_automation_points = inserted;
    *needs_rebuild = true;
}

/// Build a phrase replacement while retaining the points on either side of
/// the replaced range.  A curved segment that crosses a replacement edge is
/// clipped with `SegmentShape::subrange`, so the surviving outer segment keeps
/// its original curve rather than being reset to linear interpolation.
fn shape_points(
    old_points: Option<&[AutomationPoint]>,
    start: Beats,
    end: Beats,
    min: f32,
    max: f32,
    whole_numbers: bool,
    shape: AutomationShape,
) -> Vec<AutomationPoint> {
    let span = (end.0 - start.0).max(0.0);
    let old = old_points.unwrap_or_default();
    let value = |norm: f32| {
        let raw = min + norm.clamp(0.0, 1.0) * (max - min);
        if whole_numbers {
            raw.round().clamp(min, max)
        } else {
            raw.clamp(min, max)
        }
    };
    let authored_shape = |segment: SegmentShape| {
        if whole_numbers {
            SegmentShape::Hold
        } else {
            segment
        }
    };

    let phrase: Vec<(f64, f32, SegmentShape)> = match shape {
        AutomationShape::RampUp => vec![
            (0.0, 0.0, SegmentShape::Linear),
            (1.0, 1.0, SegmentShape::Linear),
        ],
        AutomationShape::RampDown => vec![
            (0.0, 1.0, SegmentShape::Linear),
            (1.0, 0.0, SegmentShape::Linear),
        ],
        AutomationShape::Triangle => vec![
            (0.0, 0.0, SegmentShape::Linear),
            (0.5, 1.0, SegmentShape::Linear),
            (1.0, 0.0, SegmentShape::Linear),
        ],
        AutomationShape::Sine => {
            const SAMPLES: usize = 17;
            (0..SAMPLES)
                .map(|i| {
                    let t = i as f64 / (SAMPLES - 1) as f64;
                    let n = (0.5 - 0.5 * (std::f64::consts::TAU * t).cos()) as f32;
                    (t, n, SegmentShape::Linear)
                })
                .collect()
        }
        AutomationShape::Square => vec![
            (0.0, 0.0, SegmentShape::Hold),
            (0.5, 1.0, SegmentShape::Hold),
            (1.0, 0.0, SegmentShape::Hold),
        ],
        AutomationShape::HoldLow => vec![
            (0.0, 0.0, SegmentShape::Hold),
            (1.0, 0.0, SegmentShape::Hold),
        ],
        AutomationShape::HoldHigh => vec![
            (0.0, 1.0, SegmentShape::Hold),
            (1.0, 1.0, SegmentShape::Hold),
        ],
    };

    let phrase: Vec<AutomationPoint> = phrase
        .into_iter()
        .map(|(t, norm, segment)| AutomationPoint {
            beat: Beats(start.0 + t * span),
            value: value(norm),
            shape: authored_shape(segment),
        })
        .collect();
    manifold_playback::automation::punch_recorded_points(old, &phrase)
}

/// Insert a basic phrase shape into one lane. The selected lane span is
/// replaced when it contains two distinct beats; otherwise one bar in the
/// project's time signature beginning at the snapped context-menu beat is
/// replaced.
pub(crate) fn insert_shape(
    project: &mut manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    target: &UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
    click_beat: Beats,
    shape: AutomationShape,
    needs_rebuild: &mut bool,
) {
    set_lane_context(selection, target, param_id);
    let graph_target = graph_target(target);
    let Some(instance) = project.preset_instance(&graph_target) else {
        return;
    };
    let Some(param) = instance.params.get(param_id.as_ref()) else {
        return;
    };
    let lane_points = instance
        .automation_lanes
        .as_ref()
        .and_then(|lanes| lanes.iter().find(|lane| lane.param_id == *param_id))
        .map(|lane| lane.points.clone());

    let mut selected_beats: Vec<f64> = selected_refs(selection)
        .into_iter()
        .filter(|point| point.target == *target && point.param_id == *param_id)
        .map(|point| point.beat.0)
        .collect();
    selected_beats.sort_by(f64::total_cmp);
    selected_beats.dedup();
    let beats_per_bar = project.settings.time_signature_numerator.max(1) as f64;
    let (start, end) = shape_range(&selected_beats, click_beat, beats_per_bar);
    let points = shape_points(
        lane_points.as_deref(),
        start,
        end,
        param.spec.min,
        param.spec.max,
        param.whole_numbers(),
        shape,
    );
    let old_points = lane_points;
    let mut command = CommitRecordedGestureCommand::new(
        graph_target,
        param_id.as_ref(),
        points.clone(),
        old_points,
    );
    command.execute(project);
    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(command)));
    selection.selected_automation_points = points
        .iter()
        .filter(|point| point.beat.0 >= start.0 && point.beat.0 <= end.0)
        .map(|point| UiAutomationPointRef {
            target: target.clone(),
            param_id: param_id.clone(),
            beat: point.beat,
        })
        .collect();
    selection.selected_automation_point = selection.selected_automation_points.first().cloned();
    *needs_rebuild = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape,
        }
    }

    #[test]
    fn shape_range_uses_non_four_bar_signature_without_selection() {
        let (start, end) = shape_range(&[], Beats(8.0), 3.0);
        assert_eq!(start, Beats(8.0));
        assert_eq!(end, Beats(11.0));
    }

    #[test]
    fn shape_range_uses_selected_span_when_two_distinct_points_exist() {
        let (start, end) = shape_range(&[2.0, 7.0], Beats(20.0), 4.0);
        assert_eq!((start, end), (Beats(2.0), Beats(7.0)));
    }

    #[test]
    fn empty_copy_keeps_the_existing_clipboard() {
        let mut selection = UIState::new();
        let clipboard = AutomationClipboard {
            points: vec![AutomationClipboardPoint {
                target: UiGraphTarget::Effect(manifold_core::EffectId::new("source")),
                param_id: "amount".into(),
                beat_offset: Beats(0.0),
                value_norm: 0.5,
                value: 0.5,
                source_min: 0.0,
                source_max: 1.0,
                shape: UiSegmentShape::Linear,
            }],
            span: Beats(0.0),
        };
        selection.automation_clipboard = Some(clipboard.clone());
        selection.automation_paste_context = Some((
            UiGraphTarget::Effect(manifold_core::EffectId::new("source")),
            "amount".into(),
        ));

        copy_selected(&manifold_core::project::Project::default(), &mut selection);

        assert_eq!(selection.automation_clipboard, Some(clipboard));
        assert!(selection.automation_paste_context.is_some());
    }

    #[test]
    fn shape_without_lane_covers_target_range() {
        let points = shape_points(
            None,
            Beats(2.0),
            Beats(5.0),
            -2.0,
            6.0,
            false,
            AutomationShape::RampUp,
        );
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].beat, Beats(2.0));
        assert_eq!(points[1].beat, Beats(5.0));
        assert_eq!(points[0].value, -2.0);
        assert_eq!(points[1].value, 6.0);
    }

    #[test]
    fn shape_preserves_outer_points_and_clips_crossing_curves() {
        let old = [
            point(0.0, 0.0, SegmentShape::Curved(0.6)),
            point(10.0, 1.0, SegmentShape::Hold),
            point(20.0, 0.5, SegmentShape::Linear),
        ];
        let points = shape_points(
            Some(&old),
            Beats(4.0),
            Beats(8.0),
            0.0,
            1.0,
            false,
            AutomationShape::RampUp,
        );
        let original = manifold_core::effects::AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: old.to_vec(),
        };
        let replaced = manifold_core::effects::AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points,
        };
        for beat in [0.0, 1.0, 3.9, 8.1, 9.0, 10.0, 16.0, 24.0] {
            assert!((original.value_at(Beats(beat)) - replaced.value_at(Beats(beat))).abs() < 1e-5);
        }
        assert_eq!(replaced.value_at(Beats(4.0)), 0.0);
        assert_eq!(replaced.value_at(Beats(8.0)), 1.0);
    }

    #[test]
    fn discrete_shape_rounds_clamps_and_forces_hold_segments() {
        let points = shape_points(
            None,
            Beats(0.0),
            Beats(4.0),
            0.0,
            3.0,
            true,
            AutomationShape::Triangle,
        );
        assert_eq!(
            points.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![0.0, 3.0, 0.0]
        );
        assert!(points.iter().all(|point| point.shape == SegmentShape::Hold));

        let clamped = shape_points(
            None,
            Beats(0.0),
            Beats(1.0),
            0.0,
            1.0,
            true,
            AutomationShape::HoldHigh,
        );
        assert!(
            clamped
                .iter()
                .all(|point| (0.0..=1.0).contains(&point.value))
        );
    }
}
