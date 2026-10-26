//! Shared automation clipboard/editing operations.
//!
//! The keyboard path and the typed lane context menu both call these helpers.
//! Keeping command construction here ensures that menu edits retain the same
//! range conversion, collision handling, and undo grouping as shortcuts.

use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
use manifold_core::{Beats, GraphTarget};
use manifold_editing::command::Command;
use manifold_editing::commands::automation::{
    CommitRecordedGestureCommand, RemoveAutomationPointCommand,
};
use manifold_ui::panels::actions::AutomationShape;
use manifold_ui::slider::BitmapSlider;
use manifold_ui::ui_state::{
    AutomationClipboard, AutomationClipboardPoint, AutomationTimeSelection, UIState,
};
use manifold_ui::view::{UiAutomationPointRef, UiGraphTarget, UiSegmentShape};

use crate::content_command::ContentCommand;

fn point_matches_ref(
    point: &AutomationPoint,
    point_ref: &UiAutomationPointRef,
    min: f32,
    max: f32,
) -> bool {
    if point.beat != point_ref.beat {
        return false;
    }
    BitmapSlider::value_to_normalized(point.value, min, max) == point_ref.value_norm
}

fn normalized_value(value: f32, min: f32, max: f32) -> f32 {
    BitmapSlider::value_to_normalized(value, min, max)
}

fn selected_refs(selection: &UIState) -> Vec<UiAutomationPointRef> {
    let mut refs = selection.selected_automation_points.clone();
    if let Some(point) = selection.selected_automation_point.clone()
        && !refs.contains(&point)
    {
        refs.push(point);
    }
    refs
}

/// Set the context-menu destination while retaining a selection that includes it.
/// This is deliberately a projection of UI selection, never a project write.
pub(crate) fn set_lane_context(
    selection: &mut UIState,
    target: &UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
) {
    let before = selection.selected_automation_points.len()
        + usize::from(selection.selected_automation_point.is_some());
    let key = (target.clone(), param_id.clone());
    let target_is_member = selected_refs(selection)
        .iter()
        .any(|point| point.target == *target && point.param_id == *param_id)
        || selection
            .automation_time_selection
            .as_ref()
            .is_some_and(|time| time.lanes.contains(&key));
    if !target_is_member {
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
        selection.automation_time_selection = None;
        selection.automation_insert_beat = None;
    }
    selection.automation_paste_context = Some((target.clone(), param_id.clone()));
    let after = selection.selected_automation_points.len()
        + usize::from(selection.selected_automation_point.is_some());
    if before != after {
        selection.note_automation_selection_change();
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
    let Some(param) = instance.params.get(param_id.as_ref()) else {
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
            value_norm: normalized_value(point.value, param.slider_span().0, param.slider_span().1),
        })
        .collect();
    selection.selected_automation_point = selection.selected_automation_points.first().cloned();
    selection.automation_time_selection = None;
    selection.note_automation_selection_change();
}

fn destination(selection: &UIState) -> Option<(UiGraphTarget, manifold_core::effects::ParamId)> {
    if let Some(range) = &selection.automation_time_selection {
        return selection
            .automation_paste_context
            .as_ref()
            .filter(|context| range.lanes.contains(context))
            .or_else(|| range.lanes.first())
            .cloned();
    }
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

fn interval_shape(lane: &AutomationLane, left: Beats, right: Beats) -> SegmentShape {
    if right <= left {
        return SegmentShape::Hold;
    }
    let midpoint = Beats((left.0 + right.0) * 0.5);
    let upper = lane.points.partition_point(|point| point.beat <= midpoint);
    let index = upper.saturating_sub(1);
    let Some(a) = lane.points.get(index) else {
        return SegmentShape::Hold;
    };
    let Some(b) = lane.points.get(index + 1) else {
        return SegmentShape::Hold;
    };
    if b.beat <= a.beat {
        return SegmentShape::Hold;
    }
    let span = b.beat.0 - a.beat.0;
    let start = ((left.0 - a.beat.0) / span).clamp(0.0, 1.0) as f32;
    let end = ((right.0 - a.beat.0) / span).clamp(0.0, 1.0) as f32;
    a.shape.subrange(start, end)
}

/// Clip one lane to a time range, adding sampled boundaries and restricting
/// every surviving segment shape to the part inside the range.
fn clip_lane_to_range(lane: &AutomationLane, start: Beats, end: Beats) -> Vec<AutomationPoint> {
    if end < start || lane.points.is_empty() {
        return Vec::new();
    }
    let mut points = Vec::new();
    points.push(AutomationPoint {
        beat: start,
        value: lane.value_at(start),
        shape: SegmentShape::Hold,
    });
    points.extend(
        lane.points
            .iter()
            .filter(|point| point.beat > start && point.beat < end)
            .copied(),
    );
    if end > start {
        points.push(AutomationPoint {
            beat: end,
            value: lane.value_at(end),
            shape: SegmentShape::Hold,
        });
    }
    for index in 0..points.len().saturating_sub(1) {
        points[index].shape = interval_shape(lane, points[index].beat, points[index + 1].beat);
    }
    points
}

fn copy_time_selection(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    time: &AutomationTimeSelection,
) -> bool {
    let start = time.start.min(time.end);
    let end = time.start.max(time.end);
    let mut copied = Vec::new();
    for (target, param_id) in &time.lanes {
        let graph_target = graph_target(target);
        let Some(instance) = project.preset_instance(&graph_target) else {
            continue;
        };
        let Some(lane) = instance.automation_lanes.as_ref().and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.param_id == *param_id && lane.enabled)
        }) else {
            continue;
        };
        let Some(param) = instance.params.get(param_id.as_ref()) else {
            continue;
        };
        for point in clip_lane_to_range(lane, start, end) {
            copied.push(AutomationClipboardPoint {
                target: target.clone(),
                param_id: param_id.clone(),
                beat_offset: point.beat - start,
                value_norm: normalized_value(
                    point.value,
                    param.slider_span().0,
                    param.slider_span().1,
                ),
                value: point.value,
                source_min: param.slider_span().0,
                source_max: param.slider_span().1,
                shape: from_core_shape(point.shape),
            });
        }
    }
    if copied.is_empty() {
        return false;
    }
    selection.automation_paste_context = copied
        .first()
        .map(|point| (point.target.clone(), point.param_id.clone()));
    selection.automation_clipboard = Some(AutomationClipboard {
        points: copied,
        span: end - start,
    });
    true
}

/// Copy the currently selected points into the shared UI clipboard.
pub(crate) fn copy_selected(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
) -> bool {
    if let Some(time) = selection.automation_time_selection.clone() {
        let copied = copy_time_selection(project, selection, &time);
        if !copied {
            selection.automation_clipboard = None;
            selection.automation_paste_context = None;
        }
        return copied;
    }
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
        let Some(param) = instance.params.get(point_ref.param_id.as_ref()) else {
            continue;
        };
        let Some(point) = lane
            .points
            .iter()
            .find(|point| {
                point_matches_ref(
                    point,
                    &point_ref,
                    param.slider_span().0,
                    param.slider_span().1,
                )
            })
            .copied()
        else {
            continue;
        };
        let Some(param) = instance.params.get(point_ref.param_id.as_ref()) else {
            continue;
        };
        let range = param.slider_span().1 - param.slider_span().0;
        let value_norm = if range.abs() > f32::EPSILON {
            (point.value - param.slider_span().0) / range
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
            source_min: param.slider_span().0,
            source_max: param.slider_span().1,
            shape: from_core_shape(point.shape),
        });
    }

    if found.is_empty() {
        return false;
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
    true
}

/// Flatten the selected time interval to its entry value in every selected
/// enabled lane, retaining exact curve continuity outside the interval.
fn flatten_time_selection(
    project: &manifold_core::project::Project,
    selection: &UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
) -> bool {
    let Some(time) = selection.automation_time_selection.as_ref() else {
        return false;
    };
    let start = time.start.min(time.end);
    let end = time.start.max(time.end);
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for (ui_target, param_id) in &time.lanes {
        let target = graph_target(ui_target);
        let Some(instance) = project.preset_instance(&target) else {
            continue;
        };
        let Some(lane) = instance.automation_lanes.as_ref().and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.param_id == *param_id && lane.enabled)
        }) else {
            continue;
        };
        if lane.points.is_empty() {
            continue;
        }
        let entry = lane.value_at(start);
        let replacement = if start == end {
            vec![AutomationPoint {
                beat: start,
                value: entry,
                shape: SegmentShape::Hold,
            }]
        } else {
            vec![
                AutomationPoint {
                    beat: start,
                    value: entry,
                    shape: SegmentShape::Hold,
                },
                AutomationPoint {
                    beat: end,
                    value: entry,
                    shape: SegmentShape::Hold,
                },
            ]
        };
        let old_points = lane.points.clone();
        let new_points =
            manifold_playback::automation::punch_recorded_points(&old_points, &replacement);
        let command = CommitRecordedGestureCommand::new(
            target,
            param_id.as_ref(),
            new_points,
            Some(old_points),
        );
        commands.push(Box::new(command));
    }
    if commands.is_empty() {
        return false;
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Flatten Automation Range".to_string()),
    );
    true
}

/// Delete selected points as one undoable batch. Missing targets/points are
/// skipped without changing the project.
pub(crate) fn delete_selected(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
) {
    if selection.automation_time_selection.is_some() {
        flatten_time_selection(project, selection, content_tx);
        selection.automation_insert_beat = selection
            .automation_time_selection
            .as_ref()
            .map(|range| range.start);
        selection.automation_time_selection = None;
        selection.selected_automation_point = None;
        selection.selected_automation_points.clear();
        return;
    }
    use std::collections::HashMap;

    let refs = selected_refs(selection);
    selection.selected_automation_points.clear();
    selection.selected_automation_point = None;
    if refs.is_empty() {
        return;
    }
    let mut by_lane: HashMap<(GraphTarget, String), Vec<UiAutomationPointRef>> = HashMap::new();
    for point in &refs {
        by_lane
            .entry((
                graph_target(&point.target),
                point.param_id.as_ref().to_string(),
            ))
            .or_default()
            .push(point.clone());
    }
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for ((target, param_id), refs) in by_lane {
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
        let Some(param) = inst.params.get(&param_id) else {
            continue;
        };
        let mut indices: Vec<usize> = refs
            .iter()
            .filter_map(|point_ref| {
                lane.points.iter().position(|point| {
                    point_matches_ref(
                        point,
                        point_ref,
                        param.slider_span().0,
                        param.slider_span().1,
                    )
                })
            })
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
        return;
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Delete Automation Points".to_string()),
    );
}

/// New value for a nudged point. A stepped lane (whole numbers or value
/// labels) moves one whole step per press; a continuous lane moves
/// `value_steps` percent of its range. Always clamped into the range.
fn nudged_value(value: f32, value_steps: f32, min: f32, max: f32, whole_numbers: bool) -> f32 {
    if value_steps == 0.0 {
        return value;
    }
    let norm_delta = if whole_numbers {
        value_steps.signum() / (max - min).abs().max(f32::EPSILON)
    } else {
        value_steps * 0.01
    };
    manifold_ui::automation_drag::apply_value(value, norm_delta, min, max, whole_numbers)
}

/// Arrow-key nudge of the selected breakpoints as ONE undo entry. `beat_delta`
/// moves them along the timeline (the whole group stops at beat 0 and at the
/// nearest unselected point together, keeping its shape); `value_steps` is
/// signed percent-of-range steps, 1.0 for a normal press and 0.1 for a fine
/// one. Nothing is ever deleted: landing on an exact twin keeps both points.
/// Returns whether any selected point resolved, so the key is consumed even
/// when a limit leaves nothing to move; a stale selection returns false and
/// the arrow nudges clips instead.
pub(crate) fn nudge_selected(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    beat_delta: Beats,
    value_steps: f32,
) -> bool {
    use manifold_editing::commands::automation::MoveAutomationPointCommand;

    let refs = selected_refs(selection);
    if refs.is_empty() {
        return false;
    }
    struct Resolved {
        point_ref: UiAutomationPointRef,
        old: AutomationPoint,
        min: f32,
        max: f32,
        whole: bool,
        /// Beats to the nearest unselected point on each side of this one on
        /// its lane (infinite when there is none): the nudge stops there.
        room_right: f64,
        room_left: f64,
    }
    let mut resolved: Vec<Resolved> = Vec::with_capacity(refs.len());
    for point_ref in &refs {
        let target = graph_target(&point_ref.target);
        let Some(inst) = project.preset_instance(&target) else {
            continue;
        };
        let Some(param) = inst.params.get(point_ref.param_id.as_ref()) else {
            continue;
        };
        let Some(lane) = inst.automation_lanes.as_ref().and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.param_id.as_ref() == point_ref.param_id.as_ref())
        }) else {
            continue;
        };
        let (min, max) = param.slider_span();
        let Some(index) = lane
            .points
            .iter()
            .position(|p| point_matches_ref(p, point_ref, min, max))
        else {
            continue;
        };
        let old = lane.points[index];
        let (room_left, room_right) = manifold_ui::automation_drag::room_to_unselected(
            index,
            old.beat.0,
            lane.points.iter().enumerate().map(|(other_index, other)| {
                let selected = refs.iter().any(|selected| {
                    selected.target == point_ref.target
                        && selected.param_id == point_ref.param_id
                        && point_matches_ref(other, selected, min, max)
                });
                (other_index, other.beat.0, selected)
            }),
        );
        resolved.push(Resolved {
            point_ref: point_ref.clone(),
            old,
            min,
            max,
            whole: param.whole_numbers(),
            room_right,
            room_left,
        });
    }
    // Nothing resolved: the selection is stale, so the arrow falls through to
    // the clip nudge instead of being swallowed.
    if resolved.is_empty() {
        return false;
    }
    // One shared delta keeps the group's shape. It stops at beat zero and at
    // the nearest unselected point on any lane, so a nudge never crosses (and
    // never overwrites) a point it was not asked to move.
    let min_beat = resolved
        .iter()
        .map(|r| r.old.beat.0)
        .fold(f64::INFINITY, f64::min);
    let room_right = resolved
        .iter()
        .map(|r| r.room_right)
        .fold(f64::INFINITY, f64::min);
    let room_left = resolved
        .iter()
        .map(|r| r.room_left)
        .fold(f64::INFINITY, f64::min);
    let beat_delta = Beats(manifold_ui::automation_drag::clamp_group_beat_delta(
        beat_delta.0,
        min_beat,
        room_left,
        room_right,
    ));

    type LaneMoves = (GraphTarget, String, Vec<(AutomationPoint, AutomationPoint)>);
    let mut lanes: Vec<LaneMoves> = Vec::new();
    let mut moved_refs: Vec<(UiAutomationPointRef, UiAutomationPointRef)> = Vec::new();
    for r in &resolved {
        let new = AutomationPoint {
            beat: r.old.beat + beat_delta,
            value: nudged_value(r.old.value, value_steps, r.min, r.max, r.whole),
            shape: r.old.shape,
        };
        if new.beat == r.old.beat && new.value == r.old.value {
            continue;
        }
        moved_refs.push((
            r.point_ref.clone(),
            UiAutomationPointRef {
                beat: new.beat,
                value_norm: normalized_value(new.value, r.min, r.max),
                ..r.point_ref.clone()
            },
        ));
        let target = graph_target(&r.point_ref.target);
        let param_id = r.point_ref.param_id.as_ref().to_string();
        match lanes
            .iter_mut()
            .find(|(t, p, _)| *t == target && *p == param_id)
        {
            Some((_, _, moves)) => moves.push((r.old, new)),
            None => lanes.push((target, param_id, vec![(r.old, new)])),
        }
    }
    if lanes.is_empty() {
        return true;
    }
    let commands: Vec<Box<dyn Command>> = lanes
        .into_iter()
        .map(|(target, param_id, moves)| {
            Box::new(MoveAutomationPointCommand::for_group(
                target, param_id, moves,
            )) as Box<dyn Command>
        })
        .collect();
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Nudge Automation Points".to_string()),
    );

    let follow = |point: &mut UiAutomationPointRef| {
        if let Some((_, to)) = moved_refs.iter().find(|(from, _)| from == point) {
            *point = to.clone();
        }
    };
    if let Some(single) = selection.selected_automation_point.as_mut() {
        follow(single);
    }
    selection
        .selected_automation_points
        .iter_mut()
        .for_each(follow);
    if beat_delta != Beats::ZERO
        && let Some(range) = selection.automation_time_selection.as_mut()
    {
        range.start = (range.start + beat_delta).max(Beats::ZERO);
        range.end = (range.end + beat_delta).max(Beats::ZERO);
    }
    selection.automation_insert_beat = selection
        .automation_insert_beat
        .map(|beat| beat + beat_delta);
    true
}

pub(crate) fn cut_selected(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
) {
    if selection.automation_time_selection.is_some() {
        if copy_selected(project, selection) {
            flatten_time_selection(project, selection, content_tx);
        }
        selection.automation_insert_beat = selection
            .automation_time_selection
            .as_ref()
            .map(|range| range.start);
        selection.automation_time_selection = None;
        selection.selected_automation_point = None;
        selection.selected_automation_points.clear();
        return;
    }
    let refs = selected_refs(selection);
    if refs.is_empty() {
        return;
    }
    copy_selected(project, selection);
    selection.selected_automation_point = None;
    selection.selected_automation_points = refs;
    delete_selected(project, selection, content_tx);
    if let Some(point) = selection
        .automation_clipboard
        .as_ref()
        .and_then(|clipboard| clipboard.points.first())
    {
        selection.automation_paste_context = Some((point.target.clone(), point.param_id.clone()));
    }
}

pub(crate) fn paste(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    target_beat: Beats,
) {
    let Some(clipboard) = selection.automation_clipboard.clone() else {
        return;
    };
    let destination = destination(selection);
    let single_lane = lane_count(&clipboard) == 1;
    if single_lane && destination.is_none() {
        return;
    }
    let mut source_lanes = Vec::new();
    for point in &clipboard.points {
        let key = (point.target.clone(), point.param_id.clone());
        if !source_lanes.contains(&key) {
            source_lanes.push(key);
        }
    }
    let mut plans = Vec::new();
    for source_key in source_lanes {
        let (ui_target, param_id) = if single_lane {
            destination
                .clone()
                .expect("single-lane paste has a destination")
        } else {
            source_key.clone()
        };
        let target = graph_target(&ui_target);
        let Some(instance) = project.preset_instance(&target) else {
            return;
        };
        let Some(param) = instance.params.get(param_id.as_ref()) else {
            return;
        };
        let (param_min, param_max, whole_numbers) = (
            param.slider_span().0,
            param.slider_span().1,
            param.whole_numbers(),
        );
        let mut phrase: Vec<_> = clipboard
            .points
            .iter()
            .filter(|point| point.target == source_key.0 && point.param_id == source_key.1)
            .map(|point| {
                let same_range = (point.source_min - param_min).abs() <= f32::EPSILON
                    && (point.source_max - param_max).abs() <= f32::EPSILON;
                let mut value = if same_range {
                    point.value
                } else {
                    param_min + point.value_norm * (param_max - param_min)
                };
                value = value.clamp(param_min, param_max);
                if whole_numbers {
                    value = manifold_ui::automation_drag::quantise_whole_number(
                        value, param_min, param_max,
                    );
                }
                AutomationPoint {
                    beat: target_beat + point.beat_offset,
                    value,
                    shape: if whole_numbers {
                        SegmentShape::Hold
                    } else {
                        to_core_shape(point.shape)
                    },
                }
            })
            .collect();
        if phrase.is_empty() {
            return;
        }
        phrase.sort_by(|left, right| left.beat.0.total_cmp(&right.beat.0));
        plans.push((ui_target, param_id, target, param_min, param_max, phrase));
    }

    let mut commands: Vec<Box<dyn Command>> = Vec::with_capacity(plans.len());
    let mut inserted = Vec::new();
    let pasted_lanes: Vec<_> = plans
        .iter()
        .map(|(target, param_id, ..)| (target.clone(), param_id.clone()))
        .collect();
    for (ui_target, param_id, target, _param_min, _param_max, phrase) in plans {
        let old_points = project
            .preset_instance(&target)
            .and_then(|instance| instance.automation_lanes.as_ref())
            .and_then(|lanes| lanes.iter().find(|lane| lane.param_id == param_id))
            .map(|lane| lane.points.clone());
        let new_points = if phrase.first().unwrap().beat == phrase.last().unwrap().beat {
            // A point paste edits the breakpoint and its adjoining segments.
            // Punch guards around a zero-duration phrase would create an
            // inaudible spike instead of a useful new breakpoint.
            let beat = phrase[0].beat;
            let mut points = old_points.clone().unwrap_or_default();
            points.retain(|point| point.beat != beat);
            points.extend_from_slice(&phrase);
            points.sort_by(|left, right| left.beat.0.total_cmp(&right.beat.0));
            points
        } else {
            manifold_playback::automation::punch_recorded_points(
                old_points.as_deref().unwrap_or_default(),
                &phrase,
            )
        };
        let command =
            CommitRecordedGestureCommand::new(target, param_id.as_ref(), new_points, old_points);
        for point in phrase {
            inserted.push(UiAutomationPointRef {
                target: ui_target.clone(),
                param_id: param_id.clone(),
                beat: point.beat,
                value_norm: normalized_value(point.value, _param_min, _param_max),
            });
        }
        commands.push(Box::new(command));
    }
    ContentCommand::send(
        content_tx,
        ContentCommand::ExecuteBatch(commands, "Paste Automation".to_string()),
    );
    selection.selected_automation_point = inserted.first().cloned();
    selection.selected_automation_points = inserted;
    if selection.automation_time_selection.is_some() {
        selection.automation_time_selection = Some(AutomationTimeSelection {
            start: target_beat,
            end: target_beat + clipboard.span,
            lanes: pasted_lanes,
        });
    }
}

pub(crate) fn duplicate_selected(
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    grid_step: f32,
) {
    let (start, end) = if let Some(time) = &selection.automation_time_selection {
        (time.start.min(time.end), time.start.max(time.end))
    } else {
        let refs = selected_refs(selection);
        if refs.is_empty() {
            return;
        }
        let start = refs
            .iter()
            .map(|point| point.beat.0)
            .fold(f64::INFINITY, f64::min);
        let end = refs
            .iter()
            .map(|point| point.beat.0)
            .fold(f64::NEG_INFINITY, f64::max);
        (Beats(start), Beats(end))
    };
    let destination = end
        + if start == end {
            Beats(f64::from(grid_step.max(f32::EPSILON)))
        } else {
            Beats::ZERO
        };
    let saved_clipboard = selection.automation_clipboard.clone();
    let saved_context = selection.automation_paste_context.clone();
    if !copy_selected(project, selection) {
        selection.automation_clipboard = saved_clipboard;
        selection.automation_paste_context = saved_context;
        return;
    }
    paste(project, selection, content_tx, destination);
    selection.automation_clipboard = saved_clipboard;
    selection.automation_paste_context = saved_context;
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
            manifold_ui::automation_drag::quantise_whole_number(raw, min, max)
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
    project: &manifold_core::project::Project,
    selection: &mut UIState,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    target: &UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
    click_beat: Beats,
    shape: AutomationShape,
) {
    set_lane_context(selection, target, param_id);
    let graph_target = graph_target(target);
    let Some(instance) = project.preset_instance(&graph_target) else {
        return;
    };
    let Some(param) = instance.params.get(param_id.as_ref()) else {
        return;
    };
    let (param_min, param_max, whole_numbers) = (
        param.slider_span().0,
        param.slider_span().1,
        param.whole_numbers(),
    );
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
    let (start, end) = selection
        .automation_time_selection
        .as_ref()
        .filter(|range| range.end > range.start)
        .map(|range| (range.start, range.end))
        .unwrap_or_else(|| shape_range(&selected_beats, click_beat, beats_per_bar));
    let points = shape_points(
        lane_points.as_deref(),
        start,
        end,
        param_min,
        param_max,
        whole_numbers,
        shape,
    );
    let old_points = lane_points;
    let command = CommitRecordedGestureCommand::new(
        graph_target,
        param_id.as_ref(),
        points.clone(),
        old_points,
    );
    ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(command)));
    selection.selected_automation_points = points
        .iter()
        .filter(|point| point.beat.0 >= start.0 && point.beat.0 <= end.0)
        .map(|point| UiAutomationPointRef {
            target: target.clone(),
            param_id: param_id.clone(),
            beat: point.beat,
            value_norm: normalized_value(point.value, param_min, param_max),
        })
        .collect();
    selection.selected_automation_point = selection.selected_automation_points.first().cloned();
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

    #[test]
    fn nudged_value_steps_percent_of_range_or_one_whole_step() {
        assert!((nudged_value(0.5, 1.0, 0.0, 1.0, false) - 0.51).abs() < 1e-6);
        assert!((nudged_value(0.5, -0.1, 0.0, 1.0, false) - 0.499).abs() < 1e-6);
        assert_eq!(
            nudged_value(0.995, 1.0, 0.0, 1.0, false),
            1.0,
            "clamped to the range"
        );
        // Stepped lanes move one whole step whatever the fine setting says.
        assert_eq!(nudged_value(2.0, 0.1, 0.0, 5.0, true), 3.0);
        assert_eq!(nudged_value(2.0, -1.0, 0.0, 5.0, true), 1.0);
        assert_eq!(nudged_value(5.0, 1.0, 0.0, 5.0, true), 5.0);
    }

    #[test]
    fn nudge_moves_the_selection_as_one_undo_entry_and_stops_at_beat_zero() {
        use manifold_core::effects::PresetInstance;
        use manifold_core::project::Project;
        use manifold_editing::service::EditingService;

        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let ui_target = UiGraphTarget::Effect(fx.id.clone());
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                point(0.0, 0.2, SegmentShape::Linear),
                point(4.0, 0.6, SegmentShape::Linear),
            ],
        }]);
        let mut project = Project::default();
        project.settings.master_effects.push(fx);
        let before = serde_json::to_value(&project).unwrap();
        let (min, max) = project.settings.master_effects[0]
            .params
            .get("amount")
            .unwrap()
            .slider_span();
        let span = max - min;
        let select = |beat: f64, value: f32| UiAutomationPointRef {
            target: ui_target.clone(),
            param_id: "amount".into(),
            beat: Beats(beat),
            value_norm: normalized_value(value, min, max),
        };
        let mut selection = UIState::new();
        selection.selected_automation_points = vec![select(4.0, 0.6)];
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut service = EditingService::new();

        // Right one grid step, up one percent. One batch is sent, which the
        // content thread runs as one undo entry.
        assert!(nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.25),
            1.0
        ));
        let sent: Vec<_> = rx.try_iter().collect();
        assert_eq!(sent.len(), 1);
        assert!(
            matches!(sent[0], ContentCommand::ExecuteBatch(..)),
            "a nudge is one batch"
        );
        crate::edit_runner::apply_sent(&mut service, &mut project, sent);
        let lane = &project.settings.master_effects[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0];
        assert_eq!(lane.points[1].beat, Beats(4.25));
        let nudged = lane.points[1].value;
        assert!((nudged - (0.6 + 0.01 * span)).abs() < 1e-5);
        assert_eq!(
            selection.selected_automation_points[0].beat,
            Beats(4.25),
            "selection follows the point"
        );

        // One undo entry for the whole keypress.
        let moved = serde_json::to_value(&project).unwrap();
        assert!(service.undo(&mut project));
        assert!(!service.undo(&mut project));
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(service.redo(&mut project));
        assert_eq!(serde_json::to_value(&project).unwrap(), moved);

        // A point at beat 0 pins the group: a left nudge moves nothing and
        // still consumes the key.
        selection.selected_automation_points = vec![select(0.0, 0.2), select(4.25, nudged)];
        let after = serde_json::to_value(&project).unwrap();
        assert!(nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(-0.25),
            0.0
        ));
        assert_eq!(serde_json::to_value(&project).unwrap(), after);
        assert!(rx.try_recv().is_err(), "nothing moved, nothing sent");

        // No selection: the key is not consumed.
        selection.selected_automation_points.clear();
        assert!(!nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.25),
            0.0
        ));
    }

    /// A Mirror effect whose "amount" lane holds `points`, plus the lane's
    /// UI target and slider span.
    fn project_with_amount_lane(
        points: Vec<AutomationPoint>,
    ) -> (manifold_core::project::Project, UiGraphTarget, f32, f32) {
        let mut fx =
            manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let ui_target = UiGraphTarget::Effect(fx.id.clone());
        let (min, max) = fx.params.get("amount").unwrap().slider_span();
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points,
        }]);
        let mut project = manifold_core::project::Project::default();
        project.settings.master_effects.push(fx);
        (project, ui_target, min, max)
    }

    fn lane_points(project: &manifold_core::project::Project) -> Vec<(f64, f32)> {
        project.settings.master_effects[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0]
            .points
            .iter()
            .map(|p| (p.beat.0, p.value))
            .collect()
    }

    #[test]
    fn nudge_with_a_stale_selection_is_not_consumed() {
        let (project, ui_target, min, max) =
            project_with_amount_lane(vec![point(4.0, 0.6, SegmentShape::Linear)]);
        let before = serde_json::to_value(&project).unwrap();
        let mut selection = UIState::new();
        selection.selected_automation_points = vec![UiAutomationPointRef {
            target: ui_target,
            param_id: "amount".into(),
            beat: Beats(99.0),
            value_norm: normalized_value(0.3, min, max),
        }];
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(!nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.25),
            1.0
        ));
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn horizontal_nudge_stops_at_the_nearest_unselected_point() {
        let (mut project, ui_target, min, max) = project_with_amount_lane(vec![
            point(0.0, 0.2, SegmentShape::Linear),
            point(4.0, 0.6, SegmentShape::Linear),
            point(4.1, 0.9, SegmentShape::Linear),
        ]);
        let mut selection = UIState::new();
        selection.selected_automation_points = vec![UiAutomationPointRef {
            target: ui_target,
            param_id: "amount".into(),
            beat: Beats(4.0),
            value_norm: normalized_value(0.6, min, max),
        }];
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut service = manifold_editing::service::EditingService::new();

        assert!(nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.25),
            0.0
        ));
        crate::edit_runner::apply_sent(&mut service, &mut project, rx.try_iter());
        let points = lane_points(&project);
        assert_eq!(points.len(), 3, "a nudge never deletes a point");
        assert!(
            (points[1].0 - 4.1).abs() < 1e-9,
            "stops on the neighbour's beat, got {points:?}"
        );
        assert_eq!(
            selection.selected_automation_points[0].beat,
            Beats(points[1].0)
        );

        // Already against the neighbour: further presses move nothing.
        assert!(nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.25),
            0.0
        ));
        crate::edit_runner::apply_sent(&mut service, &mut project, rx.try_iter());
        assert_eq!(lane_points(&project), points);
    }

    #[test]
    fn value_nudge_onto_an_exact_twin_keeps_both_points() {
        let (mut project, ui_target, lane_min, lane_max) =
            project_with_amount_lane(vec![point(4.0, 0.5, SegmentShape::Linear)]);
        let twin = nudged_value(0.5, 1.0, lane_min, lane_max, false);
        project.settings.master_effects[0]
            .automation_lanes
            .as_mut()
            .unwrap()[0]
            .points
            .push(point(4.0, twin, SegmentShape::Linear));
        let mut selection = UIState::new();
        selection.selected_automation_points = vec![UiAutomationPointRef {
            target: ui_target,
            param_id: "amount".into(),
            beat: Beats(4.0),
            value_norm: normalized_value(0.5, lane_min, lane_max),
        }];
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(nudge_selected(
            &project,
            &mut selection,
            &tx,
            Beats(0.0),
            1.0
        ));
        let mut service = manifold_editing::service::EditingService::new();
        crate::edit_runner::apply_sent(&mut service, &mut project, rx.try_iter());
        let points = lane_points(&project);
        assert_eq!(points.len(), 2, "the twin survives the move: {points:?}");
        assert!(points.iter().all(|p| p.1 == twin));
    }
}
