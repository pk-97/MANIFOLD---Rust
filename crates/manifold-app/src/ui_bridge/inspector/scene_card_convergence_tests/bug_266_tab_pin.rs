//! BUG-266 (inspector tab pin): the pin was clearing on any `selection_version`
//! bump, including ones from command side effects (add-effect's
//! behind-the-scenes selection touch) that never change WHICH thing is
//! selected. Three probes on the real path (`state_sync::
//! sync_inspector_data`, the same fn the app's per-frame sync calls):
//! a version bump with unchanged selection identity must not clear the
//! pin; a genuine identity change must; a transient empty selection
//! (clear-then-reselect churn) must not.

use super::*;
use manifold_ui::InspectorTab;

fn active_tab(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) -> InspectorTab {
    crate::ui_bridge::state_sync::sync_inspector_data(
        ui,
        project,
        active_layer,
        selection,
        &[],
        None,
    );
    ui.inspector.active_tab()
}

#[test]
fn incidental_version_bump_does_not_clear_the_pin() {
    let (project, layer_id) = scene_layer_project();
    let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
    let mut ui = UIRoot::new();
    let mut selection = SelectionState::new();
    selection.select_layer(layer_id.clone());
    selection.pin_scope(InspectorTab::Master);
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx), &selection),
        InspectorTab::Master
    );

    // What add-effect's behind-the-scenes selection touch looks like
    // at the ui_state level: re-selecting the SAME layer bumps
    // `selection_version` without changing WHICH layer is selected.
    let before = selection.selection_visual_version();
    selection.select_layer(layer_id.clone());
    assert!(
        selection.selection_visual_version() > before,
        "sanity: version must actually bump"
    );
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx), &selection),
        InspectorTab::Master,
        "pin must survive a version bump that doesn't change WHICH layer is selected"
    );
}

#[test]
fn genuine_selection_change_clears_the_pin() {
    let (mut project, layer_id) = scene_layer_project();
    let idx2 = project.timeline.add_layer(
        "Scene2",
        LayerType::Generator,
        PresetTypeId::from_string("Scene".to_string()),
    );
    let layer_id_2 = project.timeline.layers[idx2].layer_id.clone();
    let idx1 = project.timeline.find_layer_index_by_id(&layer_id).unwrap();

    let mut ui = UIRoot::new();
    let mut selection = SelectionState::new();
    selection.select_layer(layer_id.clone());
    selection.pin_scope(InspectorTab::Master);
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx1), &selection),
        InspectorTab::Master
    );

    selection.select_layer(layer_id_2.clone());
    let idx2 = project
        .timeline
        .find_layer_index_by_id(&layer_id_2)
        .unwrap();
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx2), &selection),
        InspectorTab::Layer,
        "a genuine selection change (different layer) must drop the pin back to \
         the selection-derived default"
    );
}

#[test]
fn transient_empty_selection_holds_the_pin() {
    let (project, layer_id) = scene_layer_project();
    let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
    let mut ui = UIRoot::new();
    let mut selection = SelectionState::new();
    selection.select_layer(layer_id.clone());
    selection.pin_scope(InspectorTab::Master);
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx), &selection),
        InspectorTab::Master
    );

    // Clear-then-reselect churn: an empty selection observed
    // mid-gesture must not itself kill the pin.
    selection.clear_layer_selection();
    assert_eq!(
        active_tab(&mut ui, &project, None, &selection),
        InspectorTab::Master,
        "a transient empty selection must not clear the pin"
    );

    // ...and reselecting the SAME layer afterward still finds it pinned.
    selection.select_layer(layer_id.clone());
    assert_eq!(
        active_tab(&mut ui, &project, Some(idx), &selection),
        InspectorTab::Master
    );
}
