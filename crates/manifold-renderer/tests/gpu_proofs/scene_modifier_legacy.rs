//! Pixel parity for frozen v2 graphs and their v3 migration.
//! These synthetic material/coordinate fixtures qualify migration only;
//! playing real photoscans are covered by the application journey.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::scene_modifier_legacy_migration::migrate_legacy_scene_modifiers;
use manifold_renderer::preset_context::PresetContext;

// Frozen fixtures were captured for structure and intentionally lack lighting.
// Supply the same required PBR environment to both comparison inputs; keep
// the immutable files and all modifier controls/geometry unchanged.
fn illuminate(def: &mut EffectGraphDef) {
    let render = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .unwrap()
        .id;
    let env_id = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    def.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": env_id, "nodeId": "parity_environment", "typeId": "node.bake_environment",
            "params": {"width":{"type":"Int","value":128}, "height":{"type":"Int","value":64}}
        }))
        .unwrap(),
    );
    def.wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: env_id,
            from_port: "envmap".into(),
            to_node: render,
            to_port: "envmap".into(),
        });
}

fn render(def: EffectGraphDef, context: &PresetContext) -> Vec<u8> {
    let device = crate::harness::shared().device.clone();
    let mut runtime = manifold_renderer::preset_runtime::PresetRuntime::from_def_with_device(
        def,
        &PrimitiveRegistry::with_builtin(),
        device.clone(),
        256,
        256,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap();
    let target = manifold_renderer::render_target::RenderTarget::new(
        &device,
        256,
        256,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
        "modifier-migration-parity",
    );
    for frame in 0..2 {
        let mut encoder = device.create_encoder("modifier-migration-parity");
        let mut ctx = *context;
        ctx.frame_count = frame;
        runtime.render(
            &mut manifold_renderer::gpu_encoder::GpuEncoder::new(&mut encoder, &device),
            &target.texture,
            &ctx,
            &manifold_core::params::ParamManifest::default(),
        );
        encoder.commit_and_wait_completed();
        assert!(
            runtime.errors().is_empty(),
            "render diagnostics: {:?}",
            runtime.errors()
        );
    }
    manifold_renderer::headless_readback::readback_tonemapped_rgba8(
        &device,
        &target.texture,
        256,
        256,
    )
}

#[test]
fn scene_modifier_legacy_migration_preserves_rendered_frames() {
    let registry = PrimitiveRegistry::with_builtin();
    let output = std::path::Path::new("/tmp/unified-scene-modifier-parity");
    std::fs::create_dir_all(output).unwrap();
    let context = PresetContext {
        time: 0.625,
        beat: 1.25,
        dt: 1.0 / 60.0,
        width: 256,
        height: 256,
        output_width: 256,
        output_height: 256,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    for name in [
        "elastic_sculpture",
        "surface_peel",
        "vortex_fragments",
        "photoscan_stack",
        "scene_loop",
        "scene_fog",
        "scene_loop_scene_fog",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tests/fixtures/scene-modifiers/{name}_applied_v2.json"
        ));
        let mut before: EffectGraphDef =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        illuminate(&mut before);
        let mut after = before.clone();
        let report = migrate_legacy_scene_modifiers(&mut after, &registry);
        assert!(report.changed, "{name}: {:?}", report.diagnostics);
        let old = render(before, &context);
        let new = render(after, &context);
        assert!(
            old.chunks_exact(4).any(|pixel| pixel != &old[..4]),
            "{name}: uniform output is not geometry evidence"
        );
        for (suffix, pixels) in [("original", &old), ("migrated", &new)] {
            image::save_buffer(
                output.join(format!("{name}-{suffix}.png")),
                pixels,
                256,
                256,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        }
        let visible = old
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && pixel[..3].iter().any(|v| *v > 8))
            .count();
        assert!(
            visible > 32,
            "{name}: original render is empty ({visible} visible pixels)"
        );
        let max_delta = old
            .iter()
            .zip(&new)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        eprintln!("{name}: visible={visible}, maximum RGBA8 delta={max_delta}");
        assert_eq!(
            max_delta,
            0,
            "{name}: migration changed rendered output; see {}",
            output.display()
        );
    }
}
