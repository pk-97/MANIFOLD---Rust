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
    let Ok(ContentCommand::ExecuteSelecting(command, select)) = rx.try_recv() else { panic!("an Inflow inserts on content") };
    assert!(matches!(select, crate::edit_selection::SelectAfterEdit::NewSource(_)), "the new source is selected");
    assert!(rx.is_empty());
    let pending = select.capture(&project);
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
    // Beside the water's Initial Fill.
    let [fill, placed] = row.placed_sources.as_slice() else { panic!("the fill and the Inflow: {:?}", row.placed_sources) };
    assert_eq!(fill.role, manifold_foundation::scene_surface::SourceRole::InitialFill);
    assert_eq!(placed.name, "Inflow 1");
    assert!(!row.control_nodes().iter().any(|node| placed.owned.contains(node)), "the Inflow's controls are its own row's");

    // Placed at the top centre of the water's simulated box.
    let box_ = row.fluid_domain.expect("the water's box");
    let inflow_transform = group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.transform_3d").unwrap();
    let at = |name: &str| match inflow_transform.params.get(name) {
        Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => *value,
        other => panic!("{name}: {other:?}"),
    };
    for (axis, name) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
        let value = at(name);
        assert!(value > box_.min[axis] && value < box_.min[axis] + box_.size[axis], "{name} {value} inside {box_:?}");
    }
    assert!(at("pos_y") > box_.min[1] + box_.size[1] * 0.5, "an Inflow starts in the top half");

    // The receipt selects the Inflow and unfolds its water, so its row and
    // gizmo show.
    let update = crate::edit_selection::EditSelectionUpdate { sequence: 1, selection: pending.resolve(&project).expect("the Inflow is found") };
    assert!(crate::edit_selection::apply_update(&mut ui, &project, Some(&update), &mut selection, &mut active));
    sync(&mut ui, &project, &selection);
    assert_eq!(
        ui.scene_setup_panel.live_selection(),
        Some(&manifold_ui::panels::scene_setup_panel::SceneSelection::Source(placed.node.clone()))
    );
    let rows = scene_tree_rows(&mut ui);
    let at = rows.iter().position(|name| name == &water.name).expect("the water row");
    let children = SceneVm::from_def(&added)
        .unwrap()
        .objects
        .iter()
        .filter(|object| matches!(object, SceneObjectVm::Known(child) if child.parent.as_ref() == Some(&water.address)))
        .count();
    assert_eq!(rows.get(at + children + 2).map(String::as_str), Some("Inflow 1"), "the Inflow lists after the water's children and fill: {rows:?}");

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

/// BUG-dphvp: Delete on a placed source takes its group and its wire into the
/// water out in one undo step; redo takes them out again.
#[test]
fn physics_boundary_delete_placed_source_one_undo() {
    let (mut project, layer, _) = super::water_family::water_project();
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    selection.select_layer(layer.clone());
    selection.pin_scope(manifold_ui::InspectorTab::Scene);
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut editing = EditingService::new();
    let mut run = |action: &ProjectAction, project: &mut Project, ui: &mut UIRoot, selection: &mut SelectionState| {
        dispatch_project(action, &mut crate::ui_bridge::ProjectRef::new(project, &mut Default::default(), &tx), &tx, &state,
            ui, selection, &mut active, &mut prefs, Default::default());
        let command = match rx.try_recv() {
            Ok(ContentCommand::ExecuteSelecting(command, _) | ContentCommand::ExecuteOnContent(command)) => command,
            Ok(ContentCommand::GraphEditRejected(reason)) => panic!("refused: {reason}"),
            _ => panic!("a content edit"),
        };
        editing.execute(command, project);
        assert_eq!(editing.take_rejection(), None);
    };
    sync(&mut ui, &project, &selection);
    let menu = ui.scene_setup_panel.add_menu(manifold_ui::panels::scene_setup_panel::SceneGroup::Objects);
    let Some(manifold_ui::PanelAction::Project(insert)) = menu.iter().find(|entry| entry.label == "Drain").and_then(|entry| entry.action.clone())
    else { panic!("Drain in the Add menu") };
    run(&insert, &mut project, &mut ui, &mut selection);
    let added = effective_def(&project, &layer);
    let water = water_row(&added);
    let placed = water.placed_sources.iter().find(|source| source.name == "Drain 1").expect("the Drain's row");

    sync(&mut ui, &project, &selection);
    ui.scene_setup_panel.set_selection(layer.clone(), manifold_ui::panels::scene_setup_panel::SceneSelection::Source(placed.node.clone()));
    let Some(manifold_ui::PanelAction::Project(remove)) = ui.scene_setup_panel.remove_selection_action() else {
        panic!("Delete on a source removes it")
    };
    assert!(matches!(&remove, ProjectAction::SceneSetupRemoveSource(_, node) if *node == placed.node));
    run(&remove, &mut project, &mut ui, &mut selection);
    let removed = effective_def(&project, &layer);
    let level = domain_level(&removed, &water);
    assert!(!level.nodes.iter().any(|node| node.type_id == GROUP_TYPE_ID && node.handle.as_deref() == Some("Drain 1")), "the Drain's group is gone");
    let group = source_group(domain_level(&added, &water), "Drain 1").id;
    assert!(!level.wires.iter().any(|wire| wire.from_node == group || wire.to_node == group), "no wire is left dangling");
    assert!(water_row(&removed).placed_sources.iter().all(|source| source.name != "Drain 1"));

    assert!(editing.undo(&mut project));
    assert_eq!(effective_def(&project, &layer), added, "one undo brings the Drain back");
    assert!(editing.redo(&mut project));
    assert_eq!(effective_def(&project, &layer), removed);
}

/// Role set to Initial Fill from the card moves Effects, so the frame that
/// shows the write rebuilds the Scene tab tree: the Inflow's row becomes an
/// Initial Fill row. Undo moves Effects back the same way.
#[test]
fn physics_boundary_role_to_initial_fill_rebuilds_the_tree() {
    let (mut project, layer, _) = super::water_family::water_project();
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    selection.select_layer(layer.clone());
    selection.pin_scope(manifold_ui::InspectorTab::Scene);
    sync(&mut ui, &project, &selection);
    let menu = ui.scene_setup_panel.add_menu(manifold_ui::panels::scene_setup_panel::SceneGroup::Objects);
    let Some(manifold_ui::PanelAction::Project(insert)) = menu.iter().find(|entry| entry.label == "Inflow").and_then(|entry| entry.action.clone())
    else { panic!("Inflow in the Add menu") };
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(&insert, &mut crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &tx), &tx, &state,
        &mut ui, &mut selection, &mut active, &mut prefs, Default::default());
    let Ok(ContentCommand::ExecuteSelecting(command, select)) = rx.try_recv() else { panic!("an Inflow inserts on content") };
    let pending = select.capture(&project);
    let mut editing = EditingService::new();
    editing.execute(command, &mut project);
    assert_eq!(editing.take_rejection(), None);
    let added = effective_def(&project, &layer);
    let water = water_row(&added);
    assert_eq!(water.placed_sources.len(), 2, "the water's fill and the Inflow");
    // The receipt selects the Inflow and unfolds its water.
    let update = crate::edit_selection::EditSelectionUpdate { sequence: 1, selection: pending.resolve(&project).expect("the Inflow is found") };
    assert!(crate::edit_selection::apply_update(&mut ui, &project, Some(&update), &mut selection, &mut active));
    sync(&mut ui, &project, &selection);
    assert!(scene_tree_rows(&mut ui).iter().any(|name| name == "Inflow 1"));

    let group = source_group(domain_level(&added, &water), "Inflow 1");
    let role = group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.fluid_role_source").expect("the Inflow's role");
    let scope = vec![water.group_node_id.expect("family group"), group.id];
    editing.execute(apply_scene_param_write(&project, &layer, scope, role.id, "role", 0.0).expect("Role is a card"), &mut project);
    assert_eq!(editing.take_rejection(), None);
    assert!(editing.peek_undo_touches().contains(manifold_core::change::Part::Effects), "the role write moves the tree's input");
    // The Scene tab reads the authored graph: the card value over the literal.
    let authored = |project: &Project| {
        let mut def = effective_def(project, &layer);
        crate::scene_gizmo::project_authored_params(&mut def, project, &manifold_core::GraphTarget::Generator(layer.clone()));
        def
    };
    let role_of = |project: &Project| {
        water_row(&authored(project)).placed_sources.iter().find(|source| source.node == role.node_id).expect("its row stays").role
    };
    assert_eq!(role_of(&project), manifold_foundation::scene_surface::SourceRole::InitialFill, "the row is an Initial Fill now");
    assert!(!water_row(&authored(&project)).control_nodes().contains(&role.node_id), "its controls stay its own");

    assert!(editing.undo(&mut project));
    assert!(editing.peek_redo_touches().contains(manifold_core::change::Part::Effects), "undo moves it back");
    assert_eq!(role_of(&project), manifold_foundation::scene_surface::SourceRole::Inflow);
}

/// Inserting into an imported scene keeps every imported row: the model
/// still lists the import's group and its parts.
#[test]
fn physics_boundary_insert_keeps_imported_rows() {
    let (graph, _) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb")).expect("the azalea imports");
    let mut imported = manifold_core::layer::Layer::new_generator("Azalea".into(), PresetTypeId::new("Scene"), 0);
    let host = imported.gen_params_mut().unwrap();
    host.graph = Some(graph);
    host.refresh_manifest_from_graph();
    let layer = imported.layer_id.clone();
    let mut project = Project::default();
    project.timeline.layers.push(imported);
    let rows = |project: &Project| -> Vec<String> {
        SceneVm::from_def(&effective_def(project, &layer)).unwrap().objects.iter().map(|object| match object {
            SceneObjectVm::Known(row) => format!("{}:{:?}:{:?}", row.name, row.kind, row.parent.is_some()),
            SceneObjectVm::Custom { index } => format!("custom {index}"),
        }).collect()
    };
    let before = rows(&project);
    let mut editing = EditingService::new();
    for kind in [manifold_core::scene_surface::SceneTemplateKind::Object, manifold_core::scene_surface::SceneTemplateKind::Water] {
        let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
        let (tx, rx) = crossbeam_channel::unbounded();
        let action = crate::ui_bridge::project::insert_template_action(&project, layer.clone(), kind);
        dispatch_project(&action, &mut crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &tx), &tx, &state,
            &mut ui, &mut selection, &mut active, &mut prefs, Default::default());
        let command = match rx.try_recv() {
            Ok(ContentCommand::ExecuteSelecting(command, _)) => command,
            Ok(ContentCommand::GraphEditRejected(reason)) => panic!("{kind:?} refused: {reason}"),
            _ => panic!("{kind:?} queued nothing"),
        };
        editing.execute(command, &mut project);
        assert_eq!(editing.take_rejection(), None);
    }
    let after = rows(&project);
    assert!(after.iter().all(|row| !row.starts_with("custom")), "before {before:?}\nafter {after:?}");
    assert!(before.iter().all(|row| after.contains(row)), "before {before:?}\nafter {after:?}");
}
