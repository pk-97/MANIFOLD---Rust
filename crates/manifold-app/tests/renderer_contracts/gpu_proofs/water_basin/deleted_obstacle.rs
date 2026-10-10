//! Deleting the GPU FLIP Dam Break obstacle through Remove Object takes its
//! collider out of the solver: the domain reports one body before, none after.
use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::layer::Layer;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::{EffectId, GraphTarget, LayerId, NodeId, PresetTypeId};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::RemoveSceneObjectCommand;

fn remove_obstacle(def: &EffectGraphDef) -> EffectGraphDef {
    let render_id = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .id;
    let index = def
        .wires
        .iter()
        .find(|wire| {
            wire.to_node == render_id
                && def.nodes.iter().any(|node| {
                    node.id == wire.from_node && node.handle.as_deref() == Some("Obstacle")
                })
        })
        .and_then(|wire| wire.to_port.strip_prefix("object_")?.parse().ok())
        .expect("Obstacle object slot");
    let mut layer = Layer::new_generator("Dam".into(), PresetTypeId::new("WaterDamBreakGpuFlip"), 0);
    let layer_id = LayerId::new("deleted-obstacle-proof");
    layer.layer_id = layer_id.clone();
    let host = layer.gen_params_or_init();
    host.graph = Some(def.clone());
    host.refresh_manifest_from_graph();
    let mut project = Project::default();
    project.timeline.layers.push(layer);
    let target = GraphTarget::Generator(layer_id);
    let mut remove = RemoveSceneObjectCommand::new(target.clone(), vec![], render_id, index, def.clone());
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    project
        .graph_target_owner(&target)
        .and_then(|owner| owner.graph.clone())
        .unwrap()
}

fn body_count_after_frames(def: &EffectGraphDef, frames: u32) -> f32 {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let manifest = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    let registry = manifold_nodes::registry::GeneratorRegistry::new(
        GpuTextureFormat::Rgba16Float,
    );
    let mut runtime = registry
        .create_with_override(
            Arc::clone(&harness.device),
            &PresetTypeId::new("WaterDamBreakGpuFlip"),
            Some(def),
            WIDTH,
            HEIGHT,
            false,
            Some(&manifest),
            None,
        )
        .unwrap();
    runtime.set_preview_target(&EffectId::default(), Some(&NodeId::new("domain")));
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "deleted-obstacle",
    );
    let _offline = PhysicsStepScope::for_render(true);
    for frame in 0..frames {
        let mut encoder = harness.device.create_encoder("deleted-obstacle-frame");
        let status = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
            runtime.render(&mut gpu, &target.texture, &context(frame), &manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        assert!(!matches!(status, FrameRenderStatus::Failed(_)), "{status:?}");
    }
    let (_, outputs) = runtime.preview_scalar_io();
    outputs
        .iter()
        .find(|(port, _)| port == "body_count")
        .unwrap_or_else(|| panic!("domain publishes body_count: {outputs:?}"))
        .1
}

#[test]
fn deleted_gpu_flip_dam_break_obstacle_leaves_the_solver() {
    let mut def: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_WATERDAMBREAKGPUFLIP_JSON)
    .unwrap();
    manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
    assert_eq!(body_count_after_frames(&def, 4), 1.0, "the obstacle is a solver body");
    let after = remove_obstacle(&def);
    assert_eq!(
        body_count_after_frames(&after, 4),
        0.0,
        "a deleted obstacle must not stay in the solver"
    );
}
