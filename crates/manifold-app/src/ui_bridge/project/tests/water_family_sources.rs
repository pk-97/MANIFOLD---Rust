//! D-94: the Add menu's Inflow goes inside its water, wired into the water's
//! next free role input, lists under the water in the Scene tab tree, undoes
//! in one step and keeps its settings through a save and reload.

use super::*;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, GROUP_TYPE_ID};
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectKnownRow, SceneObjectVm, SceneVm};

fn water_row(def: &EffectGraphDef) -> SceneObjectKnownRow {
    SceneVm::from_def(def)
        .unwrap()
        .objects
        .into_iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) if row.is_group && row.liquid_domain.is_some() => Some(*row),
            _ => None,
        })
        .expect("the preset's water")
}

fn scene_tree_rows(ui: &mut UIRoot) -> Vec<String> {
    let mut tree = manifold_ui::UITree::new();
    let rect = manifold_ui::Rect::new(0.0, 0.0, 400.0, 1200.0);
    let region = tree.begin_region(rect, manifold_ui::ZTier::Base, "scene_setup", manifold_ui::UIFlags::empty());
    let content_start = tree.count();
    ui.scene_setup_panel.build_docked(&mut tree, rect);
    tree.end_region(region, content_start);
    tree.all_nodes()
        .filter(|node| tree.name_of(node.id) == Some("scene_tree.item"))
        .filter_map(|node| node.text.clone())
        .collect()
}

fn sync(ui: &mut UIRoot, project: &Project, selection: &SelectionState) {
    crate::ui_bridge::projection::inspector::sync_inspector_data(ui, project, Some(0), selection, &[], None);
}

/// The level holding the water's domain: the family group's body.
fn domain_level<'a>(def: &'a EffectGraphDef, water: &SceneObjectKnownRow) -> &'a manifold_core::effect_graph_def::GroupDef {
    let family = def.nodes.iter().find(|node| Some(node.id) == water.group_node_id).expect("family group");
    family.group.as_deref().expect("family body")
}

fn source_group<'a>(level: &'a manifold_core::effect_graph_def::GroupDef, name: &str) -> &'a EffectGraphNode {
    level
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_TYPE_ID && node.handle.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("{name} sits inside its water"))
}

#[test]
fn water_family_inflow_inserts_inside_its_water() {
    let (mut project, layer, _) = super::water_family::water_project();
    let before = effective_def(&project, &layer);
    let water = water_row(&before);
    let domain = water.liquid_domain.clone().expect("water domain");
    let level = domain_level(&before, &water);
    let domain_doc = level.nodes.iter().find(|node| node.node_id == domain.node).expect("domain in the family").id;
    let taken: Vec<String> = level.wires.iter().filter(|wire| wire.to_node == domain_doc).map(|wire| wire.to_port.clone()).collect();
    let free = (0..manifold_core::fluid_domain::MAX_FLUID_ROLES)
        .map(|slot| format!("role_{slot}"))
        .find(|port| !taken.contains(port))
        .expect("a free role input");

    // The water parent is selected by default, so the menu's Inflow targets it.
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    selection.select_layer(layer.clone());
    selection.pin_scope(manifold_ui::InspectorTab::Scene);
    sync(&mut ui, &project, &selection);
    let menu = ui.scene_setup_panel.add_menu(manifold_ui::panels::scene_setup_panel::SceneGroup::Objects);
    let inflow = menu.iter().find(|entry| entry.label == "Inflow").expect("Inflow in the Add menu");
    assert_eq!(inflow.disabled, None);
    let action = match inflow.action.clone() {
        Some(manifold_ui::PanelAction::Project(action)) => action,
        other => panic!("Inflow inserts a template: {other:?}"),
    };
    assert!(matches!(&action, ProjectAction::SceneSetupInsertTemplate { water: Some(node), .. } if *node == domain.node));

    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(&action, &mut crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &tx), &tx, &state,
        &mut ui, &mut selection, &mut active, &mut prefs, Default::default());
    assert_eq!(effective_def(&project, &layer), before, "UI only queues content edits");
    let Ok(ContentCommand::ExecuteOnContent(command)) = rx.try_recv() else { panic!("an Inflow inserts on content") };
    assert!(rx.is_empty());
    let mut editing = EditingService::new();
    editing.execute(command, &mut project);
    assert_eq!(editing.take_rejection(), None);

    let added = effective_def(&project, &layer);
    let level = domain_level(&added, &water);
    let group = source_group(level, "Inflow 1");
    assert!(
        level.wires.iter().any(|wire| wire.from_node == group.id && wire.from_port == "role" && wire.to_node == domain_doc && wire.to_port == free),
        "the Inflow feeds the water's first free role input, {free}"
    );
    let row = water_row(&added);
    let [placed] = row.placed_sources.as_slice() else { panic!("one placed source: {:?}", row.placed_sources) };
    assert_eq!(placed.name, "Inflow 1");
    assert!(!row.fluid_controls.iter().any(|node| placed.owned.contains(node)), "the Inflow's controls are its own row's");

    sync(&mut ui, &project, &selection);
    let rows = scene_tree_rows(&mut ui);
    let at = rows.iter().position(|name| name == &water.name).expect("the water row");
    assert_eq!(rows.get(at + 1).map(String::as_str), Some("Inflow 1"), "the Inflow lists under its water: {rows:?}");

    assert!(editing.undo(&mut project));
    assert_eq!(effective_def(&project, &layer), before, "one undo takes the Inflow out");
    assert!(editing.redo(&mut project));
    assert_eq!(effective_def(&project, &layer), added, "redo restores the same ids");

    // Move the Inflow up through its card and reload: the setting holds.
    let transform = group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.transform_3d").expect("the Inflow's transform");
    let scope = vec![water.group_node_id.expect("family group"), group.id];
    let write = apply_scene_param_write(&project, &layer, scope, transform.id, "pos_y", 2.5).expect("the Inflow's height is a card");
    editing.execute(write, &mut project);
    assert_eq!(editing.take_rejection(), None);
    let moved = effective_def(&project, &layer);
    let card = moved.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| matches!(&binding.target,
        BindingTarget::Node { node_id, param } if node_id == &transform.node_id && param == "pos_y")).expect("pos_y card").id.clone();
    let height = |project: &Project| project.timeline.find_layer_by_id(&layer).unwrap().1.gen_params().unwrap().get_base_param(&card);
    assert_eq!(height(&project), 2.5);
    let saved = serde_json::to_string(&project).unwrap();
    let reloaded = manifold_io::loader::load_project_from_json(&saved).expect("real project load pipeline");
    assert_eq!(effective_def(&reloaded, &layer), moved, "the Inflow's nodes, wires and cards survive a reload");
    assert_eq!(height(&reloaded), 2.5);
}
