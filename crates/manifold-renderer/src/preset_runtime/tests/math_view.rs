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
        let variant = &mut runtime.math_views[0].variant;
        variant.apply_param_values(&params);
        let local = manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: vec![NodeId::new("vortex_stage")],
            node: NodeId::new("patch"),
        };
        let copies = variant
            .modifier_node_copies(&NodeId::new("vortex_a"), &local)
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
        set(&owner, &mut params, "math_view_mode", 0.0);
    }
    let restored: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&owner).unwrap()).unwrap();
    assert_eq!(restored, owner);
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_depth_modes_borrow_scene_depth_and_survive_resize() {
    let guard = crate::test_device();
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
                crate::headless_readback::readback_raw_halves(&device, &target.texture, w, h)
            };
            set(&owner, &mut params, "math_view_mode", 1.0);
            set(&owner, &mut params, "math_view_occlusion", 0.0);
            let xray = render(&mut runtime, &params);
            set(&owner, &mut params, "math_view_occlusion", 1.0);
            let depth = render(&mut runtime, &params);
            assert!(energy(&depth) > 1.0, "Depth must not blank Math view");
            assert!(energy(&depth) < energy(&xray), "Depth must hide rear marks: fused={fused}");
            set(&owner, &mut params, "math_view_occlusion", 0.0);
            assert_eq!(xray, render(&mut runtime, &params), "X-ray must restore the original image");
            set(&owner, &mut params, "math_view_mode", 2.0);
            let overlay_xray = render(&mut runtime, &params);
            set(&owner, &mut params, "math_view_occlusion", 1.0);
            let overlay_depth = render(&mut runtime, &params);
            assert!(energy(&overlay_depth) < energy(&overlay_xray), "Depth must occlude Overlay marks");
            let view = &runtime.math_views[0];
            let variant = &view.variant;
            for &(source, destination) in &view.shared_depth {
                let parent = runtime.executor.backend();
                let child = variant.executor.backend();
                let original = parent.texture_2d(parent.slot_for(source).unwrap()).unwrap();
                let borrowed = child.texture_2d(child.slot_for(destination).unwrap()).unwrap();
                assert!(original.ptr_eq(borrowed), "view must borrow the retained scene depth");
                assert_eq!((borrowed.width, borrowed.height), (w, h));
                assert_eq!(borrowed.format, GpuTextureFormat::R32Float);
            }
            if w == 320 {
                if fused {
                    assert!(crate::headless_readback::mean_abs_half_diff(
                        standalone.as_ref().unwrap(), &overlay_depth,
                    ) < 0.002, "fused Depth diverged");
                } else {
                    standalone = Some(overlay_depth);
                    if let Ok(path) = std::env::var("MANIFOLD_MATH_DEPTH_PREVIEW") {
                        std::fs::write(path, crate::headless_readback::readback_to_srgb_png_linear(
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
    let view_shared = &runtime.math_views[0].variant.shared_arrays;
    assert!(!view_shared.is_empty());
    for (_, retained) in view_shared.iter() {
        assert!(
            runtime
                .plan
                .steps()
                .iter()
                .flat_map(|step| &step.outputs)
                .any(|(_, resource)| runtime
                    .executor
                    .backend()
                    .slot_for(*resource)
                    .and_then(|slot| runtime.executor.backend().array_buffer(slot))
                    .is_some_and(|parent| parent.ptr_eq(retained))),
            "borrowed storage must be the parent's buffer"
        );
    }
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
            crate::headless_readback::readback_raw_halves(&device, &target.texture, W, H)
        };
    let render = |runtime: &mut PresetRuntime, params: &ParamManifest, frame: i64| {
        render_at(runtime, params, frame, 0)
    };
    let black = |pixels: &[u8]| pixels.chunks_exact(8).all(|pixel| pixel[..6] == [0; 6]);
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

    // Modifier controls are public writes. Brightness is per diagram element,
    // while pulse and mesh connection affect the composed visibility path.
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
    set(&owner, &mut params, "math_view_pulse", 1.0);
    set(&owner, &mut params, "math_view_pulse_strength", -1.0);
    set(&owner, &mut params, "math_view_pulse_target", 0.0);
    assert!(
        black(&render(&mut runtime, &params, 8)),
        "negative pulse strength at pulse 1 must be zero"
    );
    set(&owner, &mut params, "math_view_pulse", 0.0);
    set(&owner, &mut params, "math_view_pulse_strength", 1.0);

    set(&owner, &mut params, "math_view_grid", 0.0);
    set(&owner, &mut params, "math_view_fragments", 1.0);
    set(&owner, &mut params, "math_view_mode", 0.0);
    set(&owner, &mut params, "math_view_connect_mesh", 0.0);
    let neutral_scene = render(&mut runtime, &params, 9);
    assert!(
        !black(&neutral_scene),
        "neutral Scene fixture must be visible"
    );
    assert_eq!(
        neutral_scene,
        render(&mut baseline, &params, 9),
        "disconnecting Math View must preserve Scene output"
    );
    set(&owner, &mut params, "math_view_pulse", 1.0);
    set(&owner, &mut params, "math_view_pulse_strength", -1.0);
    assert_eq!(
        render(&mut runtime, &params, 9),
        neutral_scene,
        "graphics-only pulse must leave the actual scene visible"
    );
    set(&owner, &mut params, "math_view_mode", 1.0);
    assert!(
        black(&render(&mut runtime, &params, 9)),
        "graphics-only pulse must hide the marks"
    );
    set(&owner, &mut params, "math_view_connect_mesh", 1.0);
    for mode in [0.0, 1.0, 2.0] {
        set(&owner, &mut params, "math_view_mode", mode);
        assert!(
            black(&render(&mut runtime, &params, 9)),
            "connected pulse must hide every presentation"
        );
    }
    set(&owner, &mut params, "math_view_pulse", 0.0);
    set(&owner, &mut params, "math_view_pulse_strength", 1.0);
    set(&owner, &mut params, "math_view_scan_mode", 1.0);
    set(&owner, &mut params, "math_view_scan_amount", 1.0);
    set(&owner, &mut params, "math_view_scan_progress", 0.0);
    set(&owner, &mut params, "math_view_scan_target", 0.0);
    for (mode, label) in [(0.0, "Scene"), (1.0, "Math"), (2.0, "Overlay")] {
        runtime.clear_state();
        set(&owner, &mut params, "math_view_mode", mode);
        assert!(
            black(&render(&mut runtime, &params, 10)),
            "connected reveal start must hide {label} mesh and diagram"
        );
        set(&owner, &mut params, "math_view_scan_progress", 1.0);
        assert!(
            !black(&render(&mut runtime, &params, 11)),
            "connected reveal endpoint must restore {label} mesh and diagram"
        );
        set(&owner, &mut params, "math_view_scan_progress", 0.0);
    }
    set(&owner, &mut params, "math_view_scan_progress", 0.5);
    render(&mut runtime, &params, 12);
    for resources in runtime.math_views[0].variant
        .shared_arrays
        .chunks_exact(2)
    {
        let weights = &resources[1].1;
        // Read only after render's commit-and-wait, never from a live frame.
        let values = unsafe {
            std::slice::from_raw_parts(
                weights.mapped_ptr().unwrap() as *const f32,
                weights.size as usize / 4,
            )
        };
        assert!(
            values.contains(&0.0) && values.contains(&1.0),
            "mid-scan must select some real faces and hide others"
        );
        assert!(
            values
                .chunks_exact(3)
                .all(|face| face[0] == face[1] && face[1] == face[2]),
            "selection must retain complete real faces"
        );
    }
    set(&owner, &mut params, "math_view_scan_progress", 0.0);

    // Trigger count edges are one-shot and retriggerable; a backward seek
    // cancels the active pulse instead of reviving a negative phase.
    set(&owner, &mut params, "math_view_scan_mode", 0.0);
    set(&owner, &mut params, "math_view_scan_amount", 0.0);
    set(&owner, &mut params, "math_view_connect_mesh", 0.0);
    set(&owner, &mut params, "math_view_pulse_trigger", 1.0);
    set(&owner, &mut params, "math_view_pulse_beats", 1.0);
    runtime.note_modifier_clip_event(None);
    let pulse_first = render_at(&mut runtime, &params, 30, 1);
    let pulse_tail = render_at(&mut runtime, &params, 31, 1);
    runtime.note_modifier_clip_event(None);
    let pulse_retrigger = render_at(&mut runtime, &params, 32, 2);
    assert_ne!(
        pulse_first, pulse_tail,
        "pulse must decay after its one-shot edge"
    );
    assert_ne!(
        pulse_retrigger, pulse_tail,
        "a new trigger must retrigger the pulse"
    );
    let seek = render_at(&mut runtime, &params, 2, 2);
    assert_ne!(
        seek, pulse_retrigger,
        "backward seek must cancel the active pulse"
    );
    set(&owner, &mut params, "math_view_pulse_trigger", 0.0);

    // The parent's scan clock continues through presentation cuts.
    set(&owner, &mut params, "math_view_connect_mesh", 1.0);
    set(&owner, &mut params, "math_view_scan_trigger", 1.0);
    set(&owner, &mut params, "math_view_scan_beats", 4.0);
    set(&owner, &mut params, "math_view_scan_mode", 1.0);
    runtime.note_modifier_clip_event(None);
    set(&owner, &mut params, "math_view_mode", 0.0);
    assert!(black(&render(&mut runtime, &params, 60)));
    set(&owner, &mut params, "math_view_mode", 1.0);
    render(&mut runtime, &params, 90);
    set(&owner, &mut params, "math_view_mode", 2.0);
    render(&mut runtime, &params, 120);
    let event_mask = runtime
        .graph
        .instance_by_node_id(
            &crate::node_graph::scene_modifier_expand::math_resource_node_id(
                &owner.scene_modifiers[1].id,
                &owner.scene_modifiers[1].mesh_frames[0].target,
                "weights",
            ),
        )
        .unwrap();
    let center = runtime.graph.get_node(event_mask).unwrap().params["center_y"]
        .as_scalar()
        .unwrap();
    assert!(
        center.abs() < 1e-5,
        "two elapsed beats must remain halfway through the four-beat scan after cuts"
    );
    runtime.note_modifier_clip_event(None);
    assert!(
        black(&render(&mut runtime, &params, 121)),
        "clip edge must restart the connected scan"
    );
    set(&owner, &mut params, "math_view_scan_trigger", 0.0);

    // Restore the original moved-frame controls before the pre-existing
    // standalone/fused parity proof below.
    for control in ["grid", "fragments", "ghosts", "vectors"] {
        set(&owner, &mut params, &format!("math_view_{control}"), 1.0);
    }
    set(&owner, &mut params, "math_view_trails", 0.0);
    for (control, value) in [
        ("grid_brightness", 1.0),
        ("pulse", 0.0),
        ("pulse_strength", 1.0),
        ("pulse_target", 0.0),
        ("scan_amount", 0.0),
        ("scan_progress", 0.0),
        ("scan_mode", 0.0),
        ("scan_target", 0.0),
        ("connect_mesh", 0.0),
    ] {
        set(&owner, &mut params, &format!("math_view_{control}"), value);
    }
    set(&owner, &mut params, "math_view_mode", 1.0);
    runtime.clear_state();
    let moved_restored = render(&mut runtime, &params, 3);

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
        crate::headless_readback::mean_abs_half_diff(&moved_restored, &fused_image) < 0.002,
        "fused and standalone authored evaluations diverged"
    );
    for runtime in [&mut runtime, &mut fused] {
        set(&owner, &mut params, "math_view_axes", 0.0);
        let no_axes = render(runtime, &params, 4);
        assert!(!black(&no_axes), "Axes Off must retain the other diagram marks");
        assert_ne!(no_axes, moved_restored, "Axes control must reach the renderer");
        set(&owner, &mut params, "math_view_axes", 1.0);
        assert!(crate::headless_readback::mean_abs_half_diff(
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
    let guard = crate::test_device();
    let device = guard.arc();
    let owner = owner();
    let registry = PrimitiveRegistry::with_builtin();
    let params = manifest(&owner);
    let mut runtime = PresetRuntime::from_def_for_render(owner, &registry, Some(&params), false)
        .unwrap().with_generator_device(device.clone(), 64, 48, GpuTextureFormat::Rgba16Float).unwrap();
    let arrays = || {
        let backend = runtime.executor.backend();
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
        let injection = crate::render_target::fail_allocation_after(stage);
        let result = runtime.prepare_resize(&device, 48, 64);
        drop(injection);
        assert_eq!((runtime.width, runtime.height), (64, 48));
        for (id, buffer) in &original {
            let backend = runtime.executor.backend();
            assert!(backend.array_buffer(backend.slot_for(*id).unwrap()).unwrap().ptr_eq(buffer));
        }
        match result {
            Err(error) => { assert!(error.to_string().contains("injected GPU allocation failure"), "{error}"); failures += 1; }
            Ok(prepared) => { runtime.commit_resize(prepared); successes += 1; break; }
        }
    }
    assert!(failures > 2, "must exercise parent and child allocation failures");
    assert_eq!(successes, 1, "bounded preparation must eventually succeed");
    assert_eq!((runtime.width, runtime.height), (48, 64));
    let backend = runtime.executor.backend();
    for view in &runtime.math_views {
        let variant = &view.variant;
        for (parent, child) in &view.shared_resources {
            let parent = backend.array_buffer(backend.slot_for(*parent).unwrap()).unwrap();
            let child_backend = variant.executor.backend();
            let child = child_backend.array_buffer(child_backend.slot_for(*child).unwrap()).unwrap();
            assert!(parent.ptr_eq(child));
        }
    }
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn staged_transform_resize_keeps_compatible_chain_and_renders() {
    let device = crate::test_device();
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
    let pixels = crate::headless_readback::readback_raw_halves(&device, &output, 24, 32);
    assert!(pixels.chunks_exact(8).any(|pixel| pixel[..6].chunks_exact(2)
        .any(|channel| half::f16::from_le_bytes([channel[0], channel[1]]).to_f32() > 0.01)));
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn math_view_instance_echoes_render_copies_and_vertices_only_path_is_unchanged() {
    // BUG-uvts GPU proof: Vortex + SpatialEchoes -> Math View must render the
    // echo copies, and the vertices-only fixture must stay byte-identical to
    // its pre-instances behavior (unwired instances port).
    let guard = crate::test_device();
    let device = guard.arc();
    let echo_owner = crate::node_graph::scene_modifier_expand::math_view_test_owner_with_instance_echoes();
    let plain_owner = crate::node_graph::scene_modifier_expand::math_view_test_owner();
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
        crate::headless_readback::readback_raw_halves(&device, &target.texture, 320, 180)
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
    if let Ok(path) = std::env::var("MANIFOLD_MATH_ECHOES_PREVIEW") {
        std::fs::write(
            path,
            crate::headless_readback::readback_to_srgb_png_linear(
                &device,
                &RenderTarget::new(
                    &device,
                    320,
                    180,
                    GpuTextureFormat::Rgba16Float,
                    "math-instance-copies-proof-png",
                )
                .texture,
                320,
                180,
            ),
        )
        .unwrap();
    }
}
