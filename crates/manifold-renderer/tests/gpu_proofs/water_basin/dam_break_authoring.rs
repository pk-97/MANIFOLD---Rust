//! Exercise the shipped Dam Break with its migrated, real parameter manifest.
use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::Param;
use manifold_core::{EffectId, NodeId};

#[test]
fn migrated_dam_break_publishes_progress_while_live_preview_is_behind() {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let mut def: EffectGraphDef = serde_json::from_str(
        manifold_renderer::testkit::reference_fixtures::cpu_flip_preset_json("WaterDamBreak.json"),
    )
    .unwrap();
    manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
    // Scalar outputs without consumers are pruned. Keep the surface count live
    // through visibility, which is equivalent for any non-empty water mesh.
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 4,
            from_port: "vertex_count".into(),
            to_node: 9,
            to_port: "visible".into(),
        });
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 4,
            from_port: "simulation_time".into(),
            to_node: 14,
            to_port: "metallic".into(),
        });
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
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .unwrap();
    runtime.set_preview_target(&EffectId::default(), Some(&NodeId::new("fluid_surface")));
    let target = RenderTarget::new(
        &harness.device,
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        "dam-break-authoring-regression",
    );
    let _live = PhysicsStepScope::for_render(false);
    // Preparation at zero precedes the first native tick that creates water.
    for frame in 0..=1 {
        let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new(format!("water frame {frame}"));
        loop {
            let mut encoder = harness.device.create_encoder("dam-break-initial-frame");
            let status = {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
                runtime.render(&mut gpu, &target.texture, &context(frame), &manifest);
                gpu.frame_status()
            };
            encoder.commit_and_wait_completed();
            assert!(
                !matches!(status, FrameRenderStatus::Failed(_)),
                "{status:?}"
            );
            let (_, outputs) = runtime.preview_scalar_io();
            if status == FrameRenderStatus::Complete
                && !runtime.warmup_pending()
                && (frame == 0
                    || outputs
                        .iter()
                        .any(|(port, value)| port == "vertex_count" && *value > 0.0))
            {
                break;
            }
            wait.hold();
        }
    }
    let (_, outputs) = runtime.preview_scalar_io();
    let initial_time = outputs
        .iter()
        .find(|(port, _)| port == "simulation_time")
        .unwrap()
        .1;
    // A display hitch creates two seconds of debt. A live worker must publish
    // intermediate progress instead of hiding every update until all debt drains.
    let mut progress = None;
    for _ in 0..1000 {
        let mut encoder = harness.device.create_encoder("dam-break-live-progress");
        let status = {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
            runtime.render(&mut gpu, &target.texture, &context(120), &manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        assert!(
            !matches!(status, FrameRenderStatus::Failed(_)),
            "{status:?}"
        );
        let (_, outputs) = runtime.preview_scalar_io();
        progress = outputs
            .iter()
            .find(|(port, value)| port == "simulation_time" && *value > initial_time)
            .map(|(_, value)| *value);
        if progress.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        progress.is_some_and(|time| time < 2.0),
        "no intermediate water surface: {progress:?}"
    );
}
