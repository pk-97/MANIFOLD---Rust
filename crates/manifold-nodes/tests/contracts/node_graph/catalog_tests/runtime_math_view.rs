#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::exec::execution_plan::ResourceId;
use manifold_node_engine::persistence::PrimitiveRegistry;
#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::runtime::preset_context::PresetContext;
#[cfg(feature = "gpu-proofs")]
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_core::NodeId;
#[cfg(feature = "gpu-proofs")]
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;
#[cfg(feature = "gpu-proofs")]
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::*;
use manifold_core::effect_graph_def::BindingTarget;
fn owner() -> EffectGraphDef {
    let mut owner = crate::contracts::node_graph::catalog_tests::math_view_fixtures::test_owner();
    // Internal render-view preparation receives a canonical loaded document.
    manifold_core::phong_migration::migrate_phong_to_pbr(&mut owner);
    owner
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
fn math_view_runtime_adds_diagrams_to_the_one_scene_plan_and_routes_values() {
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let mut params = manifest(&owner);
    let plain = crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner);
    let count = |runtime: &PresetRuntime, type_id: &str| {
        runtime.plan.steps().iter().filter(|step| {
            runtime.graph.get_node(step.node).unwrap().node.type_id().as_str() == type_id
        }).count()
    };
    for fused in [false, true] {
        let baseline =
            PresetRuntime::from_def_for_render(plain.clone(), &registry, Some(&params), fused)
                .unwrap();
        let mut runtime =
            PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), fused)
                .unwrap();
        assert!(runtime.plan.steps().len() > baseline.plan.steps().len());
        assert!(count(&runtime, "node.render_mesh_diagram") > 0);
        assert_eq!(count(&baseline, "node.render_mesh_diagram"), 0);
        // The diagram reads the parent's mesh; the scene is never rendered twice.
        assert_eq!(
            count(&runtime, "node.render_scene"),
            count(&baseline, "node.render_scene"),
            "fused={fused}"
        );
        set(&owner, &mut params, "math_view_mode", 1.0);
        set(&owner, &mut params, "orbit", 0.73);
        runtime.apply_param_values(&params);
        let local = manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: vec![NodeId::new("vortex_stage")],
            node: NodeId::new("patch"),
        };
        let copies = runtime
            .modifier_node_copies(&NodeId::new("vortex_a"), &local)
            .unwrap();
        assert!(!copies.is_empty());
        for copy in copies {
            let (target, param) = manifold_node_engine::runtime::testkit::fused_retarget(&runtime, &copy.node_id, "orbit")
                .unwrap_or_else(|| (copy.node_id.clone(), "orbit".into()));
            let node = runtime.graph.instance_by_node_id(&target).unwrap();
            let value = runtime.graph.get_node(node).unwrap().params[param.as_str()]
                .as_scalar()
                .unwrap();
            assert!(
                (value - 0.73).abs() < 1e-6,
                "scoped Orbit binding diverged: {value}"
            );
        }
        set(&owner, &mut params, "math_view_mode", 0.0);
    }
    let restored: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&owner).unwrap()).unwrap();
    assert_eq!(restored, owner);
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_occlusion_applies_to_overlay_only_and_survives_resize() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let mut params = manifest(&owner);
    for (name, value) in [("mode", 1.0), ("grid", 0.0), ("trails", 0.0), ("density", 8.0)] {
        set(&owner, &mut params, &format!("math_view_{name}"), value);
    }
    set(&owner, &mut params, "orbit", 0.5);
    let energy = |pixels: &[u8]| -> f64 {
        pixels.chunks_exact(8).map(|pixel| {
            pixel[..6].chunks_exact(2).map(|channel| {
                f64::from(half::f16::from_le_bytes([channel[0], channel[1]]).to_f32())
            }).sum::<f64>()
        }).sum()
    };
    let mut standalone: Option<Vec<u8>> = None;
    for fused in [false, true] {
        let mut runtime = PresetRuntime::from_def_for_render(
            owner.clone(), &registry, Some(&params), fused,
        ).unwrap().with_generator_device(device.clone(), 320, 180, GpuTextureFormat::Rgba16Float).unwrap();
        for (w, h) in [(320, 180), (240, 160)] {
            if w != 320 { runtime.resize(&device, w, h).unwrap(); }
            let target = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "math-depth-runtime-proof");
            let render = |runtime: &mut PresetRuntime, params: &ParamManifest| {
                let ctx = PresetContext {
                    time: 0.0, beat: 0.0, dt: 0.0, width: w, height: h,
                    output_width: w, output_height: h, aspect: w as f32 / h as f32,
                    owner_key: 1, is_clip_level: false, frame_count: 1,
                    anim_progress: 0.0, trigger_count: 0,
                };
                let mut encoder = device.create_encoder("math-depth-runtime-proof");
                runtime.render(&mut GpuEncoder::new(&mut encoder, &device), &target.texture, &ctx, params);
                encoder.commit_and_wait_completed();
                manifold_node_engine::testkit::gpu::readback_raw_halves(&device, &target.texture, w, h)
            };
            // A wired mode selector lands one frame late; render once to settle.
            set(&owner, &mut params, "math_view_mode", 1.0);
            set(&owner, &mut params, "math_view_occlusion", 0.0);
            render(&mut runtime, &params);
            let xray = render(&mut runtime, &params);
            assert!(energy(&xray) > 1.0, "Math view must draw marks");
            set(&owner, &mut params, "math_view_occlusion", 1.0);
            assert_eq!(xray, render(&mut runtime, &params), "Math view never occludes: fused={fused}");
            set(&owner, &mut params, "math_view_occlusion", 0.0);
            set(&owner, &mut params, "math_view_mode", 2.0);
            render(&mut runtime, &params);
            let overlay_xray = render(&mut runtime, &params);
            set(&owner, &mut params, "math_view_occlusion", 1.0);
            let overlay_depth = render(&mut runtime, &params);
            assert!(energy(&overlay_depth) < energy(&overlay_xray), "Depth must occlude Overlay marks behind the scene");
            if w == 320 {
                if fused {
                    assert!(manifold_node_engine::testkit::gpu::mean_abs_half_diff(
                        standalone.as_ref().unwrap(), &overlay_depth,
                    ) < 0.002, "fused Depth diverged");
                } else {
                    standalone = Some(overlay_depth);
                    if let Ok(path) = std::env::var("MANIFOLD_MATH_DEPTH_PREVIEW") {
                        std::fs::write(path, manifold_node_engine::testkit::gpu::readback_to_srgb_png_linear(
                            &device, &target.texture, w, h,
                        )).unwrap();
                    }
                }
            }
        }
    }
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_native_scene_parity_orbit_change_and_overlay() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let mut params = manifest(&owner);
    let plain = crate::contracts::node_graph::catalog_tests::math_view_fixtures::without_view(&owner);
    let mut baseline = PresetRuntime::from_def_for_render(plain, &registry, Some(&params), false)
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
    let render_at =
        |runtime: &mut PresetRuntime, params: &ParamManifest, frame: i64, trigger_count: u32| {
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
                trigger_count,
            };
            let mut encoder = device.create_encoder("math-view-proof");
            {
                let mut gpu = GpuEncoder::new(&mut encoder, &device);
                runtime.render(&mut gpu, &target.texture, &ctx, params);
            }
            encoder.commit_and_wait_completed();
            manifold_node_engine::testkit::gpu::readback_raw_halves(&device, &target.texture, W, H)
        };
    let render = |runtime: &mut PresetRuntime, params: &ParamManifest, frame: i64| {
        render_at(runtime, params, frame, 0)
    };
    let black = |pixels: &[u8]| pixels.chunks_exact(8).all(|pixel| pixel[..6] == [0; 6]);
    // The wired mode selector lands one frame late: each mode change renders
    // one settling frame before its result is read.
    let expected = render(&mut baseline, &params, 1);
    let scene = render(&mut runtime, &params, 1);
    assert_eq!(scene, expected, "Scene mode changed the existing renderer");
    set(&owner, &mut params, "math_view_mode", 1.0);
    set(&owner, &mut params, "math_view_trails", 0.0);
    set(&owner, &mut params, "orbit", 0.0);
    render(&mut runtime, &params, 2);
    let initial = render(&mut runtime, &params, 2);
    set(&owner, &mut params, "orbit", 2.2);
    let moved = render(&mut runtime, &params, 3);
    assert_ne!(initial, moved, "Orbit must move the sampled authored graph");
    assert_ne!(scene, moved, "Math must replace the scene");

    // Modifier controls are public writes. Brightness is per diagram element.
    for control in ["fragments", "ghosts", "vectors", "trails"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 0.0);
    }
    set(&owner, &mut params, "math_view_grid", 1.0);
    set(&owner, &mut params, "math_view_grid_brightness", 0.0);
    assert!(
        black(&render(&mut runtime, &params, 6)),
        "Grid brightness 0 must hide Grid"
    );
    set(&owner, &mut params, "math_view_grid_brightness", 1.0);
    assert!(
        !black(&render(&mut runtime, &params, 7)),
        "Grid brightness must be independent"
    );
    set(&owner, &mut params, "math_view_grid", 0.0);
    set(&owner, &mut params, "math_view_fragments", 1.0);
    set(&owner, &mut params, "math_view_mode", 0.0);
    render(&mut runtime, &params, 9);
    let neutral_scene = render(&mut runtime, &params, 9);
    assert!(
        !black(&neutral_scene),
        "neutral Scene fixture must be visible"
    );
    assert_eq!(
        neutral_scene,
        render(&mut baseline, &params, 9),
        "Scene mode must preserve Scene output"
    );

    // Restore the original moved-frame controls before the pre-existing
    // standalone/fused parity proof below.
    for control in ["grid", "fragments", "ghosts", "vectors"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 1.0);
    }
    set(&owner, &mut params, "math_view_trails", 0.0);
    set(&owner, &mut params, "math_view_grid_brightness", 1.0);
    set(&owner, &mut params, "math_view_mode", 1.0);
    render(&mut runtime, &params, 3);
    runtime.clear_state();
    let moved_restored = render(&mut runtime, &params, 3);

    let rgba = manifold_node_engine::testkit::gpu::readback_srgb_rgba8(&device, &target.texture, W, H);
    assert!(
        rgba.chunks_exact(4)
            .filter(|pixel| pixel[..3].iter().any(|channel| *channel > 30))
            .count()
            > 100
    );
    if let Ok(path) = std::env::var("MANIFOLD_MATH_VIEW_PREVIEW") {
        std::fs::write(
            path,
            manifold_node_engine::testkit::gpu::encode_rgba8_png(&rgba, W, H),
        )
        .unwrap();
    }
    set(&owner, &mut params, "math_view_mode", 2.0);
    render(&mut runtime, &params, 4);
    let overlay = render(&mut runtime, &params, 4);
    assert_ne!(overlay, moved);
    assert_ne!(overlay, scene);
    set(&owner, &mut params, "math_view_mode", 1.0);
    render(&mut runtime, &params, 4);
    let mut fused =
        PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), true)
            .unwrap()
            .with_generator_device(device.clone(), W, H, GpuTextureFormat::Rgba16Float)
            .unwrap();
    let fused_image = render(&mut fused, &params, 4);
    assert!(
        manifold_node_engine::testkit::gpu::mean_abs_half_diff(&moved_restored, &fused_image) < 0.002,
        "fused and standalone authored evaluations diverged"
    );
    for runtime in [&mut runtime, &mut fused] {
        set(&owner, &mut params, "math_view_axes", 0.0);
        let no_axes = render(runtime, &params, 4);
        assert!(!black(&no_axes), "Axes Off must retain the other diagram marks");
        assert_ne!(no_axes, moved_restored, "Axes control must reach the renderer");
        set(&owner, &mut params, "math_view_axes", 1.0);
        assert!(manifold_node_engine::testkit::gpu::mean_abs_half_diff(
            &moved_restored, &render(runtime, &params, 4)) < 0.002,
            "Axes On must restore the original marks");
    }

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
    assert!(!black(&with_history), "fixture must have visible motion history");
    set(&owner, &mut params, "math_view_trails", 0.0);
    render(&mut runtime, &params, 14);
    set(&owner, &mut params, "phase", 0.9);
    render(&mut runtime, &params, 15);
    set(&owner, &mut params, "math_view_trails", 1.0);
    assert!(black(&render(&mut runtime, &params, 16)),
        "first Trails On frame must not replay pre-toggle history");
    set(&owner, &mut params, "phase", 1.1);
    render(&mut runtime, &params, 17);
    set(&owner, &mut params, "phase", 1.3);
    assert!(!black(&render(&mut runtime, &params, 18)),
        "fresh motion history must resume after re-enabling trails");
    runtime.clear_state();
    let cleared = render(&mut runtime, &params, 19);
    assert_ne!(
        with_history, cleared,
        "clear_state must remove GPU motion history"
    );
    runtime.clear_state();
    set(&owner, &mut params, "math_view_mode", 0.0);
    render(&mut runtime, &params, 5);
    assert_eq!(
        render(&mut runtime, &params, 5),
        render(&mut baseline, &params, 5)
    );

    // Exercise the full macro -> compiled multi-object graph -> primitive
    // path. Only the first diagram has a Grid wire; the second has Bool(false).
    // The final image has opaque alpha, so blankness concerns RGB only.
    assert_eq!(owner.scene_modifiers[1].mesh_frames.len(), 2);
    set(&owner, &mut params, "math_view_mode", 1.0);
    for control in ["grid", "fragments", "ghosts", "vectors", "trails"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 0.0);
    }
    for (name, runtime) in [("standalone", &mut runtime), ("fused", &mut fused)] {
        render(runtime, &params, 20);
        assert!(
            black(&render(runtime, &params, 20)),
            "{name}: all marks off must be black"
        );
        set(&owner, &mut params, "math_view_grid", 1.0);
        assert!(
            !black(&render(runtime, &params, 21)),
            "{name}: scalar Grid on must override Bool(false)"
        );
        set(&owner, &mut params, "math_view_grid", 0.0);
        assert!(
            black(&render(runtime, &params, 22)),
            "{name}: Grid off must clear every object diagram"
        );
    }
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_resize_rejection_preserves_live_resources_at_every_allocation() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let params = manifest(&owner);
    let mut runtime = PresetRuntime::from_def_for_render(owner, &registry, Some(&params), false)
        .unwrap().with_generator_device(device.clone(), 64, 48, GpuTextureFormat::Rgba16Float).unwrap();
    let arrays = || {
        let backend = runtime.backend_for_test();
        (0..runtime.plan.resource_count()).filter_map(|id| {
            let id = ResourceId(id as u32);
            backend.slot_for(id).and_then(|slot| backend.array_buffer(slot)).map(|buffer| (id, buffer.clone()))
        }).collect::<Vec<_>>()
    };
    let original = arrays();
    assert!(!original.is_empty());
    let mut successes = 0;
    let mut failures = 0;
    for stage in 0..64 {
        let injection = manifold_node_engine::gpu::render_target::fail_allocation_after(stage);
        let result = runtime.prepare_resize(&device, 48, 64);
        drop(injection);
        assert_eq!((runtime.width_for_test(), runtime.height_for_test()), (64, 48));
        for (id, buffer) in &original {
            let backend = runtime.backend_for_test();
            assert!(backend.array_buffer(backend.slot_for(*id).unwrap()).unwrap().ptr_eq(buffer));
        }
        match result {
            Err(error) => { assert!(error.to_string().contains("injected GPU allocation failure"), "{error}"); failures += 1; }
            Ok(prepared) => { runtime.commit_resize(prepared); successes += 1; break; }
        }
    }
    assert!(failures > 0, "must exercise an allocation failure");
    assert_eq!(successes, 1, "bounded preparation must eventually succeed");
    assert_eq!((runtime.width_for_test(), runtime.height_for_test()), (48, 64));
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn staged_transform_resize_keeps_compatible_chain_and_renders() {
    let device = manifold_gpu::testkit::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let effects = vec![manifold_core::preset_definition_registry::create_default(&PresetTypeId::new("ColorGrade"))];
    let mut runtime = PresetRuntime::try_build(ChainBuildInputs {
        effects: &effects, groups: &[], primitives: &primitives, device: &device,
        pool: None, width: 32, height: 24, preview_effect: None,
    }, None).unwrap();
    let prepared = runtime.prepare_resize(&device, 24, 32).unwrap();
    runtime.commit_resize(prepared);
    assert!(runtime.is_compatible(&effects, &[], 24, 32, None));
    assert!(!runtime.is_compatible(&effects, &[], 32, 24, None));
    let input = RenderTarget::new(&device, 24, 32, GpuTextureFormat::Rgba16Float, "resize-chain-input");
    let ctx = PresetContext {
        time: 0.0, beat: 0.0, dt: 0.0, width: 24, height: 32,
        output_width: 24, output_height: 32, aspect: 0.75,
        owner_key: 1, is_clip_level: false, frame_count: 1,
        anim_progress: 0.0, trigger_count: 0,
    };
    let mut encoder = device.create_encoder("resize-chain-proof");
    encoder.clear_texture(&input.texture, 0.4, 0.2, 0.1, 1.0);
    let output = runtime.run(&mut GpuEncoder::new(&mut encoder, &device), &input.texture, &effects, &[], &ctx).unwrap().clone();
    encoder.try_commit_and_wait_completed().unwrap();
    assert_eq!((output.width, output.height), (24, 32));
    let pixels = manifold_node_engine::testkit::gpu::readback_raw_halves(&device, &output, 24, 32);
    assert!(pixels.chunks_exact(8).any(|pixel| pixel[..6].chunks_exact(2)
        .any(|channel| half::f16::from_le_bytes([channel[0], channel[1]]).to_f32() > 0.01)));
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_instance_echoes_render_copies_and_vertices_only_path_is_unchanged() {
    // BUG-uvts GPU proof: Vortex + SpatialEchoes -> Math View must render the
    // echo copies, and the vertices-only fixture must stay byte-identical to
    // its pre-instances behavior (unwired instances port).
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let echo_owner = crate::contracts::node_graph::catalog_tests::math_view_fixtures::test_owner_with_instance_echoes();
    let plain_owner = crate::contracts::node_graph::catalog_tests::math_view_fixtures::test_owner();
    let registry = PrimitiveRegistry::with_builtin();
    let render_math = |owner: &EffectGraphDef| {
        let mut params = manifest(owner);
        set(owner, &mut params, "math_view_mode", 1.0);
        set(owner, &mut params, "math_view_trails", 0.0);
        set(owner, &mut params, "math_view_grid", 0.0);
        let mut runtime = PresetRuntime::from_def_for_render(
            owner.clone(),
            &registry,
            Some(&params),
            false,
        )
        .unwrap()
        .with_generator_device(device.clone(), 320, 180, GpuTextureFormat::Rgba16Float)
        .unwrap();
        let target = RenderTarget::new(
            &device,
            320,
            180,
            GpuTextureFormat::Rgba16Float,
            "math-instance-copies-proof",
        );
        let ctx = PresetContext {
            time: 1.0 / 60.0,
            beat: 1.0 / 30.0,
            dt: 1.0 / 60.0,
            width: 320,
            height: 180,
            output_width: 320,
            output_height: 180,
            aspect: 320.0 / 180.0,
            owner_key: 1,
            is_clip_level: false,
            frame_count: 1,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = device.create_encoder("math-instance-copies-proof");
        runtime.render(
            &mut GpuEncoder::new(&mut encoder, &device),
            &target.texture,
            &ctx,
            &params,
        );
        encoder.commit_and_wait_completed();
        let pixels = manifold_node_engine::testkit::gpu::readback_raw_halves(&device, &target.texture, 320, 180);
        if std::env::var_os("MANIFOLD_MATH_ECHOES_PREVIEW").is_some() {
            // Encode the live target before the closure drops it.
            std::fs::write(
                std::env::var_os("MANIFOLD_MATH_ECHOES_PREVIEW").unwrap(),
                manifold_node_engine::testkit::gpu::readback_to_srgb_png_linear(
                    &device,
                    &target.texture,
                    320,
                    180,
                ),
            )
            .unwrap();
        }
        pixels
    };
    let black = |pixels: &[u8]| pixels.chunks_exact(8).all(|pixel| pixel[..6] == [0; 6]);
    let energy = |pixels: &[u8]| -> f64 {
        pixels.chunks_exact(8).map(|pixel| {
            pixel[..6].chunks_exact(2).map(|channel| {
                f64::from(half::f16::from_le_bytes([channel[0], channel[1]]).to_f32())
            }).sum::<f64>()
        }).sum()
    };

    let plain = render_math(&plain_owner);
    assert!(!black(&plain), "vertices-only Math view must render marks");
    let echoes = render_math(&echo_owner);
    assert!(!black(&echoes), "echo Math view must render marks");
    assert_ne!(
        plain, echoes,
        "preceding instance echoes must change the Math view output"
    );
    // SpatialEchoes defaults to 3 echoes around the base object, so the copy
    // marks add real energy rather than replacing the base diagram.
    assert!(
        energy(&echoes) > energy(&plain),
        "echo copies must add visible marks: {} vs {}",
        energy(&echoes),
        energy(&plain)
    );
}

/// A saved generator whose card drives a retired param loads on the
/// scene-modifier path: the authored graph its value writes read keeps
/// nothing the render graph dropped (a saved GPU FLIP layer's
/// `step.top_speed` card refused the whole layer).
#[test]
fn retired_params_leave_before_scene_modifier_value_writes() {
    fn find_step(nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode]) -> Option<&mut manifold_core::effect_graph_def::EffectGraphNode> {
        for node in nodes {
            if node.type_id == "node.gpu_flip_step" {
                return Some(node);
            }
            if let Some(found) = node.group.as_mut().and_then(|group| find_step(&mut group.nodes)) {
                return Some(found);
            }
        }
        None
    }
    let mut def: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/generator-presets/WaterDamBreakGpuFlip.json"
    )))
    .unwrap();
    let step = find_step(&mut def.nodes).expect("the Dam Break has a GPU FLIP step");
    step.params.insert("top_speed".into(), manifold_core::effect_graph_def::SerializedParamValue::Float { value: 23.0 });
    let step_id = step.node_id.clone();
    let metadata = def.preset_metadata.as_mut().unwrap();
    let mut binding = metadata.bindings[0].clone();
    let mut param = metadata.params.iter().find(|param| param.id == binding.id).unwrap().clone();
    binding.id = "speed_cap".into();
    binding.target = BindingTarget::Node { node_id: step_id, param: "top_speed".into() };
    param.id = "speed_cap".into();
    metadata.bindings.push(binding);
    metadata.params.push(param);
    // A Uniform Force on every object, as the saved layer carries one.
    let recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/UniformForce.json"
    )))
    .unwrap();
    let scene = manifold_core::scene_modifier_preset::SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
    let force = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        &def,
        &recipe,
        NodeId::new("force"),
        scene,
        manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
    )
    .unwrap();
    let def = manifold_core::scene_modifier_edit::insert_scene_modifier(&def, 0, force).unwrap().graph;
    assert!(manifold_node_engine::load::graph_loader::has_retired_params(&def));
    PresetRuntime::from_def_for_render(def, &PrimitiveRegistry::with_builtin(), None, false)
        .expect("the retired card leaves at load and the layer builds");
}

/// Each mesh modifier before Math View. Fragment presets cut the mesh into a
/// padded layout; Math View must align its reference through the same cut
/// map so arrows still draw. Arrow energy is Vectors on minus Vectors off; the
/// topology guard hides arrows, so a positive difference means it did not
/// fire. SpatialEchoes moves copies, not vertices: it only must not fail.
#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_arrows_draw_after_every_mesh_modifier() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let registry = PrimitiveRegistry::with_builtin();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/");
    let energy = |pixels: &[u8]| -> f64 {
        pixels.chunks_exact(8).map(|pixel| {
            pixel[..6].chunks_exact(2).map(|channel| {
                f64::from(half::f16::from_le_bytes([channel[0], channel[1]]).to_f32())
            }).sum::<f64>()
        }).sum()
    };
    let (w, h) = (320, 180);
    for (name, must_draw_arrows) in [
        ("VortexFragments", true), ("SurfacePeel", true), ("MaskedPeel", true),
        ("OrderedRecon", true), ("SurfaceWaves", true), ("ElasticSculpture", true),
        ("SpatialEchoes", false),
    ] {
        let mut owner = owner();
        let recipe: EffectGraphDef = serde_json::from_str(
            &std::fs::read_to_string(format!("{dir}{name}.json")).unwrap(),
        ).unwrap();
        *owner.scene_modifiers[0].graph = recipe;
        let owner = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            &owner, &NodeId::new("vortex_a"),
        ).unwrap().graph;
        let mut params = manifest(&owner);
        for (control, value) in [("mode", 1.0), ("grid", 0.0), ("trails", 0.0), ("ghosts", 0.0), ("density", 8.0)] {
            set(&owner, &mut params, &format!("math_view_{control}"), value);
        }
        let mut runtime = PresetRuntime::from_def_for_render(owner.clone(), &registry, Some(&params), false)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"))
            .with_generator_device(device.clone(), w, h, GpuTextureFormat::Rgba16Float)
            .unwrap();
        let target = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "math-arrows");
        let mut frame = 0_u32;
        let mut render = |runtime: &mut PresetRuntime, params: &ParamManifest| {
            frame += 1;
            let ctx = PresetContext {
                time: f64::from(frame) * 0.5, beat: f64::from(frame), dt: 0.5, width: w, height: h,
                output_width: w, output_height: h, aspect: w as f32 / h as f32,
                owner_key: 1, is_clip_level: false, frame_count: i64::from(frame),
                anim_progress: 0.0, trigger_count: 0,
            };
            let mut encoder = device.create_encoder("math-arrows");
            runtime.render(&mut GpuEncoder::new(&mut encoder, &device), &target.texture, &ctx, params);
            encoder.commit_and_wait_completed();
            manifold_node_engine::testkit::gpu::readback_raw_halves(&device, &target.texture, w, h)
        };
        // Wired controls land one frame late; render twice per setting.
        set(&owner, &mut params, "math_view_vectors", 1.0);
        render(&mut runtime, &params);
        let with_vectors = energy(&render(&mut runtime, &params));
        set(&owner, &mut params, "math_view_vectors", 0.0);
        render(&mut runtime, &params);
        let without_vectors = energy(&render(&mut runtime, &params));
        assert!(without_vectors > 1.0, "{name}: Math view must draw fragment marks");
        if must_draw_arrows {
            assert!(
                with_vectors > without_vectors + 1.0,
                "{name}: arrows missing (Vectors on {with_vectors}, off {without_vectors})"
            );
        }
    }
}
