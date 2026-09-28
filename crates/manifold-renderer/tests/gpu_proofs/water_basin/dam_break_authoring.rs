//! Exercise the shipped Dam Break with its migrated, real parameter manifest.
use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::Param;
use manifold_core::{EffectId, NodeId};

#[test]
fn migrated_dam_break_defaults_publish_water_with_real_manifest() {
    let harness = harness::shared();
    let mut def: EffectGraphDef = serde_json::from_str(include_str!(
        "../../../assets/generator-presets/WaterDamBreak.json"
    ))
    .unwrap();
    manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
    // Scalar outputs without consumers are pruned. Keep the surface count live
    // through visibility, which is equivalent for any non-empty water mesh.
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 4,
            from_port: "vertex_count".into(),
            to_node: 9,
            to_port: "visible".into(),
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
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        Arc::clone(&harness.device),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        None,
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
    let _offline = PhysicsStepScope::for_render(true);
    for frame in 0..=3 {
        let mut completed = false;
        for _ in 0..200 {
            let mut encoder = harness.device.create_encoder("dam-break-authoring-frame");
            let status = {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, &harness.device);
                runtime.render(&mut gpu, &target.texture, &context(frame), &manifest);
                gpu.frame_status()
            };
            encoder.commit_and_wait_completed();
            assert!(
                !matches!(status, FrameRenderStatus::Failed(_)),
                "frame {frame}: {status:?}"
            );
            if status == FrameRenderStatus::Complete && !runtime.warmup_pending() {
                completed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            completed,
            "frame {frame} did not complete within bounded preparation"
        );
        if frame > 0 {
            let (_, outputs) = runtime.preview_scalar_io();
            let count = outputs
                .iter()
                .find(|(port, _)| port == "vertex_count")
                .map(|(_, value)| *value);
            assert!(
                count.is_some_and(|count| count > 0.0),
                "frame {frame}: {outputs:?}"
            );
        }
    }
    std::fs::write(
        "/tmp/manifold-dam-break-regression.png",
        readback_to_srgb_png(&harness.device, &target.texture, WIDTH, HEIGHT),
    )
    .unwrap();
}
