//! Edits made through the cards while a show plays reach the running Physics
//! World: a dragged Fixed body moves its engine body, and no edit rebuilds
//! the world.
use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::params::{Param, ParamManifest};
use manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures;
use manifold_water_rigid::primitives::physics_world::PhysicsWorldNode;

fn solids() -> (EffectGraphDef, ParamManifest) {
    let mut def: EffectGraphDef =
        serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_PHYSICSSOLIDS_JSON).unwrap();
    migrate_scene_exposures(&mut def);
    let manifest = ParamManifest::from_params(
        def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect(),
    );
    (def, manifest)
}

fn card_of(def: &EffectGraphDef, node: &str, param: &str) -> String {
    def.preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param: p } if node_id.as_str() == node && p == param))
        .unwrap_or_else(|| panic!("{node}.{param} has no card"))
        .id
        .clone()
}

fn world_poses(runtime: &PresetRuntime, def: &EffectGraphDef) -> (Vec<manifold_node_engine::scene::transform::Transform>, Option<u64>) {
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
    let id = runtime.graph.instance_by_node_id(&world.node_id).unwrap();
    let node = runtime.graph.get_node(id).unwrap().node.as_any().downcast_ref::<PhysicsWorldNode>().unwrap();
    (node.simulation.poses.clone(), node.simulation.impulse_epoch())
}

fn play(runtime: &mut PresetRuntime, manifest: &ParamManifest, from: usize, to: usize) {
    for frame in from..to {
        runtime.apply_param_values(manifest);
        runtime.execute_frame(time(frame as f64 * DT));
    }
}

/// PhysicsSolids' ground is a Fixed body whose transform is a card. Dragging
/// it while the show plays moves the engine body within a tick or two; the
/// falling solids keep falling instead of restarting.
#[test]
fn a_dragged_fixed_body_moves_its_engine_body_without_a_rebuild() {
    let (def, mut manifest) = solids();
    let mut runtime = PresetRuntime::from_def_for_render(def.clone(), &registry(), Some(&manifest), false).unwrap();
    play(&mut runtime, &manifest, 0, 20);
    let (before, epoch) = world_poses(&runtime, &def);
    let card = card_of(&def, "physics_demo_100", "pos_x");
    manifest.get_mut(&card).unwrap().value = 6.0;
    play(&mut runtime, &manifest, 20, 23);
    let (after, rebuilt) = world_poses(&runtime, &def);
    assert!((after[0].pos[0] - 6.0).abs() < 1e-3, "the ground moved: {:?}", after[0].pos);
    assert_eq!(rebuilt, epoch, "no rebuild");
    assert!(after[1].pos[1] < before[1].pos[1], "the falling solid kept falling: {:?} {:?}", before[1].pos, after[1].pos);
}
