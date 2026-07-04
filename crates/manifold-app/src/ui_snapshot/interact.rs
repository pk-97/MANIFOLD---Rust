//! Drive interaction through the REAL input host, then the caller re-syncs and
//! re-renders. A `select:<layer>` resolves the target header's hit rect from the
//! built tree, synthesizes a real pointer Down+Up (`UIRoot::pointer_event` →
//! `UIInputSystem::process_pointer`), drains the real `UIEvent`s, dispatches them
//! through the real `LayerHeaderPanel` (`LayerHeaderPanel::handle_event` →
//! `PanelAction::LayerClicked`), and applies the resulting selection via the same
//! `UIState::select_layer` the app bridge calls. No faked state.
//! See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_ui::{PanelAction, PointerAction, UIState, Vec2};

use super::fixtures::SceneData;
use crate::ui_root::UIRoot;

/// Apply one interaction spec (`"select:<layer-id>"`). Mutates `data`'s
/// selection so the caller can re-sync + re-render. Returns a description of
/// what happened (for stdout evidence).
pub fn apply(ui: &mut UIRoot, data: &mut SceneData, spec: &str) -> String {
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
        Some((verb, _)) => format!(
            "unknown interact verb '{verb}' (known: select, collapse, delete, open, \
             automation_add, automation_move, automation_bend, automation_segment_drag)"
        ),
        None => format!("malformed interact '{spec}' (want verb:target)"),
    }
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
