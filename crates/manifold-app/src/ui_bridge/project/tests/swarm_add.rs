//! The Add menu's Swarm goes in as one template: its object row owns the
//! swarm's forces node, seeder, copies and box, the box is a gizmo shape,
//! scene forces reach it, it follows the Physics World's clock as water does
//! (D-107), its card carries Count, Size and Air Drag under Simulation, it
//! brings an Attractor, a Radial Force and gusts that belong to it (D-114): they go with it
//! on delete and come with a duplicate. The whole insertion is one undo step.

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
    assert_eq!(effective_def(&project, &layer).scene_modifiers, before.scene_modifiers, "and none of its forces");
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
    // The swarm brings its forces: expand them before flattening.
    let expanded = manifold_node_engine::load::expand::prepare_scene_modifiers(&def, &manifold_node_engine::persistence::PrimitiveRegistry::with_builtin())
        .expect("the swarm's forces expand")
        .def;
    let flat = manifold_core::flatten::flatten_groups(&expanded).expect("flattens");
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
/// The stock forces a swarm brings, in order (D-114).
const SWARM_FORCES: [&str; 3] = ["Attractor", "RadialForce", "Wind"];

/// The groups of `def` that hold a swarm, in graph order.
fn swarm_groups(def: &EffectGraphDef) -> Vec<&EffectGraphNode> {
    def.nodes
        .iter()
        .filter(|node| node.group.as_deref().is_some_and(|body| body.nodes.iter().any(|node| node.type_id == "node.swarm_forces")))
        .collect()
}

/// The preset names of the forces aimed at the swarm in `group` alone.
fn forces_on(def: &EffectGraphDef, group: &EffectGraphNode) -> Vec<String> {
    use manifold_core::scene_modifier_preset::SceneTargetSelection;
    let object = group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.scene_object").unwrap();
    def.scene_modifiers
        .iter()
        .filter(|instance| matches!(&instance.targets, SceneTargetSelection::Explicit { objects }
            if objects.len() == 1 && objects[0].node == object.node_id && objects[0].scope == [group.node_id.clone()]))
        .map(|instance| instance.graph.preset_metadata.as_ref().unwrap().id.as_str().to_owned())
        .collect()
}

/// The preset names of the forces aimed at the one swarm's object alone.
fn swarm_forces_on_its_object(def: &EffectGraphDef) -> Vec<String> {
    let groups = swarm_groups(def);
    let [group] = groups.as_slice() else { panic!("one swarm") };
    forces_on(def, group)
}

/// The content command the UI action `action` queues.
fn dispatched(project: &mut Project, action: ProjectAction) -> ContentCommand {
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(
        &action,
        &mut crate::ui_bridge::ProjectRef::new(project, &mut Default::default(), &tx),
        &tx,
        &state,
        &mut ui,
        &mut selection,
        &mut active,
        &mut prefs,
        Default::default(),
    );
    rx.try_recv().expect("a queued edit")
}

/// The scene row index of the first swarm.
fn swarm_row(def: &EffectGraphDef) -> u32 {
    SceneVm::from_def(def)
        .unwrap()
        .objects
        .iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) if row.swarm.is_some() => Some(row.index as u32),
            _ => None,
        })
        .expect("a swarm row")
}

/// Duplicate the first swarm with its row's duplicate button.
fn duplicate_swarm(project: &mut Project, editing: &mut EditingService, layer: &LayerId, render: u32) {
    let index = swarm_row(&effective_def(project, layer));
    let ContentCommand::SceneItem(action) =
        dispatched(project, ProjectAction::SceneSetupDuplicateObject(layer.clone(), render, index))
    else { panic!("duplicate is a content-owned scene item edit") };
    let command = crate::scene_item_transfer::build_action(project, action).expect("the duplicate builds");
    editing.execute(command, project);
    assert_eq!(editing.take_rejection(), None);
}

/// What a scene's physics looks like, whatever ids its parts took: the
/// World's controls, the water's and the swarm's params, and the forces.
fn physics_look(def: &EffectGraphDef) -> impl PartialEq + std::fmt::Debug {
    let nodes = all_nodes(&def.nodes);
    let params = |found: fn(&str) -> bool| nodes.iter().find(|node| found(&node.type_id)).map(|node| node.params.clone()).expect("a simulation");
    (
        manifold_core::scene_world_controls::WORLD_INPUTS.map(|control| world_source_value(def, control.world)),
        params(manifold_core::liquid_domain::is_liquid_domain),
        params(|type_id| type_id == "node.swarm_forces"),
        def.scene_modifiers.iter().map(|instance| (*instance.graph).clone()).collect::<Vec<_>>(),
    )
}

/// D-114: a swarm brings its own Attractor, Radial Force and drifting gusts, aimed at it
/// alone, and never sets the Physics World: still air stays 0, and World Air
/// Resistance is a setup row in the World's section. A swarm and water look
/// the same whichever is added first. The shipped preset is the same.
#[test]
fn add_swarm_brings_an_attractor_and_gusts_and_leaves_the_world_alone() {
    use manifold_core::scene_surface::SceneTemplateKind::{Swarm, Water};
    let added = |kinds: &[manifold_core::scene_surface::SceneTemplateKind]| {
        let (mut project, layer, _) = scene_layer_project();
        for kind in kinds {
            insert(&mut project, &layer, *kind);
        }
        let def = effective_def(&project, &layer);
        let (_, owner) = project.timeline.find_layer_by_id(&layer).unwrap();
        (def, owner.gen_params().unwrap().params.clone())
    };
    let (def, params) = added(&[Swarm]);
    assert_eq!(world_source_value(&def, "air_resistance"), 0.0, "the swarm leaves the World's still air alone");
    assert_eq!(swarm_forces_on_its_object(&def), SWARM_FORCES);
    assert_eq!(def.scene_modifiers.len(), SWARM_FORCES.len(), "nothing else is pushed");
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
    let binding = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &world.node_id && param == "air_resistance"))
        .expect("World Air Resistance is a scene row");
    let scene = params.get(&binding.id).and_then(|param| param.spec.scene).expect("scene facts");
    assert_eq!((scene.role, scene.group), (SurfaceRole::Setup, ControlGroup::PhysicsWorld));

    let (swarm_first, _) = added(&[Swarm, Water]);
    let (water_first, _) = added(&[Water, Swarm]);
    assert_eq!(physics_look(&swarm_first), physics_look(&water_first), "the same water and swarm in either order");
    assert_eq!(swarm_forces_on_its_object(&water_first), SWARM_FORCES, "the swarm's forces push it, not the water");

    let preset = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("Swarm")).unwrap();
    assert_eq!(world_source_value(preset.as_ref(), "air_resistance"), 0.0);
    assert_eq!(swarm_forces_on_its_object(preset.as_ref()), SWARM_FORCES);
}

/// A swarm's forces belong to it (D-94): its row's delete removes the swarm
/// and the forces aimed at it alone in one undo step, with their card rows,
/// and one undo brings both back.
#[test]
fn delete_swarm_takes_its_forces_and_one_undo_brings_both_back() {
    let (mut project, layer, render) = scene_layer_project();
    let mut editing = insert_swarm(&mut project, &layer);
    let with_swarm = effective_def(&project, &layer);
    let index = swarm_row(&with_swarm);
    let ContentCommand::ExecuteOnContent(command) =
        dispatched(&mut project, ProjectAction::SceneSetupRemoveObject(layer.clone(), render, index))
    else { panic!("delete is a content-owned edit") };
    editing.execute(command, &mut project);
    assert_eq!(editing.take_rejection(), None, "the delete is accepted");

    let deleted = effective_def(&project, &layer);
    assert!(swarm_groups(&deleted).is_empty(), "the swarm is gone");
    assert!(deleted.scene_modifiers.is_empty(), "its forces went with it");
    assert!(
        !deleted.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| matches!(binding.target, BindingTarget::SceneModifier { .. })),
        "and so did their card rows"
    );

    assert!(editing.undo(&mut project));
    let back = effective_def(&project, &layer);
    assert_eq!(back.scene_modifiers, with_swarm.scene_modifiers, "one undo brings the forces back");
    assert_eq!(swarm_forces_on_its_object(&back), SWARM_FORCES);

    // The shipped preset's swarm deletes the same way.
    let preset = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("Swarm")).unwrap();
    let render = preset.nodes.iter().find(|node| node.type_id == "node.render_scene").unwrap().id;
    let mut project = Project::default();
    let idx = project.timeline.add_layer("Swarm", LayerType::Generator, PresetTypeId::new("Swarm"));
    let layer = project.timeline.layers[idx].layer_id.clone();
    let mut command = manifold_editing::commands::graph::RemoveSceneObjectCommand::new(
        manifold_core::GraphTarget::Generator(layer.clone()),
        Vec::new(),
        render,
        swarm_row(preset.as_ref()),
        (*preset).clone(),
    );
    command.execute(&mut project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    assert!(effective_def(&project, &layer).scene_modifiers.is_empty());
}

/// Duplicating a swarm gives the copy its own forces, tuned as
/// the source's and aimed at the copy alone; the source keeps its own, and
/// one undo takes the copy and its forces away.
#[test]
fn duplicate_swarm_brings_its_own_forces() {
    let (mut project, layer, render) = scene_layer_project();
    let mut editing = insert_swarm(&mut project, &layer);
    let before = effective_def(&project, &layer);
    duplicate_swarm(&mut project, &mut editing, &layer, render);
    let def = effective_def(&project, &layer);
    let groups = swarm_groups(&def);
    assert_eq!(groups.len(), 2, "a second swarm");
    for group in &groups {
        assert_eq!(forces_on(&def, group), SWARM_FORCES, "{:?} has its own forces", group.handle);
    }
    assert_eq!(def.scene_modifiers.len(), 2 * SWARM_FORCES.len());
    let graphs = |def: &EffectGraphDef| def.scene_modifiers.iter().map(|instance| (*instance.graph).clone()).collect::<Vec<_>>();
    let n = SWARM_FORCES.len();
    assert_eq!(graphs(&def)[n..], graphs(&def)[..n], "the copies are tuned as the source's");

    assert!(editing.undo(&mut project));
    assert_eq!(effective_def(&project, &layer).scene_modifiers, before.scene_modifiers);
}

/// A duplicated swarm holds its formation: after 2 s under World gravity the
/// copy's cloud, pushed by its own forces, sits where the source's does, held
/// well above where a swarm with no forces would have fallen.
#[cfg(feature = "gpu-proofs")]
#[test]
fn a_duplicated_swarm_holds_formation_after_two_seconds() {
    use manifold_node_engine::particles::Particle;
    const SIZE: u32 = 64;
    let (mut project, layer, render) = scene_layer_project();
    let mut editing = insert_swarm(&mut project, &layer);
    duplicate_swarm(&mut project, &mut editing, &layer, render);
    let def = effective_def(&project, &layer);
    let moves: Vec<manifold_core::NodeId> = swarm_groups(&def)
        .iter()
        .map(|group| group.group.as_deref().unwrap().nodes.iter().find(|node| node.type_id == "node.move_swarm").unwrap().node_id.clone())
        .collect();
    let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
    // Each swarm's live particles after 2 s: their centre and how far they spread.
    let cloud = |mover: &manifold_core::NodeId| {
        let mut runtime = manifold_node_engine::runtime::PresetRuntime::from_def_with_device(
            def.clone(),
            &manifold_node_engine::persistence::PrimitiveRegistry::with_builtin(),
            std::sync::Arc::clone(device),
            SIZE,
            SIZE,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("the scene builds");
        runtime.set_dump_arrays(None, std::slice::from_ref(mover));
        let target = manifold_node_engine::gpu::render_target::RenderTarget::new(device, SIZE, SIZE, manifold_gpu::GpuTextureFormat::Rgba16Float, "duplicate-swarm");
        let manifest = manifold_core::params::ParamManifest::default();
        for frame in 0..=120u32 {
            let seconds = f64::from(frame) / 60.0;
            let context = manifold_node_engine::runtime::preset_context::PresetContext {
                time: seconds, beat: seconds * 2.0, dt: 1.0 / 60.0, width: SIZE, height: SIZE,
                output_width: SIZE, output_height: SIZE, aspect: 1.0, owner_key: 0, is_clip_level: false,
                frame_count: 0, anim_progress: 0.0, trigger_count: 0,
            };
            runtime.set_sim_step(manifold_node_engine::exec::effect_node::SimStep::live(manifold_core::Seconds(1.0 / 60.0)));
            let mut encoder = device.create_encoder("duplicate-swarm");
            runtime.render(&mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut encoder, device), &target.texture, &context, &manifest);
            encoder.commit_and_wait_completed();
        }
        let dumps = runtime.dump_arrays_all();
        let dump = dumps.iter().find(|dump| dump.port == "out").expect("the swarm's particles are held");
        let count = (dump.buffer.size / std::mem::size_of::<Particle>() as u64) as usize;
        let particles: Vec<Particle> = manifold_node_engine::testkit::array_harness::read(dump.buffer, count);
        let live: Vec<[f32; 3]> = particles.iter().filter(|particle| particle.life > 0.0).map(|particle| particle.position).collect();
        let n = live.len() as f32;
        let centre = [0, 1, 2].map(|axis| live.iter().map(|p| p[axis]).sum::<f32>() / n);
        let spread = (live.iter().map(|p| (0..3).map(|axis| (p[axis] - centre[axis]).powi(2)).sum::<f32>()).sum::<f32>() / n).sqrt();
        (live.len(), centre, spread)
    };
    let source = cloud(&moves[0]);
    let copy = cloud(&moves[1]);
    // With no forces, gravity against the gusts' drag (3/s) would carry a
    // cloud about 5.4 m down in 2 s.
    for (live, centre, spread) in [source, copy] {
        assert_eq!(live, 20_000);
        assert!(centre[1] > -2.5, "the cloud is held up, not falling: {centre:?}");
        assert!(spread < 3.0, "the cloud keeps together: {spread}");
    }
    let apart = (0..3).map(|axis| (source.1[axis] - copy.1[axis]).abs()).fold(0.0f32, f32::max);
    assert!(apart < 1.0, "the copy holds formation where the source does: {source:?} {copy:?}");
}
