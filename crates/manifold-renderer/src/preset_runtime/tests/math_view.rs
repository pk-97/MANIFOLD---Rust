use super::*;
use manifold_core::effect_graph_def::BindingTarget;
fn owner() -> EffectGraphDef {
    crate::node_graph::scene_modifier_expand::math_view_test_owner()
}

fn manifest(owner: &EffectGraphDef) -> ParamManifest {
    ParamManifest::from_params(
        owner
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .cloned()
            .map(manifold_core::params::Param::bundled)
            .collect(),
    )
}

fn set(owner: &EffectGraphDef, manifest: &mut ParamManifest, local: &str, value: f32) {
    let binding = owner.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding|
        matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == local)
    ).unwrap();
    let param = manifest.get_mut(&binding.id).unwrap();
    param.value = value;
    param.base = value;
}

#[test]
fn math_view_runtime_keeps_scene_plan_and_routes_values_to_bounded_variants() {
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let mut params = manifest(&owner);
    for fused in [false, true] {
        let baseline = PresetRuntime::from_def_for_render_view(
            owner.clone(),
            &registry,
            Some(&params),
            fused,
            None,
        )
        .unwrap();
        let mut runtime =
            PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), fused)
                .unwrap();
        assert_eq!(runtime.plan.steps().len(), baseline.plan.steps().len());
        assert_eq!(runtime.math_views.len(), 1);
        assert_eq!(runtime.math_views[0].mode(&runtime.graph), 0);
        set(&owner, &mut params, "math_view_mode", 1.0);
        set(&owner, &mut params, "orbit", 0.73);
        runtime.apply_param_values(&params);
        assert_eq!(runtime.math_views[0].mode(&runtime.graph), 1);
        for variant in &mut runtime.math_views[0].variants {
            variant.apply_param_values(&params);
            let local = manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: vec![NodeId::new("vortex_stage")],
                node: NodeId::new("patch"),
            };
            let copies = variant
                .modifier_node_copies(&NodeId::new("vortex_math_view"), &local)
                .unwrap();
            for copy in copies {
                let (target, param) = variant.effect_nodes[0]
                    .bound
                    .fused_retarget
                    .get(&(copy.node_id.to_string(), "orbit".into()))
                    .cloned()
                    .unwrap_or_else(|| (copy.node_id.clone(), "orbit".into()));
                let node = variant.graph.instance_by_node_id(&target).unwrap();
                let value = variant.graph.get_node(node).unwrap().params[param.as_str()]
                    .as_scalar()
                    .unwrap();
                assert!(
                    (value - 0.73).abs() < 1e-6,
                    "scoped Orbit binding diverged: {value}"
                );
            }
            let usage = variant
                .prepared_modifier_buffer_usage((320, 180))
                .unwrap()
                .unwrap();
            assert!(
                usage.candidate_bytes > 0 && usage.candidate_bytes < 4 * 1024 * 1024,
                "{usage:?}"
            );
            assert!(variant.plan.steps().iter().all(|step| {
                let node = variant.graph.get_node(step.node).unwrap();
                !matches!(
                    node.node.type_id().as_str(),
                    "node.render_scene" | "node.cube_mesh" | "node.gltf_mesh_source"
                )
            }));
        }
        set(&owner, &mut params, "math_view_mode", 0.0);
    }
    let restored: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&owner).unwrap()).unwrap();
    assert_eq!(restored, owner);
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_native_scene_parity_orbit_change_and_overlay() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = crate::test_device();
    let device = guard.arc();
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let mut params = manifest(&owner);
    let mut baseline = PresetRuntime::from_def_for_render_view(
        owner.clone(),
        &registry,
        Some(&params),
        false,
        None,
    )
    .unwrap()
    .with_generator_device(device.clone(), W, H, GpuTextureFormat::Rgba16Float)
    .unwrap();
    let mut runtime =
        PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), false)
            .unwrap()
            .with_generator_device(device.clone(), W, H, GpuTextureFormat::Rgba16Float)
            .unwrap();
    let target = RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "math-view-proof",
    );
    let render = |runtime: &mut PresetRuntime, params: &ParamManifest, frame: i64| {
        let ctx = PresetContext {
            time: frame as f64 / 60.0,
            beat: frame as f64 / 30.0,
            dt: 1.0 / 60.0,
            width: W,
            height: H,
            output_width: W,
            output_height: H,
            aspect: W as f32 / H as f32,
            owner_key: 1,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = device.create_encoder("math-view-proof");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, params);
        }
        encoder.commit_and_wait_completed();
        crate::headless_readback::readback_raw_halves(&device, &target.texture, W, H)
    };
    let expected = render(&mut baseline, &params, 1);
    let scene = render(&mut runtime, &params, 1);
    assert_eq!(scene, expected, "Scene mode changed the existing renderer");
    set(&owner, &mut params, "math_view_mode", 1.0);
    set(&owner, &mut params, "math_view_trails", 0.0);
    set(&owner, &mut params, "orbit", 0.0);
    let initial = render(&mut runtime, &params, 2);
    set(&owner, &mut params, "orbit", 2.2);
    let moved = render(&mut runtime, &params, 3);
    assert_ne!(initial, moved, "Orbit must move the sampled authored graph");
    assert_ne!(scene, moved, "Math must replace the scene");

    let rgba = crate::headless_readback::readback_srgb_rgba8(&device, &target.texture, W, H);
    assert!(
        rgba.chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().any(|channel| *channel > 30))
            .count()
            > 100
    );
    if let Ok(path) = std::env::var("MANIFOLD_MATH_VIEW_PREVIEW") {
        std::fs::write(
            path,
            crate::headless_readback::encode_rgba8_png(&rgba, W, H),
        )
        .unwrap();
    }
    set(&owner, &mut params, "math_view_mode", 2.0);
    let overlay = render(&mut runtime, &params, 4);
    assert_ne!(overlay, moved);
    assert_ne!(overlay, scene);
    set(&owner, &mut params, "math_view_mode", 1.0);
    let mut fused =
        PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), true)
            .unwrap()
            .with_generator_device(device.clone(), W, H, GpuTextureFormat::Rgba16Float)
            .unwrap();
    let fused_image = render(&mut fused, &params, 4);
    assert!(
        crate::headless_readback::mean_abs_half_diff(&moved, &fused_image) < 0.002,
        "fused and standalone authored evaluations diverged"
    );

    // Isolate temporal history from the other diagram marks. Captured motion
    // must disappear after the same clear used for transport discontinuities.
    for control in ["grid", "fragments", "ghosts", "vectors"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 0.0);
    }
    set(&owner, &mut params, "math_view_trails", 1.0);
    for frame in 10..=12 {
        set(&owner, &mut params, "phase", (frame - 10) as f32 * 0.2);
        render(&mut runtime, &params, frame);
    }
    let with_history = render(&mut runtime, &params, 13);
    runtime.clear_state();
    let cleared = render(&mut runtime, &params, 14);
    assert_ne!(
        with_history, cleared,
        "clear_state must remove GPU motion history"
    );
    runtime.clear_state();
    set(&owner, &mut params, "math_view_mode", 0.0);
    assert_eq!(
        render(&mut runtime, &params, 5),
        render(&mut baseline, &params, 5)
    );

    // Exercise the full macro -> compiled multi-object graph -> primitive
    // path. Only the first diagram has a Grid wire; the second has Bool(false).
    // The final image has opaque alpha, so blankness concerns RGB only.
    assert_eq!(owner.scene_modifiers[0].mesh_frames.len(), 2);
    set(&owner, &mut params, "math_view_mode", 1.0);
    for control in ["grid", "fragments", "ghosts", "vectors", "trails"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 0.0);
    }
    let black = |pixels: &[u8]| pixels.chunks_exact(8).all(|pixel| pixel[..6] == [0; 6]);
    for (name, runtime) in [("standalone", &mut runtime), ("fused", &mut fused)] {
        assert!(black(&render(runtime, &params, 20)), "{name}: all marks off must be black");
        set(&owner, &mut params, "math_view_grid", 1.0);
        assert!(!black(&render(runtime, &params, 21)), "{name}: scalar Grid on must override Bool(false)");
        set(&owner, &mut params, "math_view_grid", 0.0);
        assert!(black(&render(runtime, &params, 22)), "{name}: Grid off must clear every object diagram");
    }

}
