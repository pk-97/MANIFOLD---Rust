//! CPU proofs for Water-family rename addresses and destructive-action guards.

use super::*;
use manifold_editing::service::EditingService;
use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};

fn add_water(project: &mut Project, layer: &LayerId, render: u32) {
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(
        &ProjectAction::SceneSetupAddFluid(layer.clone(), render),
        project,
        &tx,
        &state,
        &mut ui,
        &mut selection,
        &mut active,
        &mut prefs,
    );
    let ContentCommand::ExecuteSelecting(command, _) = rx.try_recv().expect("Add Water command")
    else { panic!("Add Water must use content-owned editing"); };
    let mut editing = EditingService::new();
    editing.execute(command, project);
    assert_eq!(editing.take_rejection(), None);
}

fn sync_water_ui(project: &Project, layer: &LayerId) -> crate::ui_root::UIRoot {
    let (_, _state, mut ui, mut selection, _active, _prefs) = dispatch_harness();
    ui.scene_setup_panel.open();
    selection.select_layer(layer.clone());
    crate::ui_bridge::projection::inspector::sync_inspector_data(
        &mut ui, project, Some(0), &selection, &[], None,
    );
    ui
}

fn build_scene_tree(ui: &mut crate::ui_root::UIRoot) {
    let rect = manifold_ui::Rect::new(0.0, 0.0, 400.0, 1200.0);
    ui.tree.clear();
    let region = ui.tree.begin_region(
        rect,
        manifold_ui::ZTier::Base,
        "scene_setup",
        manifold_ui::UIFlags::empty(),
    );
    let content_start = ui.tree.count();
    ui.scene_setup_panel.build_docked(&mut ui.tree, rect);
    ui.tree.end_region(region, content_start);
}

fn rows(project: &Project, layer: &LayerId) -> Vec<manifold_renderer::node_graph::scene_vm::SceneObjectKnownRow> {
    let def = effective_def(project, layer);
    SceneVm::from_def(&def)
        .expect("scene VM")
        .objects
        .into_iter()
        .filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(*row),
            SceneObjectVm::Custom { .. } => None,
        })
        .collect()
}

fn rename_scene_object(project: &mut Project, layer: &LayerId, object_node_id: u32, name: &str) {
    let default = effective_def(project, layer);
    let mut editing = EditingService::new();
    editing.execute(
        Box::new(manifold_editing::commands::graph::RenameSceneObjectCommand::new(
            manifold_core::GraphTarget::Generator(layer.clone()),
            Vec::new(),
            object_node_id,
            name.to_string(),
            default,
        )),
        project,
    );
    assert_eq!(editing.take_rejection(), None);
}

#[test]
fn real_water_withholds_child_actions_and_parent_duplicate() {
    let (mut project, layer, render) = super::water_family::water_project();
    add_water(&mut project, &layer, render);
    let family = rows(&project, &layer);
    let water = family
        .iter()
        .find(|row| row.is_group && row.liquid_domain.is_some())
        .expect("Water parent");
    let foam = family
        .iter()
        .find(|row| row.parent_group_id == Some(water.object_node_id))
        .expect("Foam child");
    assert!(foam.look_mesh.is_some());

    let mut ui = sync_water_ui(&project, &layer);
    ui.scene_setup_panel.set_selection(
        layer.clone(),
        manifold_ui::panels::scene_setup_panel::SceneSelection::Object(water.object_node_id),
    );
    build_scene_tree(&mut ui);
    assert!(!tree_has_name(&ui.tree, "scene_setup.properties.duplicate"));
    assert!(tree_has_name(&ui.tree, "scene_setup.properties.remove"));
    assert!(ui.scene_setup_panel.selected_scene_item().unwrap().is_family_parent);
    assert!(ui.scene_setup_panel.remove_selection_action().is_some());
    assert!(!ui.scene_setup_panel.scene_item_edit_allowed(
        manifold_ui::panels::actions::CardEditAction::Duplicate,
    ));
    ui.try_open_dropdown(
        &manifold_ui::PanelAction::Root(manifold_ui::RootAction::SceneItemRightClicked),
        None,
    );
    assert!(!ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Copy")));
    assert!(!ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Cut")));
    assert!(ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Delete")));
    assert!(!ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Duplicate")));

    let toggle = ui.tree
        .nodes()
        .iter()
        .find_map(|node| {
            (ui.tree.name_of(node.id) == Some("scene_setup.objects.group_toggle"))
                .then_some(node.id)
        })
        .expect("Water group toggle");
    ui.scene_setup_panel.handle_event(
        &manifold_ui::UIEvent::Click {
            node_id: toggle,
            pos: manifold_ui::Vec2::ZERO,
            modifiers: manifold_ui::Modifiers::default(),
        },
        &mut ui.tree,
    );
    ui.scene_setup_panel
        .set_selection(layer.clone(), manifold_ui::panels::scene_setup_panel::SceneSelection::Object(foam.object_node_id));
    build_scene_tree(&mut ui);
    assert!(ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Size")),
        "Foam retains its recipe-owned Size row");
    assert!(ui.tree.nodes().iter().any(|node| ui.tree.name_of(node.id)
        .is_some_and(|name| name.ends_with("foam_size.value"))),
        "the flow selector comes from the recipe-owned foam_size binding");
    assert!(!ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some("Cast Shadows")));
    let selected = ui.scene_setup_panel.selected_scene_item().unwrap();
    assert!(selected.is_family_child);
    assert!(!selected.is_family_parent);
    assert!(!tree_has_name(&ui.tree, "scene_setup.properties.duplicate"));
    assert!(!tree_has_name(&ui.tree, "scene_setup.properties.remove"));
    assert!(!ui.scene_setup_panel.scene_item_edit_allowed(
        manifold_ui::panels::actions::CardEditAction::Delete,
    ));
    assert!(!ui.scene_setup_panel.scene_item_edit_allowed(
        manifold_ui::panels::actions::CardEditAction::Duplicate,
    ));
    ui.try_open_dropdown(
        &manifold_ui::PanelAction::Root(manifold_ui::RootAction::SceneItemRightClicked),
        None,
    );
    for label in ["Copy", "Cut", "Delete", "Duplicate"] {
        assert!(!ui.tree.nodes().iter().any(|node| node.text.as_deref() == Some(label)),
            "child context menu must omit {label}");
    }
    assert!(ui.scene_setup_panel.remove_selection_action().is_none());
}

#[test]
fn water_family_clipboard_rejects_every_group_output() {
    let (mut project, layer, render) = super::water_family::water_project();
    add_water(&mut project, &layer, render);
    let family = rows(&project, &layer);
    let family_rows: Vec<_> = family.iter().filter(|row| row.look_mesh.is_some()
        || (row.is_group && row.liquid_domain.is_some())).collect();
    assert_eq!(family_rows.len(), 8, "both complete families");
    for row in family_rows {
        let error = crate::scene_item_transfer::SceneItemClipboard::capture(
            &project,
            &layer,
            render,
            crate::scene_item_transfer::SceneItemKind::Object,
            row.index as u32,
        )
        .expect_err("Water-family group output must not enter the clipboard");
        assert!(error.contains("Groups with several objects cannot be copied"), "unexpected rejection: {error}");
    }
}

fn tree_has_name(tree: &manifold_ui::UITree, name: &str) -> bool {
    tree.nodes().iter().any(|node| tree.name_of(node.id) == Some(name))
}

#[test]
fn water_and_imported_compound_rename_use_object_node_id() {
    let (mut project, layer, render) = super::water_family::water_project();
    add_water(&mut project, &layer, render);
    let water = rows(&project, &layer)
        .into_iter()
        .find(|row| row.is_group && row.liquid_domain.is_some())
        .expect("Water parent");
    let mut ui = sync_water_ui(&project, &layer);
    ui.scene_setup_panel.set_selection(
        layer.clone(),
        manifold_ui::panels::scene_setup_panel::SceneSelection::Object(water.object_node_id),
    );
    build_scene_tree(&mut ui);
    let action = ui.scene_setup_panel.rename_selection_action().expect("Water rename");
    assert!(matches!(action,
        manifold_ui::PanelAction::Root(manifold_ui::RootAction::SceneSetupRenameObjectClicked(_, id, _))
            if id == water.object_node_id));
    rename_scene_object(&mut project, &layer, water.object_node_id, "Renamed Water");
    assert_eq!(
        rows(&project, &layer)
            .into_iter()
            .find(|row| row.object_node_id == water.object_node_id)
            .expect("renamed Water row")
            .name,
        "Renamed Water"
    );

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let default = effective_def(&project, &layer);
    let plan = manifold_renderer::node_graph::gltf_import::assemble_merge_plan(&default, &fixture)
        .expect("import compound fixture");
    let mut editing = EditingService::new();
    editing.execute(
        Box::new(manifold_editing::commands::graph::ImportModelIntoSceneCommand::new(
            manifold_core::GraphTarget::Generator(layer.clone()),
            Vec::new(),
            plan.render_scene_node_id,
            plan.new_nodes,
            plan.new_wires,
            plan.new_objects_count,
            plan.new_card_params,
            plan.new_card_bindings,
            plan.new_string_bindings,
            default,
        )),
        &mut project,
    );
    assert_eq!(editing.take_rejection(), None);
    let imported = rows(&project, &layer)
        .into_iter()
        .find(|row| row.is_group && row.liquid_domain.is_none())
        .expect("imported compound parent");
    let mut ui = sync_water_ui(&project, &layer);
    ui.scene_setup_panel.set_selection(
        layer.clone(),
        manifold_ui::panels::scene_setup_panel::SceneSelection::Object(imported.object_node_id),
    );
    build_scene_tree(&mut ui);
    let action = ui.scene_setup_panel.rename_selection_action().expect("import rename");
    assert!(matches!(action,
        manifold_ui::PanelAction::Root(manifold_ui::RootAction::SceneSetupRenameObjectClicked(_, id, _))
            if id == imported.object_node_id));
    rename_scene_object(&mut project, &layer, imported.object_node_id, "Renamed Import");
    assert_eq!(
        rows(&project, &layer)
            .into_iter()
            .find(|row| row.object_node_id == imported.object_node_id)
            .expect("renamed imported row")
            .name,
        "Renamed Import"
    );
}
