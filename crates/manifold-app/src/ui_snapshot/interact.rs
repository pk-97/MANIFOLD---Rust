//! Drive interaction through the REAL input host, then the caller re-syncs and
//! re-renders. A `select:<layer>` resolves the target header's hit rect from the
//! built tree, synthesizes a real pointer Down+Up (`UIRoot::pointer_event` →
//! `UIInputSystem::process_pointer`), drains the real `UIEvent`s, dispatches them
//! through the real `LayerHeaderPanel` (`LayerHeaderPanel::handle_event` →
//! `PanelAction::LayerClicked`), and applies the resulting selection via the same
//! `UIState::select_layer` the app bridge calls. No faked state.
//! See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_core::{ClipId, LayerId};
use manifold_ui::{PanelAction, PointerAction, UIState, Vec2};

use super::fixtures::SceneData;
use crate::ui_root::UIRoot;

/// Apply one interaction spec. Specs may be chained with `;` — each part is
/// applied in sequence against the same `data`/`ui` (e.g. a click then a
/// shift-click to build a multi-clip selection in one `--interact` string;
/// the CLI only threads one `--interact` value per process, so chaining
/// within the string is how a multi-gesture "before" scene gets built).
/// Mutates `data`'s selection/project so the caller can re-sync + re-render.
/// Returns a description of what happened (for stdout evidence).
pub fn apply(ui: &mut UIRoot, data: &mut SceneData, spec: &str) -> String {
    if spec.contains(';') {
        return spec
            .split(';')
            .map(|part| apply_one(ui, data, part.trim()))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    apply_one(ui, data, spec)
}

fn apply_one(ui: &mut UIRoot, data: &mut SceneData, spec: &str) -> String {
    match spec.split_once(':') {
        Some(("select", target)) => select_layer(ui, data, target),
        Some(("collapse", target)) => collapse_layer(data, target),
        Some(("delete", target)) => delete_layer(data, target),
        Some(("open", "settings")) => {
            ui.settings_popup.open();
            "open -> settings popup".to_string()
        }
        Some(("automation_add", rest)) => automation_add_point(data, rest),
        Some(("automation_move", rest)) => automation_move_point(data, rest),
        Some(("automation_bend", rest)) => automation_bend_segment(data, rest),
        Some(("automation_segment_drag", rest)) => automation_segment_drag(data, rest),
        Some(("automation_group_move", rest)) => automation_group_move(data, rest),
        Some(("automation_group_delete", rest)) => automation_group_delete(data, rest),
        Some(("click_clip", rest)) => click_clip(data, rest),
        Some(("shift_click_clip", rest)) => shift_click_clip(data, rest),
        Some(("cmd_click_clip", rest)) => cmd_click_clip(data, rest),
        Some(("cmd_d", _)) => duplicate_selected_clips(data),
        Some((verb, _)) => format!(
            "unknown interact verb '{verb}' (known: select, collapse, delete, open, \
             automation_add, automation_move, automation_bend, automation_segment_drag, \
             automation_group_move, automation_group_delete, click_clip, shift_click_clip, \
             cmd_click_clip, cmd_d)"
        ),
        None if spec == "cmd_d" => duplicate_selected_clips(data),
        None => format!("malformed interact '{spec}' (want verb:target)"),
    }
}

/// P1.0 evidence-gathering verb (`docs/TIMELINE_INTERACTION_P1_SPEC.md` P1.0):
/// `click_clip:<clip_id>:<layer_id>` — a plain click on a clip, driven through
/// `UIState::select_clip` exactly as `ui_bridge/editing.rs`'s
/// `PanelAction::ClipClicked` plain-click arm does (`editing.rs:60`). Selects
/// one clip and clears any region — the anchor for a subsequent
/// `shift_click_clip`/`cmd_click_clip` in a chained spec.
fn click_clip(data: &mut SceneData, rest: &str) -> String {
    let Some((clip_id, layer_id)) = rest.split_once(':') else {
        return format!("click_clip: want clip_id:layer_id, got '{rest}'");
    };
    data.selection
        .select_clip(ClipId::new(clip_id), LayerId::new(layer_id));
    format!("click_clip -> '{clip_id}' on '{layer_id}' (selection cleared, this clip selected)")
}

/// `shift_click_clip:<clip_id>:<layer_id>` drives the clip-click shift path —
/// P1.3b's D2 fix (`docs/TIMELINE_INTERACTION_P1_SPEC.md`):
/// `crate::ui_bridge::select_clip_range_to_with_project`, the same function
/// `ui_bridge/editing.rs`'s `PanelAction::ClipClicked` shift arm now calls.
/// Before this phase this verb drove `select_region_to_with_project`
/// (a REGION) — S1/S3's root; it now drives the clip-RANGE selection D2
/// mandates, so the after-PNG shows the fixed chrome (per-clip highlight
/// across the range, no region band).
fn shift_click_clip(data: &mut SceneData, rest: &str) -> String {
    // `layer_id` isn't used — the clip-range gesture looks up the anchor's
    // and target's own layer/position internally; kept in the spec only for
    // symmetry with `click_clip`/`cmd_click_clip`.
    let Some((clip_id, _layer_id)) = rest.split_once(':') else {
        return format!("shift_click_clip: want clip_id:layer_id, got '{rest}'");
    };
    let cid = ClipId::new(clip_id);
    crate::ui_bridge::select_clip_range_to_with_project(&cid, &mut data.selection, &data.project);
    format!(
        "shift_click_clip -> range extended to '{clip_id}' \
         (selected_clip_ids={:?}, has_region={})",
        data.selection.get_selected_clip_ids(),
        data.selection.has_region(),
    )
}

/// Cmd/ctrl-click path — mirrors `ui_bridge/editing.rs`'s `PanelAction::
/// ClipClicked` cmd arm. Post-D1 this is a pure `toggle_clip_selection`: no
/// region is synthesised from the clip set (the old
/// `update_region_from_clip_selection_inline` sync is deleted), so a multi-clip
/// selection produces a `Clips` selection with no region band — the S1 collapse
/// this verb now exercises.
fn cmd_click_clip(data: &mut SceneData, rest: &str) -> String {
    let Some((clip_id, layer_id)) = rest.split_once(':') else {
        return format!("cmd_click_clip: want clip_id:layer_id, got '{rest}'");
    };
    data.selection
        .toggle_clip_selection(ClipId::new(clip_id), LayerId::new(layer_id));
    format!(
        "cmd_click_clip -> toggled '{clip_id}'; selected_clip_ids={:?} has_region={}",
        data.selection.get_selected_clip_ids(),
        data.selection.has_region(),
    )
}

/// P1.0 evidence-gathering verb: `cmd_d` (no argument) reproduces today's
/// Cmd+D path — `input_host.rs:572-604`'s `duplicate_clips` — using whatever
/// selection/region state a preceding chained verb left in `data.selection`.
/// Calls the same `EditingService::duplicate_clips` + `ui_translate::
/// selection_region_to_core` the real path calls, executes the resulting
/// commands directly on `data.project` (mirrors `input_host.rs:594-597`'s
/// local execute-for-readback), and reports the id churn so the PNG's
/// "before"/"after" clip count and the console's id list agree. S3's root
/// (D3): if a preceding `shift_click_clip` left `region.is_active` true, this
/// takes the REGION-duration offset branch even though 4 *specific* clips
/// were the intent — the "gap after the originals" symptom.
fn duplicate_selected_clips(data: &mut SceneData) -> String {
    let clip_ids = data.selection.get_selected_clip_ids();
    let region = data.selection.current_region().cloned().unwrap_or_default();
    let used_region_mode = region.is_active;
    let region_core = crate::ui_translate::selection_region_to_core(&region);
    let spb = 60.0 / data.project.settings.bpm.0.max(1.0);

    let before_ids: std::collections::HashSet<ClipId> = data
        .project
        .timeline
        .layers
        .iter()
        .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
        .collect();

    let mut commands = manifold_editing::service::EditingService::duplicate_clips(
        &data.project,
        &clip_ids,
        &region_core,
        spb,
    );
    if commands.is_empty() {
        return format!(
            "cmd_d: no-op — 0 commands (selected_clip_ids={clip_ids:?}, region.is_active={used_region_mode})"
        );
    }
    for c in commands.iter_mut() {
        c.execute(&mut data.project);
    }

    let new_ids: Vec<ClipId> = data
        .project
        .timeline
        .layers
        .iter()
        .flat_map(|l| l.clips.iter().filter(|c| !before_ids.contains(&c.id)).map(|c| c.id.clone()))
        .collect();

    data.selection.select_clips(new_ids.clone());

    format!(
        "cmd_d -> mode={} ({} commands) before_ids={} new_ids={new_ids:?}",
        if used_region_mode { "REGION" } else { "individual" },
        commands.len(),
        before_ids.len(),
    )
}

/// P4 Unit A evidence-gathering verb (`docs/AUTOMATION_LANES_DESIGN.md` §7):
/// `automation_add:<layer_id>:<param_id>:<beat>` adds a breakpoint at
/// `beat` with value = the param range's midpoint, directly on `data.
/// project` — same "drive the data field directly, the level under test is
/// render reaction not input dispatch" convention `collapse_layer` already
/// establishes. `AddAutomationPointCommand`'s data shape and the click-add
/// path's denormalize math (`InteractionOverlay::handle_automation_click`)
/// are both covered by unit tests instead (`manifold-editing`'s
/// `commands::automation::tests`, `manifold-ui`'s
/// `automation_hit_tester::tests` + `view::automation_lane_tests`); this verb
/// proves the RENDER reacts to a point that lands on an existing lane.
fn automation_add_point(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    let [layer_id, param_id, beat_str] = parts.as_slice() else {
        return format!("automation_add: want layer:param:beat, got '{rest}'");
    };
    let Ok(beat) = beat_str.parse::<f64>() else {
        return format!("automation_add: bad beat '{beat_str}'");
    };
    let Some((min, max)) = lane_param_range(data, layer_id, param_id) else {
        return format!("automation_add: no lane for '{layer_id}':'{param_id}'");
    };
    let value = (min + max) * 0.5;
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_add: no lane for '{layer_id}':'{param_id}' (2nd lookup)");
    };
    lane.points.push(manifold_core::effects::AutomationPoint {
        beat: manifold_core::Beats(beat),
        value,
        shape: manifold_core::effects::SegmentShape::Linear,
    });
    lane.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
    format!("automation_add -> '{layer_id}':'{param_id}' beat={beat} value={value:.3} ({} points now)", lane.points.len())
}

/// P4 Unit A evidence-gathering verb: `automation_move:<layer_id>:<param_id>:
/// <point_index>:<new_beat>` moves an existing point's beat directly,
/// mirroring `set_automation_point_preview`'s live-drag mutation.
fn automation_move_point(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, idx_str, new_beat_str] = parts.as_slice() else {
        return format!("automation_move: want layer:param:index:new_beat, got '{rest}'");
    };
    let Ok(idx) = idx_str.parse::<usize>() else {
        return format!("automation_move: bad index '{idx_str}'");
    };
    let Ok(new_beat) = new_beat_str.parse::<f64>() else {
        return format!("automation_move: bad beat '{new_beat_str}'");
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_move: no lane for '{layer_id}':'{param_id}'");
    };
    let Some(point) = lane.points.get_mut(idx) else {
        return format!("automation_move: no point at index {idx} ({} points)", lane.points.len());
    };
    let old_beat = point.beat.0;
    point.beat = manifold_core::Beats(new_beat);
    lane.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
    format!("automation_move -> '{layer_id}':'{param_id}'[{idx}] beat {old_beat} -> {new_beat}")
}

/// P4 Unit B evidence-gathering verb (`docs/AUTOMATION_LANES_DESIGN.md` §7):
/// `automation_bend:<layer_id>:<param_id>:<point_index>:<bend>` sets the
/// point at `point_index`'s shape to `Curved(bend)`, directly on `data.
/// project` — same "drive the data field, prove the RENDER reacts" level as
/// `automation_move_point`. Mirrors `InteractionOverlay::
/// commit_automation_segment_bend`'s end state (beat/value unchanged, only
/// `shape` differs); the drag math itself (pixel delta -> bend, Alt-gating,
/// whole_numbers gate) is unit-tested separately
/// (`interaction_overlay.rs`/`automation_hit_tester.rs`).
fn automation_bend_segment(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, idx_str, bend_str] = parts.as_slice() else {
        return format!("automation_bend: want layer:param:index:bend, got '{rest}'");
    };
    let Ok(idx) = idx_str.parse::<usize>() else {
        return format!("automation_bend: bad index '{idx_str}'");
    };
    let Ok(bend) = bend_str.parse::<f32>() else {
        return format!("automation_bend: bad bend '{bend_str}'");
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_bend: no lane for '{layer_id}':'{param_id}'");
    };
    let Some(point) = lane.points.get_mut(idx) else {
        return format!("automation_bend: no point at index {idx} ({} points)", lane.points.len());
    };
    point.shape = manifold_core::effects::SegmentShape::Curved(bend);
    format!("automation_bend -> '{layer_id}':'{param_id}'[{idx}] shape=Curved({bend})")
}

/// P4 Unit B evidence-gathering verb: `automation_segment_drag:<layer_id>:
/// <param_id>:<left_index>:<right_index>:<value_delta>` moves both segment
/// endpoints' VALUES by `value_delta` (param range, unclamped — caller picks
/// safe numbers), directly on `data.project`. Mirrors `InteractionOverlay::
/// commit_automation_segment_value_drag`'s end state (beats + shapes
/// unchanged, only the two values shift by the same delta).
fn automation_segment_drag(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.split(':').collect();
    let [layer_id, param_id, left_str, right_str, delta_str] = parts.as_slice() else {
        return format!(
            "automation_segment_drag: want layer:param:left_index:right_index:delta, got '{rest}'"
        );
    };
    let Ok(left_idx) = left_str.parse::<usize>() else {
        return format!("automation_segment_drag: bad left_index '{left_str}'");
    };
    let Ok(right_idx) = right_str.parse::<usize>() else {
        return format!("automation_segment_drag: bad right_index '{right_str}'");
    };
    let Ok(delta) = delta_str.parse::<f32>() else {
        return format!("automation_segment_drag: bad delta '{delta_str}'");
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_segment_drag: no lane for '{layer_id}':'{param_id}'");
    };
    let len = lane.points.len();
    if left_idx >= len || right_idx >= len {
        return format!("automation_segment_drag: index out of range ({left_idx},{right_idx}) of {len} points");
    }
    lane.points[left_idx].value += delta;
    lane.points[right_idx].value += delta;
    format!(
        "automation_segment_drag -> '{layer_id}':'{param_id}' [{left_idx},{right_idx}] both += {delta}"
    )
}

/// P4 Unit B evidence-gathering verb: `automation_group_move:<layer_id>:
/// <param_id>:<comma_separated_indices>:<value_delta>` moves every listed
/// point's value by `value_delta` (unclamped), directly on `data.project` —
/// mirrors `InteractionOverlay::commit_automation_group_drag`'s end state
/// (a marquee-selected GROUP moved together, beats/shapes unchanged). The
/// grab/marquee-rect math itself is unit-tested separately
/// (`automation_hit_tester::dots_in_rect`).
fn automation_group_move(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, indices_str, delta_str] = parts.as_slice() else {
        return format!("automation_group_move: want layer:param:indices:delta, got '{rest}'");
    };
    let Ok(delta) = delta_str.parse::<f32>() else {
        return format!("automation_group_move: bad delta '{delta_str}'");
    };
    let mut indices = Vec::new();
    for s in indices_str.split(',') {
        match s.parse::<usize>() {
            Ok(i) => indices.push(i),
            Err(_) => return format!("automation_group_move: bad index '{s}' in '{indices_str}'"),
        }
    }
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_group_move: no lane for '{layer_id}':'{param_id}'");
    };
    let len = lane.points.len();
    for &idx in &indices {
        if idx >= len {
            return format!("automation_group_move: index {idx} out of range ({len} points)");
        }
    }
    for &idx in &indices {
        lane.points[idx].value += delta;
    }
    format!("automation_group_move -> '{layer_id}':'{param_id}' {indices:?} all += {delta}")
}

/// P4 Unit B evidence-gathering verb: `automation_group_delete:<layer_id>:
/// <param_id>:<comma_separated_indices>` removes every listed point,
/// highest-index-first — mirrors `AppInputHost::delete_selected_automation_points`'s
/// per-lane ordering (proven generically by `manifold-editing`'s
/// `composite_group_delete_highest_index_first_survives_execute_and_undo`).
fn automation_group_delete(data: &mut SceneData, rest: &str) -> String {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    let [layer_id, param_id, indices_str] = parts.as_slice() else {
        return format!("automation_group_delete: want layer:param:indices, got '{rest}'");
    };
    let mut indices = Vec::new();
    for s in indices_str.split(',') {
        match s.parse::<usize>() {
            Ok(i) => indices.push(i),
            Err(_) => return format!("automation_group_delete: bad index '{s}' in '{indices_str}'"),
        }
    }
    indices.sort_unstable_by(|a, b| b.cmp(a)); // highest first
    indices.dedup();
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return format!("automation_group_delete: no lane for '{layer_id}':'{param_id}'");
    };
    let before = lane.points.len();
    for idx in &indices {
        if *idx < lane.points.len() {
            lane.points.remove(*idx);
        }
    }
    format!(
        "automation_group_delete -> '{layer_id}':'{param_id}' removed {indices:?}; {before} -> {} points",
        lane.points.len()
    )
}

/// Find the automation lane for `param_id` among `layer_id`'s effects (checked
/// first, matching the design's effect-then-gen-params walk order).
fn find_lane_mut<'a>(
    data: &'a mut SceneData,
    layer_id: &str,
    param_id: &str,
) -> Option<&'a mut manifold_core::effects::AutomationLane> {
    let layer = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == layer_id)?;
    if let Some(effects) = layer.effects.as_mut() {
        for fx in effects.iter_mut() {
            if let Some(lanes) = fx.automation_lanes.as_mut()
                && let Some(pos) = lanes.iter().position(|l| l.param_id.as_ref() == param_id)
            {
                return lanes.get_mut(pos);
            }
        }
    }
    None
}

/// Resolved `(min, max)` param range for the lane `find_lane_mut` would
/// return — a separate read-only pass (registry lookup needs `&PresetDef`,
/// awkward to thread through the `&mut` walk above) so `automation_add_point`
/// can compute a midpoint value before mutating.
fn lane_param_range(data: &SceneData, layer_id: &str, param_id: &str) -> Option<(f32, f32)> {
    let layer = data.project.timeline.layers.iter().find(|l| l.layer_id == layer_id)?;
    let effects = layer.effects.as_ref()?;
    for fx in effects {
        let Some(def) = manifold_core::preset_definition_registry::try_get(fx.effect_type()) else {
            continue;
        };
        if let Some(resolved) = manifold_core::effects::resolve_param_in(&def, fx, param_id) {
            return Some((resolved.min, resolved.max));
        }
    }
    None
}

/// P0.0 evidence-gathering verb: flip `is_collapsed` directly on the target
/// layer's `Project` data (same field `states_scene` sets directly — see
/// `fixtures.rs`), NOT via a synthesized chevron click. The bug under
/// investigation (`docs/TIMELINE_LAYOUT_P0_SPEC.md` RC1-RC3) lives in the
/// render/sync path's reaction to the resulting state, not in input dispatch,
/// so driving the data field directly is the right level for this phase.
fn collapse_layer(data: &mut SceneData, target: &str) -> String {
    let Some(layer) = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == target)
    else {
        return format!("collapse: no layer with id '{target}'");
    };
    layer.is_collapsed = !layer.is_collapsed;
    format!("collapse -> layer '{target}' is_collapsed={}", layer.is_collapsed)
}

/// P0.0 evidence-gathering verb: remove the target layer (and any children
/// parented to it) from the `Project`, mirroring what `EditingService`'s
/// delete command achieves at the data level. No synthesized click/menu.
fn delete_layer(data: &mut SceneData, target: &str) -> String {
    let before = data.project.timeline.layers.len();
    data.project
        .timeline
        .layers
        .retain(|l| l.layer_id != target && !l.parent_layer_id.as_ref().is_some_and(|pid| *pid == target));
    let after = data.project.timeline.layers.len();
    if before == after {
        return format!("delete: no layer with id '{target}' (or children) found");
    }
    format!("delete -> removed '{target}' and any children; {before} -> {after} layers")
}

fn select_layer(ui: &mut UIRoot, data: &mut SceneData, target: &str) -> String {
    let Some(idx) = data
        .project
        .timeline
        .layers
        .iter()
        .position(|l| l.layer_id == target)
    else {
        return format!("select: no layer with id '{target}'");
    };

    // Real hit position: the centre of the target header's name node.
    let node = ui.layer_headers.name_node_id(idx);
    let rect = ui.layer_headers.get_node_bounds(&ui.tree, node);
    let pos = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);

    // Drive a real click through the production input path.
    ui.pointer_event(pos, PointerAction::Down, 1.0);
    ui.pointer_event(pos, PointerAction::Up, 1.0);

    // Drain the real events and dispatch through the real panel.
    let events = ui.input.drain_events();
    let mut clicked = None;
    for ev in &events {
        for action in ui.layer_headers.handle_event(ev, &ui.tree) {
            if let PanelAction::LayerClicked(i, _mods) = action {
                clicked = Some(i);
            }
        }
    }
    let hit = clicked.is_some();
    if !hit {
        eprintln!(
            "ui-snap: WARNING — synthesized click missed the '{target}' header; the real input \
             path was NOT exercised. Falling back to id match so the render still updates."
        );
    }
    let i = clicked.unwrap_or(idx);

    // Apply selection exactly as the bridge does on LayerClicked.
    let lid = data.project.timeline.layers[i].layer_id.clone();
    let name = data.project.timeline.layers[i].name.clone();
    let mut sel = UIState::default();
    sel.select_layer(lid);
    data.selection = sel;
    data.active = Some(i);

    format!(
        "select -> layer {i} '{name}' (click {}, {} event(s) dispatched)",
        if hit { "hit the header" } else { "missed; fell back to id match" },
        events.len(),
    )
}
