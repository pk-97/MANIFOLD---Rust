//! The Add menu's Swarm goes in as one template: its object row owns the
//! swarm's forces node, seeder, copies and box, the box is a gizmo shape,
//! scene forces reach it, it follows the Physics World's clock as water does
//! (D-107), its card carries Count, Size and Air Drag under Simulation, it
//! brings still air, an Attractor and gusts (D-114), and
//! the whole insertion is one undo step.

use super::*;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::scene_surface::{ControlGroup, SurfaceRole};
use manifold_core::scene_template::HANDLE_TOKEN;
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm, ShapeOwner};

/// Every node of `def`, groups entered.
fn all_nodes(nodes: &[EffectGraphNode]) -> Vec<&EffectGraphNode> {
    nodes.iter().flat_map(|node| std::iter::once(node).chain(node.group.as_deref().map_or(Vec::new(), |body| all_nodes(&body.nodes)))).collect()
}

fn insert_swarm(project: &mut Project, layer: &LayerId) -> EditingService {
    insert(project, layer, manifold_core::scene_surface::SceneTemplateKind::Swarm)
}

fn insert(project: &mut Project, layer: &LayerId, kind: manifold_core::scene_surface::SceneTemplateKind) -> EditingService {
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(
        &crate::ui_bridge::project::insert_template_action(project, layer.clone(), kind),
        &mut crate::ui_bridge::ProjectRef::new(project, &mut Default::default(), &tx),
        &tx,
        &state,
        &mut ui,
        &mut selection,
        &mut active,
        &mut prefs,
        Default::default(),
    );
    let ContentCommand::ExecuteSelecting(command, _) = rx.try_recv().expect("queued insertion")
    else { panic!("Add goes through content-owned editing and selection"); };
    let mut editing = EditingService::new();
    editing.execute(command, project);
    assert_eq!(editing.take_rejection(), None);
    editing
}

#[test]
fn add_swarm_inserts_one_row_with_its_forces_and_box_and_undoes_in_one_step() {
    let (mut project, layer, _) = scene_layer_project();
    let before = effective_def(&project, &layer);
    let objects_before = SceneVm::from_def(&before).unwrap().objects.len();
    let mut editing = insert_swarm(&mut project, &layer);

    let added = effective_def(&project, &layer);
    let vm = SceneVm::from_def(&added).unwrap();
    assert_eq!(vm.objects.len(), objects_before + 1, "one new object row");
    let row = vm
        .objects
        .iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) if row.swarm.is_some() => Some(row),
            _ => None,
        })
        .expect("the new row owns a swarm");
    let forces = row.swarm.clone().unwrap().node;
    let types: Vec<&str> = row
        .controls
        .iter()
        .map(|control| control.resolve(&added).expect("a control resolves").type_id.as_str())
        .collect();
    assert_eq!(
        types,
        ["node.swarm_forces", "node.spawn_from_mesh", "node.swarm_to_copies", "node.transform_3d"],
        "the row owns the swarm's clock, its Count, its Size and its box"
    );
    assert!(
        vm.shapes.iter().any(|shape| shape.owner == ShapeOwner::Swarm(forces.clone())),
        "the swarm's box is a gizmo shape from the moment it is inserted"
    );

    assert!(editing.undo(&mut project));
    let undone = SceneVm::from_def(&effective_def(&project, &layer)).unwrap();
    assert_eq!(undone.objects.len(), objects_before, "one undo removes the swarm");
    assert!(
        undone.objects.iter().all(|object| !matches!(object, SceneObjectVm::Known(row) if row.swarm.is_some())),
        "no swarm row is left"
    );
}

/// Scene forces reach an inserted swarm on its forces node's
/// `acceleration_field`, the lattice the shipped Swarm preset's take, and a
/// force's hits go to it as a whole.
#[test]
fn add_swarm_takes_scene_forces_like_the_preset() {
    use manifold_core::scene_index::FlatSceneIndex;
    use manifold_core::scene_impulse::ImpulseTarget;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_node_engine::load::expand::impulse_recipients;
    use manifold_node_engine::load::expand::testkit::recipient_key;
    use manifold_node_engine::persistence::PrimitiveRegistry;

    let registry = PrimitiveRegistry::with_builtin();
    let recipient = |def: &EffectGraphDef| {
        let vm = SceneVm::from_def(def).unwrap();
        let row = vm
            .objects
            .iter()
            .find_map(|object| match object {
                SceneObjectVm::Known(row) if row.swarm.is_some() => Some(row.clone()),
                _ => None,
            })
            .expect("a swarm row");
        let swarm = row.swarm.unwrap();
        let body = def.nodes.iter().find(|node| node.node_id == swarm.scope[0]).and_then(|node| node.group.as_deref()).expect("the swarm group");
        let object = body.nodes.iter().find(|node| node.type_id == "node.scene_object").expect("the swarm's object");
        let object = SceneNodeRef { scope: swarm.scope.clone(), node: object.node_id.clone() };
        let index = FlatSceneIndex::build(def).unwrap();
        let key = recipient_key(&index, &object, &registry).unwrap();
        assert_eq!(key, Some((swarm.clone(), "acceleration_field".to_string())), "forces enter the swarm's lattice");
        let render = def.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap();
        let scene = SceneNodeRef { scope: Vec::new(), node: render.node_id.clone() };
        let hits = impulse_recipients(def, &scene, &SceneTargetSelection::AllObjects, &registry).unwrap();
        assert!(hits.contains(&(swarm.node.clone(), ImpulseTarget::Swarm)), "{hits:?}");
    };

    let (mut project, layer, _) = scene_layer_project();
    insert_swarm(&mut project, &layer);
    recipient(&effective_def(&project, &layer));
    let preset = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("Swarm")).unwrap();
    recipient(preset.as_ref());
}

/// An inserted swarm follows every Physics World control exactly as water
/// does: World Speed drives its Speed and World Reset, a card trigger every
/// clip trigger fires, restarts it (D-107); it falls under World gravity,
/// and the World's Gravity card control drives the source it falls by
/// (D-109). Its own Speed is not a dead card control.
#[test]
fn add_swarm_restarts_on_a_clip_trigger_and_follows_world_speed_and_gravity() {
    let (mut project, layer, _) = scene_layer_project();
    insert_swarm(&mut project, &layer);
    let def = effective_def(&project, &layer);
    let flat = manifold_node_engine::load::expand::flatten_expanded(&def, &manifold_node_engine::persistence::PrimitiveRegistry::with_builtin()).expect("flattens");
    let forces = flat.nodes.iter().find(|node| node.type_id == "node.swarm_forces").expect("forces");
    let world = flat.nodes.iter().find(|node| node.type_id == "node.physics_world").expect("a World");
    let meta = def.preset_metadata.as_ref().unwrap();
    let (_, owner) = project.timeline.find_layer_by_id(&layer).unwrap();
    let params = &owner.gen_params().unwrap().params;
    for manifold_core::scene_world_controls::WorldControlInput { world: world_param, input: port } in manifold_core::scene_world_controls::WORLD_INPUTS {
        let into_forces = flat.wires.iter().find(|wire| wire.to_node == forces.id && wire.to_port == port).expect("World drives it");
        assert!(
            flat.wires.iter().any(|wire| wire.from_node == into_forces.from_node && wire.to_node == world.id && wire.to_port == world_param),
            "the swarm's {port} and the World's share one source"
        );
        let source = flat.nodes.iter().find(|node| node.id == into_forces.from_node).unwrap();
        let binding = meta
            .bindings
            .iter()
            .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &source.node_id && param == "value"))
            .unwrap_or_else(|| panic!("World {port} is on the card"));
        let param = params.get(&binding.id).expect("the World control is a layer param");
        assert_eq!(param.spec.is_trigger, port == "reset");
        if port == "reset" {
            assert_eq!(param.clip_trigger_source, manifold_core::params::ClipTriggerSource::OwnLayer, "every clip trigger fires World Reset");
        }
    }
    assert!(
        !meta.bindings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param }
            if node_id == &forces.node_id && param == "speed")),
        "the swarm's own Speed is the World's"
    );
}

/// The swarm's card: Count, Size and Air Drag are performance controls and
/// Force Detail a setup control, all under Simulation, never Transform
/// (BUG-kzsmu (swarm card controls under Transform)).
#[test]
fn add_swarm_card_carries_count_size_and_air_drag_under_simulation() {
    let (mut project, layer, _) = scene_layer_project();
    insert_swarm(&mut project, &layer);
    let def = effective_def(&project, &layer);
    let meta = def.preset_metadata.as_ref().unwrap();
    let (_, owner) = project.timeline.find_layer_by_id(&layer).unwrap();
    let params = &owner.gen_params().unwrap().params;
    let nodes = all_nodes(&def.nodes);
    let control = |type_id: &str, name: &str| {
        let node = nodes.iter().find(|node| node.type_id == type_id).expect("swarm node");
        let binding = meta
            .bindings
            .iter()
            .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &node.node_id && param == name))
            .unwrap_or_else(|| panic!("{type_id}.{name} is on the card"));
        let spec = meta.params.iter().find(|spec| spec.id == binding.id).unwrap();
        let scene = params.get(&binding.id).and_then(|param| param.spec.scene).expect("scene facts");
        (spec.card_visible, scene.role, scene.group)
    };
    for (type_id, name) in [("node.spawn_from_mesh", "active_count"), ("node.swarm_to_copies", "size"), ("node.swarm_forces", "air_drag")] {
        assert_eq!(control(type_id, name), (true, SurfaceRole::Performance, ControlGroup::Simulation), "{type_id}.{name}");
    }
    assert_eq!(control("node.swarm_forces", "force_detail"), (false, SurfaceRole::Setup, ControlGroup::Simulation));
    assert!(meta.params.iter().all(|spec| spec.section.as_deref().is_none_or(|section| !section.contains(HANDLE_TOKEN))));
    // Count reads "Count" on a performer's 0 to 100,000 slider.
    let seed = nodes.iter().find(|node| node.type_id == "node.spawn_from_mesh").unwrap();
    let count = meta
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &seed.node_id && param == "active_count"))
        .and_then(|binding| meta.params.iter().find(|spec| spec.id == binding.id))
        .unwrap();
    assert_eq!((count.name.as_str(), count.min, count.max), ("Count", 0.0, 100_000.0));
}

/// The value of the source that drives the World control `port`.
fn world_source_value(def: &EffectGraphDef, port: &str) -> f32 {
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").expect("a World");
    let wire = def.wires.iter().find(|wire| wire.to_node == world.id && wire.to_port == port).expect("a World source");
    let source = def.nodes.iter().find(|node| node.id == wire.from_node).unwrap();
    match source.params.get("value") {
        Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => *value,
        other => panic!("World {port} source value: {other:?}"),
    }
}

/// The preset names of the forces aimed at the swarm's object alone.
fn swarm_forces_on_its_object(def: &EffectGraphDef) -> Vec<String> {
    use manifold_core::scene_modifier_preset::SceneTargetSelection;
    let group = def.nodes.iter().find(|node| node.group.as_deref().is_some_and(|body| body.nodes.iter().any(|node| node.type_id == "node.swarm_forces"))).expect("the swarm group");
    let object = group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.scene_object").unwrap();
    def.scene_modifiers
        .iter()
        .filter(|instance| matches!(&instance.targets, SceneTargetSelection::Explicit { objects }
            if objects.len() == 1 && objects[0].node == object.node_id && objects[0].scope == [group.node_id.clone()]))
        .map(|instance| instance.graph.preset_metadata.as_ref().unwrap().id.as_str().to_owned())
        .collect()
}

/// D-114: a swarm brings its own Attractor and drifting gusts, aimed at it
/// alone, and a World it creates has still air, so its Air Drag gives it a
/// terminal speed. A swarm added beside water keeps the water's World, still
/// air 0, and its Attractor holds it. World Air Resistance is on the card,
/// in the World's section. The shipped preset is the same.
#[test]
fn add_swarm_brings_still_air_an_attractor_and_gusts() {
    use manifold_core::scene_surface::SceneTemplateKind;
    let (mut project, layer, _) = scene_layer_project();
    insert_swarm(&mut project, &layer);
    let def = effective_def(&project, &layer);
    assert_eq!(world_source_value(&def, "air_resistance"), 1.0, "a World the swarm creates has still air");
    assert_eq!(swarm_forces_on_its_object(&def), ["Attractor", "Wind"]);
    assert_eq!(def.scene_modifiers.len(), 2, "nothing else is pushed");
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
    let binding = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &world.node_id && param == "air_resistance"))
        .expect("World Air Resistance is on the card");
    let (_, owner) = project.timeline.find_layer_by_id(&layer).unwrap();
    let scene = owner.gen_params().unwrap().params.get(&binding.id).and_then(|param| param.spec.scene).expect("scene facts");
    assert_eq!((scene.role, scene.group), (SurfaceRole::Performance, ControlGroup::PhysicsWorld));

    let (mut project, layer, _) = scene_layer_project();
    insert(&mut project, &layer, SceneTemplateKind::Water);
    insert_swarm(&mut project, &layer);
    let def = effective_def(&project, &layer);
    assert_eq!(def.nodes.iter().filter(|node| node.type_id == "node.physics_world").count(), 1, "one shared World");
    assert_eq!(world_source_value(&def, "air_resistance"), 0.0, "the water's World keeps no still air");
    assert_eq!(swarm_forces_on_its_object(&def), ["Attractor", "Wind"], "the swarm's forces push it, not the water");

    let preset = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("Swarm")).unwrap();
    assert_eq!(world_source_value(preset.as_ref(), "air_resistance"), 1.0);
    assert_eq!(swarm_forces_on_its_object(preset.as_ref()), ["Attractor", "Wind"]);
}
