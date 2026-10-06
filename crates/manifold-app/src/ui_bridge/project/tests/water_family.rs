//! F1b exercises the real catalog load, content commands and manifest bindings.
use super::*;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::{GraphTarget, NodeId};
use manifold_editing::service::EditingService;
use manifold_renderer::node_graph::scene_vm::{SceneObjectKnownRow, SceneObjectVm, SceneVm};

pub(super) fn water_project() -> (Project, LayerId, u32) {
    let mut project = Project::default();
    let index = project.timeline.add_layer("Water", LayerType::Generator, PresetTypeId::new("WaterDamBreakGpuFlip"));
    let layer = &mut project.timeline.layers[index];
    layer.gen_params_or_init();
    let id = layer.layer_id.clone();
    let def = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
    let render = def.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap().id;
    (project, id, render)
}

fn add_water(project: &mut Project, layer: &LayerId, render: u32, editing: &mut EditingService) {
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let before = effective_def(project, layer);
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(&ProjectAction::SceneSetupAddFluid(layer.clone(), render), project, &tx, &state,
        &mut ui, &mut selection, &mut active, &mut prefs);
    assert_eq!(effective_def(project, layer), before, "UI only queues content edits");
    let ContentCommand::ExecuteSelecting(command, selection) = rx.try_recv().unwrap() else { panic!("content insertion") };
    let pending = selection.capture(project);
    editing.execute(command, project);
    assert_eq!(editing.take_rejection(), None);
    let Some(crate::edit_selection::EditSelection::Object { object_id, .. }) = pending.resolve(project) else { panic!("water selection") };
    assert!(rows(&effective_def(project, layer)).iter().any(|row| row.object_node_id == object_id && row.is_group && row.liquid_domain.is_some()));
    assert!(rx.is_empty());
}

fn rows(def: &EffectGraphDef) -> Vec<SceneObjectKnownRow> {
    SceneVm::from_def(def).unwrap().objects.into_iter().filter_map(|row| match row {
        SceneObjectVm::Known(row) => Some(*row), _ => None,
    }).collect()
}

fn binding(def: &EffectGraphDef, node: &NodeId, param: &str) -> String {
    def.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| matches!(&binding.target,
        BindingTarget::Node { node_id, param: name } if node_id == node && name == param)).unwrap().id.clone()
}

fn family_value(project: &Project, layer: &LayerId, row: &SceneObjectKnownRow, param: &str) -> f32 {
    let def = effective_def(project, layer);
    let id = binding(&def, &row.object, param);
    let (_, layer) = project.timeline.find_layer_by_id(layer).unwrap();
    layer.gen_params().unwrap().get_base_param(&id)
}

#[test]
fn water_family_add_undo_redo() {
    let (mut project, layer, render) = water_project();
    let mut editing = EditingService::new();
    let default = effective_def(&project, &layer);
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let plan = manifold_renderer::node_graph::gltf_import::assemble_merge_plan(&default, &fixture).expect("import compound fixture");
    editing.execute(Box::new(manifold_editing::commands::graph::ImportModelIntoSceneCommand::new(
        GraphTarget::Generator(layer.clone()), vec![], plan.render_scene_node_id, plan.new_nodes,
        plan.new_wires, plan.new_objects_count, plan.new_card_params, plan.new_card_bindings,
        plan.new_string_bindings, default)), &mut project);
    assert_eq!(editing.take_rejection(), None);
    let imported = effective_def(&project, &layer);
    assert!(rows(&imported).iter().any(|row| row.is_group && row.liquid_domain.is_none()), "real imported compound parent");
    let initial_count = objects_param(&project, &layer, render) as usize;
    let mut previous = imported;
    for insertion in 0..2 {
        add_water(&mut project, &layer, render, &mut editing);
        let added = effective_def(&project, &layer);
        let start = initial_count + insertion * 4;
        assert_eq!(objects_param(&project, &layer, render) as usize, start + 4);
        let family: Vec<_> = rows(&added).into_iter().filter(|row| (start..start + 4).contains(&row.index)).collect();
        assert_eq!(family.len(), 4);
        assert!(family[0].is_group);
        let group_id = family[0].group_node_id.unwrap();
        for (offset, port) in ["object", "object_1", "object_2", "object_3"].into_iter().enumerate() {
            assert!(added.wires.iter().any(|wire| wire.from_node == group_id && wire.from_port == port
                && wire.to_node == render && wire.to_port == format!("object_{}", start + offset)));
        }
        let flat = manifold_core::flatten::flatten_groups(&added).unwrap();
        let render_stable = &added.nodes.iter().find(|node| node.id == render).unwrap().node_id;
        let flat_render = flat.nodes.iter().find(|node| &node.node_id == render_stable).unwrap().id;
        for row in &family {
            let object = flat.nodes.iter().find(|node| node.node_id == row.object).unwrap();
            assert!(flat.wires.iter().any(|wire| wire.from_node == object.id && wire.to_node == flat_render && wire.to_port == format!("object_{}", row.index)), "physical draw slot survives flattening");
            for input in ["vertices", "material"] {
                assert!(flat.wires.iter().any(|wire| wire.to_node == object.id && wire.to_port == input), "{input} reaches every draw object");
            }
        }
        let gate = binding(&added, &family[0].object, "parent_visible");
        assert_eq!(added.preset_metadata.as_ref().unwrap().bindings.iter().filter(|binding| binding.id == gate).count(), 4);
        let metadata = added.preset_metadata.as_ref().unwrap();
        for child in &family[1..] {
            let mesh = child.look_mesh.as_ref().expect("look mesh");
            let size = binding(&added, mesh, "radius");
            assert!(size.ends_with("_size"), "recipe-authored Size survives fresh IDs");
            assert_eq!(metadata.params.iter().find(|spec| spec.id == size).unwrap().name, "Size");
            assert!(!metadata.bindings.iter().any(|binding| matches!(&binding.target,
                BindingTarget::Node { node_id, param } if node_id == &child.object && param == "cast_shadows")));
        }
        let budget = metadata.params.iter().find(|spec| spec.id == format!("{group_id}_whitewater_capacity"))
            .expect("shared whitewater budget");
        assert_eq!(budget.section.as_deref(), Some(format!("{} - Water Detail", family[0].name).as_str()));
        assert!(editing.undo(&mut project));
        assert_eq!(effective_def(&project, &layer), previous);
        assert!(editing.redo(&mut project));
        assert_eq!(effective_def(&project, &layer), added, "redo preserves all IDs and bindings");
        previous = added;
    }
    let metadata = previous.preset_metadata.as_ref().unwrap();
    let gates: std::collections::BTreeSet<_> = metadata.bindings.iter().filter_map(|binding| matches!(&binding.target,
        BindingTarget::Node { param, .. } if param == "parent_visible").then_some(&binding.id)).collect();
    assert!(gates.len() >= 3, "families have independent parent gates");
}

#[test]
fn water_family_add_through_content_admission_discovers_children() {
    let (mut project, layer, render) = water_project();
    let initial_objects = objects_param(&project, &layer, render) as usize;
    assert_eq!(initial_objects, 10, "shipped physical slot count includes integer literals");
    project.on_after_deserialize();
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(&ProjectAction::SceneSetupAddFluid(layer.clone(), render),
        &mut project, &tx, &state, &mut ui, &mut selection, &mut active, &mut prefs);
    let ContentCommand::ExecuteSelecting(command, _) = rx.try_recv().unwrap() else {
        panic!("Add Water must execute on content");
    };
    let mut editing = EditingService::new();
    editing.execute(crate::scene_modifier_edit::with_admission(command), &mut project);
    assert_eq!(editing.take_rejection(), None);
    project.on_after_deserialize();
    let family = rows(&effective_def(&project, &layer));
    let water = family.iter().find(|row| row.is_group && row.liquid_domain.is_some()
        && row.index >= initial_objects).expect("new Water parent");
    let children: Vec<_> = family.iter().filter(|row| row.parent_group_id == Some(water.object_node_id)
        && (initial_objects..initial_objects + 4).contains(&row.index)).collect();
    assert_eq!(children.len(), 3);
    assert!(children.iter().all(|row| row.look_mesh.is_some()));
}

#[test]
fn water_family_visibility_round_trip() {
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    let (mut project, layer, render) = water_project();
    let mut editing = EditingService::new();
    add_water(&mut project, &layer, render, &mut editing);
    let def = effective_def(&project, &layer);
    let families = rows(&def);
    for water in families.iter().filter(|row| row.is_group && row.liquid_domain.is_some()) {
        let children: Vec<_> = families.iter().filter(|row| row.parent_group_id == Some(water.object_node_id)).collect();
        assert_eq!(children.len(), 3);
        let child = children[0];
        let write = |project: &mut Project, editing: &mut EditingService, row: &SceneObjectKnownRow, value| {
            let command = apply_scene_param_write(project, &layer, row.visible_addr.scope_path.clone(),
                row.visible_addr.node_doc_id, &row.visible_addr.param_id, value).unwrap();
            editing.execute(command, project);
            assert_eq!(editing.take_rejection(), None);
        };
        write(&mut project, &mut editing, child, 0.0);
        write(&mut project, &mut editing, water, 0.0);
        for object in std::iter::once(water).chain(children.iter().copied()) {
            assert_eq!(family_value(&project, &layer, object, "parent_visible"), 0.0);
        }
        write(&mut project, &mut editing, water, 1.0);
        assert_eq!(family_value(&project, &layer, child, "visible"), 0.0);
        assert!(editing.undo(&mut project));
        assert_eq!(family_value(&project, &layer, water, "parent_visible"), 0.0);
        assert!(editing.redo(&mut project));
    }
    let saved = serde_json::to_string(&project).unwrap();
    let mut reloaded = manifold_io::loader::load_project_from_json(&saved).expect("real project load pipeline");
    let after = effective_def(&reloaded, &layer);
    let water = families.iter().find(|row| row.is_group && row.liquid_domain.is_some()).unwrap();
    let foam = families.iter().find(|row| row.parent_group_id == Some(water.object_node_id)).unwrap();
    assert_eq!(family_value(&reloaded, &layer, foam, "visible"), 0.0);
    assert_eq!(family_value(&reloaded, &layer, water, "parent_visible"), 1.0);
    let whitewater = water.fluid_controls.iter().find(|node| manifold_core::scene_modifier_preset::SceneNodeRef::locate(&after, node)
        .and_then(|reference| reference.resolve(&after)).is_some_and(|node| node.type_id == "node.whitewater_step")).unwrap();
    let amount = binding(&after, whitewater, "amount");
    let size = binding(&after, foam.look_mesh.as_ref().unwrap(), "radius");
    assert_ne!(amount, size);
    for id in [&amount, &size] {
        editing.execute(Box::new(manifold_editing::commands::drivers::AddDriverCommand::new(
            manifold_editing::commands::effect_target::DriverTarget::GeneratorParam { layer_id: layer.clone() },
            ParameterDriver::new(id.clone(), BeatDivision::Quarter, DriverWaveform::Sine))), &mut reloaded);
    }
    let with_drivers = serde_json::to_string(&reloaded).unwrap();
    let mut reloaded = manifold_io::loader::load_project_from_json(&with_drivers).expect("driver round trip");
    assert!(manifold_playback::modulation::evaluate_all_drivers(&mut reloaded, manifold_core::Beats(0.0), manifold_core::Seconds::ZERO));
    let values = |project: &Project| [amount.as_str(), size.as_str()].map(|id| project.timeline.layers[0].gen_params().unwrap().params.get(id).unwrap().value);
    let first = values(&reloaded);
    manifold_playback::modulation::evaluate_all_drivers(&mut reloaded, manifold_core::Beats(0.25), manifold_core::Seconds(0.125));
    let second = values(&reloaded);
    for index in 0..2 { assert!((first[index] - second[index]).abs() > 0.00001, "distinct reloaded target {index} modulates"); }
    assert_eq!(binding(&after, whitewater, "amount"), amount);
    assert_eq!(binding(&after, foam.look_mesh.as_ref().unwrap(), "radius"), size);
}
