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
        Some((verb, _)) => {
            format!("unknown interact verb '{verb}' (known: select, collapse, delete, open)")
        }
        None => format!("malformed interact '{spec}' (want verb:target)"),
    }
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
