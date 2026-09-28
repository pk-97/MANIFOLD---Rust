//! `docs/REALTIME_3D_DESIGN.md` P5 gate — the persistent [`ViewportSession`]
//! that backs live drag-navigation (the follow-up to
//! `scene_viewport_navigate.rs`'s one-shot `render_viewport_frame` proof).
//!
//! Three things a session must prove that a one-shot render can't:
//! 1. Camera moves are cheap — `orbit`/`pan`/`dolly` never rebuild the
//!    `PresetRuntime` (only the FIRST `render_if_dirty` after `open()` pays
//!    the two-frame warm-up; subsequent camera-only moves render once).
//! 2. A def change (the performer edits the graph while the viewport is
//!    open) is detected and rebuilt via `sync_def`, carrying the camera
//!    forward rather than resetting it.
//! 3. `render_if_dirty` is a real debounce: calling it again with no camera
//!    move and no def change returns the SAME cached bytes without another
//!    GPU dispatch (proven by content, not by a mock — a stale-camera bug
//!    would silently pass a "did it not crash" check).
//!
//! Also produces the PNG evidence the P5b task asks for: three frames at
//! different orbit angles reached purely by driving `ViewportSession`'s
//! input methods, the same surface a real mouse-drag handler calls.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_renderer::headless_readback::encode_rgba8_png;
use manifold_renderer::node_graph::{
    PrimitiveRegistry, Transform, ViewportOverlayConfig, ViewportSession,
};
use manifold_renderer::node_graph::fluid::{
    FluidDomainSnapshot, FluidDomainState, FluidSettings,
};
use manifold_renderer::preset_context::PresetContext;

use crate::harness;

/// Bounds and handles must redraw over the cached scene while a setup drag
/// is still a UI draft. No fluid solver or graph rebuild should run for it.
#[test]
fn viewport_session_fluid_domain_draft_redraws_without_rebuilding() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_renderer::node_graph::{GizmoMode, gizmo_lines, gizmo_target_for};
    use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};
    use manifold_renderer::node_graph::viewport_overlay::fluid_domain_lines;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let def: EffectGraphDef = serde_json::from_str(&scene_json()).unwrap();
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 640;
    frame_ctx.height = 400;
    frame_ctx.output_width = 640;
    frame_ctx.output_height = 400;
    frame_ctx.aspect = 1.6;
    let mut session = ViewportSession::open(
        &def, &NodeId::new("scene"), &registry, std::sync::Arc::clone(&h.device),
        frame_ctx.width, frame_ctx.height, &frame_ctx,
    ).unwrap();
    session.pan(-20.0, 25.0, 0.01);
    session.dolly(-2.5, 0.3);
    let cfg = ViewportOverlayConfig::default();
    let clean = session.render_if_dirty(&frame_ctx, &cfg, None, &[], &[]);
    let mut fluid: EffectGraphDef = serde_json::from_str(r#"{
        "version":2,"nodes":[
            {"id":1,"nodeId":"domain","typeId":"node.transform_3d","params":{
                "pos_y":{"type":"Float","value":2.0},
                "scale_x":{"type":"Float","value":4.0},
                "scale_y":{"type":"Float","value":4.0},
                "scale_z":{"type":"Float","value":4.0}}},
            {"id":2,"nodeId":"water","typeId":"node.fluid_surface"},
            {"id":3,"nodeId":"surface","typeId":"node.scene_object"},
            {"id":4,"nodeId":"scene","typeId":"node.render_scene","params":{
                "objects":{"type":"Int","value":1}}},
            {"id":5,"nodeId":"out","typeId":"system.final_output"}
        ],"wires":[
            {"fromNode":1,"fromPort":"transform","toNode":2,"toPort":"domain"},
            {"fromNode":2,"fromPort":"vertices","toNode":3,"toPort":"vertices"},
            {"fromNode":3,"fromPort":"object","toNode":4,"toPort":"object_0"},
            {"fromNode":4,"fromPort":"color","toNode":5,"toPort":"in"}
        ]}"#).unwrap();
    let mut frames = Vec::new();
    for (label, mode, edit) in [
        ("original", GizmoMode::Move, None),
        ("moved", GizmoMode::Move, Some(("pos_x", 2.0))),
        ("resized", GizmoMode::Scale, Some(("scale_x", 6.0))),
    ] {
        if let Some((param, value)) = edit {
            fluid.nodes[0].params.insert(param.into(), SerializedParamValue::Float { value });
        }
        let scene = SceneVm::from_def(&fluid).expect("fluid authoring scene");
        let target = gizmo_target_for(&scene, 3).expect("editable fluid domain");
        let domain = scene.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.object_node_id == 3 => row.fluid_domain,
            _ => None,
        }).unwrap();
        let mut lines = fluid_domain_lines(domain).to_vec();
        lines.extend(gizmo_lines(mode, &target));
        let frame = session.render_if_dirty(&frame_ctx, &cfg, None, &[], &lines);
        assert_ne!(frame, clean, "{label} bounds must be visible");
        assert!(!session.is_dirty(), "overlay drafts must not rebuild or simulate");
        std::fs::write(format!("/tmp/fluid_domain_draft_{label}.png"),
            encode_rgba8_png(&frame, frame_ctx.width, frame_ctx.height)).unwrap();
        frames.push(frame);
    }
    assert_ne!(frames[0], frames[1], "moving the bounds must redraw");
    assert_ne!(frames[1], frames[2], "resizing the bounds must redraw");
    assert_eq!(clean, session.render_if_dirty(&frame_ctx, &cfg, None, &[], &[]),
        "cancelling a draft restores the unchanged cached scene");
}

fn fluid_controls(domain_x: f32, resolution: f32, reset: f32, material_r: f32) -> ParamManifest {
    ParamManifest::from_params(
        [
            ("domain_x", domain_x, -8.0, 8.0, false, false),
            ("resolution", resolution, 8.0, 96.0, true, false),
            ("reset", reset, 0.0, 1.0, false, true),
            ("material_r", material_r, 0.0, 1.0, false, false),
        ]
        .into_iter()
        .map(|(id, value, min, max, whole_numbers, is_trigger)| {
            let spec = ParamSpecDef {
                id: id.into(),
                name: id.into(),
                min,
                max,
                default_value: value,
                whole_numbers,
                is_trigger,
                ..ParamSpecDef::default()
            };
            let mut param = Param::bundled(spec);
            param.value = value;
            param.base = value;
            param
        })
        .collect(),
    )
}

fn set_fluid_control(params: &mut ParamManifest, id: &str, value: f32) {
    let param = params.get_mut(id).expect("fluid test binding exists");
    param.value = value;
    param.base = value;
}

fn read_fluid_snapshot(session: &ViewportSession) -> FluidDomainSnapshot {
    let mut snapshots = Vec::new();
    session.write_fluid_domains(&mut snapshots);
    let (_, snapshot) = snapshots
        .into_iter()
        .find(|(node_id, _)| node_id.as_str() == "water")
        .expect("fluid surface snapshot");
    snapshot
}

/// The viewport borrows the live scene inputs. Navigation must neither step
/// the fluid again nor change the show camera or its output pixels.
#[test]
fn shared_scene_viewport_navigates_without_advancing_fluid_or_changing_show() {
    use manifold_gpu::GpuTextureFormat;
    use manifold_renderer::frame_status::FrameRenderStatus;
    use manifold_renderer::gpu_encoder::GpuEncoder;
    use manifold_renderer::headless_readback::{readback_raw_halves, readback_srgb_rgba8};
    use manifold_renderer::node_graph::ViewportCamera;
    use manifold_renderer::node_graph::physics::PhysicsStepScope;
    use manifold_renderer::node_graph::scene_viewport::{SceneViewportConfig, SceneViewportError};
    use manifold_renderer::preset_runtime::PresetRuntime;
    use manifold_renderer::render_target::RenderTarget;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    // Keep the diagnostic scalar ports live through ordinary graph wires;
    // the executor intentionally does not allocate unconsumed outputs.
    let mut def: serde_json::Value = serde_json::from_str(fluid_session_json()).unwrap();
    def["wires"].as_array_mut().unwrap().extend([
        serde_json::json!({"fromNode":3,"fromPort":"simulation_time","toNode":5,"toPort":"color_b"}),
        serde_json::json!({"fromNode":3,"fromPort":"vertex_count","toNode":4,"toPort":"visible"}),
    ]);
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(&def).unwrap(), &registry, std::sync::Arc::clone(&h.device),
        320, 200, GpuTextureFormat::Rgba16Float, None,
    ).unwrap();
    let node_count = runtime.graph.nodes().count();
    let params = fluid_controls(0.0, 8.0, 0.0, 0.8);
    let target = RenderTarget::new(&h.device, 320, 200,
        GpuTextureFormat::Rgba16Float, "shared-viewport-show");
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 320;
    frame_ctx.height = 200;
    frame_ctx.output_width = 320;
    frame_ctx.output_height = 200;
    frame_ctx.aspect = 1.6;
    frame_ctx.time = 0.0;
    frame_ctx.beat = 0.0;
    let _offline = PhysicsStepScope::for_render(true);
    runtime.set_preview_node(Some(&NodeId::new("water")));
    let render = |runtime: &mut PresetRuntime, frame_ctx: &PresetContext| {
        let mut enc = h.device.create_encoder("shared-viewport-frame");
        let status = {
            let mut gpu = GpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, frame_ctx, &params);
            gpu.frame_status()
        };
        enc.commit_and_wait_completed();
        assert_eq!(status, FrameRenderStatus::Complete);
        readback_raw_halves(&h.device, &target.texture, 320, 200)
    };
    let snapshot = |runtime: &PresetRuntime| {
        let mut domains = Vec::new();
        runtime.write_fluid_domains_watched(&mut domains);
        domains.into_iter().find(|(id, _)| id.as_str() == "water").unwrap().1
    };
    let scalar = |runtime: &PresetRuntime, name: &str| {
        runtime.preview_scalar_io().1.into_iter()
            .find(|(port, _)| port == name).unwrap().1
    };
    render(&mut runtime, &frame_ctx);
    frame_ctx.time = manifold_renderer::node_graph::fluid::TICK;
    frame_ctx.beat = frame_ctx.time;
    frame_ctx.frame_count = 1;
    let show = render(&mut runtime, &frame_ctx);
    let accepted = snapshot(&runtime);
    assert_eq!(accepted.state, FluidDomainState::Ready);
    let simulation_time = scalar(&runtime, "simulation_time");
    assert!(simulation_time > 0.0);
    assert!(scalar(&runtime, "vertex_count") > 0.0, "proof needs a visible liquid mesh");

    let mut config = SceneViewportConfig {
        camera: ViewportCamera { target: [0.0, 1.5, 0.0], pitch: -0.4,
            ..ViewportCamera::default() },
        width: 320, height: 200,
    };
    let mut previous_view = None;
    for turn in 0..3 {
        if turn > 0 { config.camera.orbit(100.0, 15.0, 0.006); }
        runtime.set_scene_viewport_watched(&NodeId::new("scene"), config).unwrap();
        assert!(runtime.scene_viewport_texture().is_none(), "request invalidates stale capture");
        let with_view = render(&mut runtime, &frame_ctx);
        assert!(show == with_view, "viewport navigation changed the show image");
        assert_eq!(snapshot(&runtime), accepted);
        assert_eq!(scalar(&runtime, "simulation_time"), simulation_time);
        assert_eq!(runtime.graph.nodes().count(), node_count);
        assert_eq!(runtime.scene_viewport_status(), Some(FrameRenderStatus::Complete));
        assert!(runtime.scene_viewport_errors().is_empty());
        let texture = runtime.scene_viewport_texture().expect("complete viewport texture");
        let view = readback_raw_halves(&h.device, texture, 320, 200);
        if let Some(previous) = previous_view {
            assert!(view != previous, "editor camera must change viewport pixels");
        }
        let rgba = readback_srgb_rgba8(&h.device, texture, 320, 200);
        std::fs::write(format!("/tmp/shared_fluid_viewport_{turn}.png"),
            encode_rgba8_png(&rgba, 320, 200)).unwrap();
        previous_view = Some(view);
    }

    // Resizing replaces only editor outputs, retaining the native world.
    config.width = 256;
    config.height = 160;
    runtime.set_scene_viewport_watched(&NodeId::new("scene"), config).unwrap();
    assert!(show == render(&mut runtime, &frame_ctx));
    let texture = runtime.scene_viewport_texture().unwrap();
    assert_eq!((texture.width, texture.height), (256, 160));
    assert_eq!(snapshot(&runtime), accepted);
    assert_eq!(scalar(&runtime, "simulation_time"), simulation_time);

    assert_eq!(runtime.set_scene_viewport_watched(&NodeId::new("water"), config),
        Err(SceneViewportError::NotSceneRenderer));
    assert!(runtime.scene_viewport_texture().is_none());
    assert_eq!(runtime.scene_viewport_status(), None);
    assert_eq!(runtime.set_scene_viewport_watched(&NodeId::new("missing"), config),
        Err(SceneViewportError::TargetNotFound));
    config.width = 0;
    assert_eq!(runtime.set_scene_viewport_watched(&NodeId::new("scene"), config),
        Err(SceneViewportError::InvalidConfiguration));
    runtime.clear_scene_viewport();
    assert!(show == render(&mut runtime, &frame_ctx), "closing viewport changed the show");
    assert_eq!(snapshot(&runtime), accepted);
}

#[test]
fn shared_scene_viewport_does_not_activate_a_hidden_fluid_branch() {
    use manifold_gpu::GpuTextureFormat;
    use manifold_renderer::frame_status::FrameRenderStatus;
    use manifold_renderer::gpu_encoder::GpuEncoder;
    use manifold_renderer::node_graph::scene_viewport::SceneViewportConfig;
    use manifold_renderer::preset_runtime::PresetRuntime;
    use manifold_renderer::render_target::RenderTarget;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut def: serde_json::Value = serde_json::from_str(fluid_session_json()).unwrap();
    def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
        "id":10, "nodeId":"active_scene", "typeId":"node.render_scene",
        "params":{"objects":{"type":"Int","value":0}, "lights":{"type":"Int","value":0}}
    }));
    let wires = def["wires"].as_array_mut().unwrap();
    wires.retain(|wire| wire["toNode"] != 9);
    wires.push(serde_json::json!({"fromNode":10,"fromPort":"color","toNode":9,"toPort":"in"}));
    wires.push(serde_json::json!({"fromNode":6,"fromPort":"out","toNode":10,"toPort":"camera"}));
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &serde_json::to_string(&def).unwrap(), &registry, std::sync::Arc::clone(&h.device),
        320, 200, GpuTextureFormat::Rgba16Float, None,
    ).unwrap();
    let mut before = Vec::new();
    runtime.write_fluid_domains_watched(&mut before);
    assert_eq!(before.len(), 1);
    runtime.set_scene_viewport_watched(&NodeId::new("scene"), SceneViewportConfig {
        camera: Default::default(), width: 320, height: 200,
    }).unwrap();
    let target = RenderTarget::new(&h.device, 320, 200,
        GpuTextureFormat::Rgba16Float, "hidden-fluid-viewport");
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 320;
    frame_ctx.height = 200;
    frame_ctx.output_width = 320;
    frame_ctx.output_height = 200;
    frame_ctx.aspect = 1.6;
    let mut enc = h.device.create_encoder("hidden-fluid-viewport-frame");
    {
        let mut gpu = GpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, &target.texture, &frame_ctx, &fluid_controls(0.0, 8.0, 0.0, 0.8));
        assert_eq!(gpu.frame_status(), FrameRenderStatus::Complete);
    }
    enc.commit_and_wait_completed();
    let mut after = Vec::new();
    runtime.write_fluid_domains_watched(&mut after);
    assert_eq!(before, after, "viewport must not initialize hidden physics");
    assert!(runtime.scene_viewport_texture().is_none());
    assert_eq!(runtime.scene_viewport_status(), Some(FrameRenderStatus::PendingGeometry));
}

/// Ray-tracing/temporal history belongs to each renderer, even though the
/// two cameras read one scene. Compare matching frames on a control runtime
/// because temporal accumulation can legitimately change repeated frames.
#[test]
fn shared_scene_viewport_preserves_rt_and_temporal_show_history() {
    use manifold_gpu::GpuTextureFormat;
    use manifold_renderer::frame_status::FrameRenderStatus;
    use manifold_renderer::gpu_encoder::GpuEncoder;
    use manifold_renderer::headless_readback::readback_raw_halves;
    use manifold_renderer::node_graph::scene_viewport::SceneViewportConfig;
    use manifold_renderer::preset_runtime::PresetRuntime;
    use manifold_renderer::render_target::RenderTarget;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut def: serde_json::Value = serde_json::from_str(&scene_json()).unwrap();
    let scene = def["nodes"].as_array_mut().unwrap().iter_mut()
        .find(|node| node["nodeId"] == "scene").unwrap();
    for param in ["rt_enabled", "temporal_upscale", "rt_denoise_feed"] {
        scene["params"][param] = serde_json::json!({"type":"Bool","value":true});
    }
    let json = serde_json::to_string(&def).unwrap();
    let build = || PresetRuntime::from_json_str_with_device(
        &json, &registry, std::sync::Arc::clone(&h.device), 320, 200,
        GpuTextureFormat::Rgba16Float, None,
    ).unwrap();
    let mut control = build();
    let mut viewed = build();
    let target = RenderTarget::new(&h.device, 320, 200,
        GpuTextureFormat::Rgba16Float, "rt-shared-viewport-show");
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 320;
    frame_ctx.height = 200;
    frame_ctx.output_width = 320;
    frame_ctx.output_height = 200;
    frame_ctx.aspect = 1.6;
    let mut config = SceneViewportConfig {
        camera: manifold_renderer::node_graph::ViewportCamera {
            pitch: -0.45, ..Default::default()
        }, width: 320, height: 200,
    };
    let render = |runtime: &mut PresetRuntime, frame_ctx: &PresetContext| {
        let mut enc = h.device.create_encoder("rt-shared-viewport-frame");
        let (status, dispatches) = {
            let mut gpu = GpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, frame_ctx, &ParamManifest::default());
            (gpu.frame_status(), gpu.rt_dispatches)
        };
        enc.commit_and_wait_completed();
        (readback_raw_halves(&h.device, &target.texture, 320, 200), status, dispatches)
    };
    let mut viewport_frames = 0;
    let mut rt_dispatches = 0;
    for frame in 0..6 {
        frame_ctx.frame_count = frame;
        if frame == 3 {
            control.clear_state();
            viewed.clear_state();
            assert!(viewed.scene_viewport_texture().is_none(), "reset must invalidate the old image");
            assert_eq!(viewed.scene_viewport_status(), Some(FrameRenderStatus::PendingGeometry));
        }
        if frame == 4 {
            viewed.clear_scene_viewport();
        } else if frame < 4 {
            config.camera.orbit(50.0, 0.0, 0.008);
            viewed.set_scene_viewport_watched(&NodeId::new("scene"), config).unwrap();
        }
        let (expected, expected_status, expected_dispatches) = render(&mut control, &frame_ctx);
        let (actual, actual_status, actual_dispatches) = render(&mut viewed, &frame_ctx);
        assert_eq!(actual_status, expected_status, "viewport changed show validity");
        assert!(!matches!(actual_status, FrameRenderStatus::Failed(_)));
        assert!(expected == actual, "viewport changed RT/temporal show frame {frame}");
        assert_eq!(actual_dispatches, expected_dispatches,
            "viewport RT accounting must be isolated from show accounting");
        rt_dispatches += actual_dispatches;
        if frame < 4 {
            assert!(viewed.scene_viewport_errors().is_empty(), "{:?}", viewed.scene_viewport_errors());
            if viewed.scene_viewport_status() == Some(FrameRenderStatus::Complete) {
                assert!(viewed.scene_viewport_texture().is_some());
                viewport_frames += 1;
            }
        } else {
            assert!(viewed.scene_viewport_texture().is_none());
        }
    }
    assert!(viewport_frames > 0, "RT viewport never produced a complete image");
    assert!(rt_dispatches > 0, "proof must actually exercise ray tracing");
}

/// Effective outer-card values must refresh the live viewport runtime in
/// place. This covers the native Metal render path, the value→domain wire,
/// and the accepted fluid-domain observation seam together in one bounded
/// 320×200 proof.
#[test]
fn viewport_session_refreshes_effective_controls_and_fluid_bounds() {
    use manifold_renderer::node_graph::physics::PhysicsStepScope;

    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let def: EffectGraphDef = serde_json::from_str(fluid_session_json()).unwrap();
    let mut frame_ctx = ctx(h);
    frame_ctx.width = 320;
    frame_ctx.height = 200;
    frame_ctx.output_width = 320;
    frame_ctx.output_height = 200;
    frame_ctx.aspect = 1.6;
    frame_ctx.time = 0.0;
    frame_ctx.beat = 0.0;
    let offline = PhysicsStepScope::for_render(true);
    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        std::sync::Arc::clone(&h.device),
        frame_ctx.width,
        frame_ctx.height,
        &frame_ctx,
    )
    .expect("fluid viewport session must open");
    session.pan(-20.0, 25.0, 0.01);
    session.dolly(-2.5, 0.3);

    let mut params = fluid_controls(0.0, 8.0, 0.0, 0.8);
    session.refresh(&frame_ctx, &params);
    let initial_snapshot = read_fluid_snapshot(&session);
    assert_eq!(initial_snapshot.state, FluidDomainState::Ready);
    assert!(initial_snapshot.epoch > 0);
    assert_eq!(
        initial_snapshot.accepted_layout,
        Some(
            FluidSettings {
                resolution: 8,
                domain: Some(Transform {
                    pos: [0.0, 2.0, 0.0],
                    scale: [4.0; 3],
                    ..Transform::default()
                }),
                ..FluidSettings::default()
            }
            .domain_layout()
            .unwrap()
        )
    );

    // Two reset edges make a retained runtime distinguishable from a rebuild
    // that observes only the final counter value (which would reset once).
    for counter in [1.0, 2.0] {
        set_fluid_control(&mut params, "reset", counter);
        session.refresh(&frame_ctx, &params);
    }
    let reset_snapshot = read_fluid_snapshot(&session);
    assert_eq!(reset_snapshot.epoch, initial_snapshot.epoch + 2);
    // Initialization accepts an empty tick-zero mesh. A navigation refresh
    // one physical tick later produces the surface used for pixel assertions.
    frame_ctx.time = manifold_renderer::node_graph::fluid::TICK;
    frame_ctx.beat = frame_ctx.time;
    frame_ctx.frame_count = 1;
    session.orbit(1.0, 0.0, 0.005);
    session.refresh(&frame_ctx, &params);
    let camera = *session.camera();
    let before_recolor = session.composite_overlays(&ViewportOverlayConfig::default(), None, &[], &[]);

    // Material changes flow through the effective manifest without changing
    // the native epoch or camera, and must alter the readback pixels.
    let epoch_after_reset = reset_snapshot.epoch;
    set_fluid_control(&mut params, "material_r", 0.05);
    session.sync_def(&def, &registry, &frame_ctx).unwrap();
    session.refresh(&frame_ctx, &params);
    let recolored = session.composite_overlays(&ViewportOverlayConfig::default(), None, &[], &[]);
    assert!(before_recolor != recolored, "effective material binding must change pixels");
    let recolored_snapshot = read_fluid_snapshot(&session);
    assert_eq!(recolored_snapshot.epoch, epoch_after_reset);
    assert_eq!(*session.camera(), camera);
    let initial_bounds = session.composite_overlays(&ViewportOverlayConfig::default(), None, &[],
        &manifold_renderer::node_graph::viewport_overlay::fluid_domain_lines(recolored_snapshot.accepted_layout.unwrap()));
    std::fs::write("/tmp/fluid_runtime_bounds_initial.png", encode_rgba8_png(&initial_bounds, frame_ctx.width, frame_ctx.height)).unwrap();

    // An unchanged manifest at a later context time is a cache hit: both the
    // bytes and accepted runtime observation remain stable.
    let mut later_ctx = frame_ctx;
    later_ctx.time = 0.8;
    later_ctx.beat = 0.9;
    later_ctx.frame_count = 9;
    session.refresh(&later_ctx, &params);
    let cached = session.composite_overlays(&ViewportOverlayConfig::default(), None, &[], &[]);
    assert!(cached == recolored, "unchanged controls must preserve cached pixels");
    assert_eq!(read_fluid_snapshot(&session), recolored_snapshot);

    // A driven domain edit plus resolution change starts a new native epoch;
    // the published layout must equal FluidSettings' snapped cell layout.
    set_fluid_control(&mut params, "domain_x", 1.0);
    set_fluid_control(&mut params, "resolution", 10.0);
    session.refresh(&frame_ctx, &params);
    let edited_snapshot = read_fluid_snapshot(&session);
    let expected = FluidSettings {
        resolution: 10,
        domain: Some(Transform {
            pos: [1.0, 2.0, 0.0],
            scale: [4.0; 3],
            ..Transform::default()
        }),
        ..FluidSettings::default()
    }
    .domain_layout()
    .unwrap();
    assert!(edited_snapshot.epoch > recolored_snapshot.epoch);
    assert_eq!(edited_snapshot.state, FluidDomainState::Ready);
    assert_eq!(edited_snapshot.accepted_layout, Some(expected));
    frame_ctx.time += manifold_renderer::node_graph::fluid::TICK;
    frame_ctx.beat = frame_ctx.time;
    frame_ctx.frame_count += 1;
    session.orbit(1.0, 0.0, 0.005);
    session.refresh(&frame_ctx, &params);
    let edited_bounds = session.composite_overlays(&ViewportOverlayConfig::default(), None, &[],
        &manifold_renderer::node_graph::viewport_overlay::fluid_domain_lines(expected));
    std::fs::write("/tmp/fluid_runtime_bounds_edited.png", encode_rgba8_png(&edited_bounds, frame_ctx.width, frame_ctx.height)).unwrap();
    assert!(initial_bounds != edited_bounds, "accepted domain edits must redraw the outline");

    // The real preview path is asynchronous. After the bounded offline
    // assertions, let the new epoch settle through repeated refreshes while
    // keeping the manifest and camera fixed.
    drop(offline);
    set_fluid_control(&mut params, "domain_x", 2.0);
    let mut async_snapshot = None;
    for _ in 0..200 {
        session.refresh(&frame_ctx, &params);
        let snapshot = read_fluid_snapshot(&session);
        if snapshot.state == FluidDomainState::Ready {
            async_snapshot = Some(snapshot);
            break;
        }
        assert_eq!(snapshot.state, FluidDomainState::Initializing);
        assert!(snapshot.accepted_layout.is_none());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let async_snapshot = async_snapshot.expect("async fluid epoch must settle");
    assert_eq!(async_snapshot.state, FluidDomainState::Ready);
    assert_eq!(async_snapshot.epoch, edited_snapshot.epoch + 1);
    assert_eq!(
        async_snapshot.accepted_layout,
        Some(
            FluidSettings {
                resolution: 10,
                domain: Some(Transform {
                    pos: [2.0, 2.0, 0.0],
                    scale: [4.0; 3],
                    ..Transform::default()
                }),
                ..FluidSettings::default()
            }
            .domain_layout()
            .unwrap()
        )
    );
}

fn fluid_session_json() -> &'static str {
    // Keep several filled cell layers at resolution 8 so boundary clipping
    // leaves a visible surface for the navigation and material assertions.
    r#"{
        "version": 2,
        "name": "ViewportFluidProof",
        "presetMetadata": {
            "id": "ViewportFluidProof",
            "displayName": "Viewport Fluid Proof",
            "category": "Diagnostic",
            "oscPrefix": "viewport_fluid_proof",
            "available": false,
            "params": [
                {"id":"domain_x","name":"Domain X","min":-8.0,"max":8.0,"defaultValue":0.0},
                {"id":"resolution","name":"Resolution","min":8.0,"max":96.0,"defaultValue":8.0,"wholeNumbers":true},
                {"id":"reset","name":"Reset","min":0.0,"max":1.0,"defaultValue":0.0,"isTrigger":true},
                {"id":"material_r","name":"Material Red","min":0.0,"max":1.0,"defaultValue":0.8}
            ],
            "bindings": [
                {"id":"domain_x","label":"Domain X","defaultValue":0.0,"target":{"kind":"node","nodeId":"domain_value","param":"value"},"convert":{"type":"Float"}},
                {"id":"resolution","label":"Resolution","defaultValue":8.0,"target":{"kind":"node","nodeId":"water","param":"resolution"},"convert":{"type":"Float"}},
                {"id":"reset","label":"Reset","defaultValue":0.0,"target":{"kind":"node","nodeId":"water","param":"reset"},"convert":{"type":"Float"}},
                {"id":"material_r","label":"Material Red","defaultValue":0.8,"target":{"kind":"node","nodeId":"mat","param":"color_r"},"convert":{"type":"Float"}}
            ]
        },
        "nodes": [
            {"id":0,"typeId":"system.generator_input","nodeId":"input"},
            {"id":1,"typeId":"node.value","nodeId":"domain_value","params":{"value":{"type":"Float","value":0.0}}},
            {"id":2,"typeId":"node.transform_3d","nodeId":"domain","params":{"pos_y":{"type":"Float","value":2.0},"scale_x":{"type":"Float","value":4.0},"scale_y":{"type":"Float","value":4.0},"scale_z":{"type":"Float","value":4.0}}},
            {"id":3,"typeId":"node.fluid_surface","nodeId":"water","params":{"resolution":{"type":"Float","value":8.0},"fill_height":{"type":"Float","value":1.5},"max_capacity":{"type":"Float","value":100000.0}}},
            {"id":4,"typeId":"node.scene_object","nodeId":"water_object"},
            {"id":5,"typeId":"node.phong_material","nodeId":"mat","params":{"color_r":{"type":"Float","value":0.8},"color_g":{"type":"Float","value":0.35},"color_b":{"type":"Float","value":0.12},"ambient":{"type":"Float","value":0.2}}},
            {"id":6,"typeId":"node.orbit_camera","nodeId":"cam","params":{"orbit":{"type":"Float","value":0.6},"tilt":{"type":"Float","value":0.7},"distance":{"type":"Float","value":9.0},"fov_y":{"type":"Float","value":0.8}}},
            {"id":7,"typeId":"node.light","nodeId":"sun","params":{"mode":{"type":"Enum","value":0},"pos_x":{"type":"Float","value":4.0},"pos_y":{"type":"Float","value":10.0},"pos_z":{"type":"Float","value":4.0},"aim_x":{"type":"Float","value":0.0},"aim_y":{"type":"Float","value":0.0},"aim_z":{"type":"Float","value":0.0},"color_r":{"type":"Float","value":1.0},"color_g":{"type":"Float","value":1.0},"color_b":{"type":"Float","value":1.0},"intensity":{"type":"Float","value":2.0},"cast_shadows":{"type":"Float","value":0.0}}},
            {"id":8,"typeId":"node.render_scene","nodeId":"scene","params":{"objects":{"type":"Int","value":1},"lights":{"type":"Int","value":1}}},
            {"id":9,"typeId":"system.final_output","nodeId":"out"}
        ],
        "wires": [
            {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"pos_x"},
            {"fromNode":2,"fromPort":"transform","toNode":3,"toPort":"domain"},
            {"fromNode":3,"fromPort":"vertices","toNode":4,"toPort":"vertices"},
            {"fromNode":4,"fromPort":"object","toNode":8,"toPort":"object_0"},
            {"fromNode":5,"fromPort":"out","toNode":4,"toPort":"material"},
            {"fromNode":6,"fromPort":"out","toNode":8,"toPort":"camera"},
            {"fromNode":7,"fromPort":"out","toNode":8,"toPort":"light_0"},
            {"fromNode":8,"fromPort":"color","toNode":9,"toPort":"in"}
        ]
    }"#
}

/// Ground plane lit by one sun, wired to an `orbit_camera` (the SHOW
/// camera) — identical scene to `scene_viewport_navigate.rs`'s proof scene,
/// deliberately: this test is about session lifecycle/dirty-tracking, not
/// scene fidelity.
fn scene_json() -> String {
    r#"{"version":2,"name":"ViewportSessionProof","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":20},
            "resolution_y":{"type":"Int","value":20},
            "size_x":{"type":"Float","value":8.0},
            "size_y":{"type":"Float","value":8.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":20},
            "src_rows":{"type":"Int","value":20}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"show_cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.6},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.cel_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.8},
            "color_b":{"type":"Float","value":0.9},
            "band_low":{"type":"Float","value":0.1}}},
        {"id":5,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":4.0},
            "pos_y":{"type":"Float","value":20.0},
            "pos_z":{"type":"Float","value":3.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
    ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
    ]}"#
        .to_string()
}

fn ctx(h: &harness::ParityHarness) -> PresetContext {
    PresetContext {
        time: 0.1,
        beat: 0.2,
        dt: 1.0 / 60.0,
        width: h.width,
        height: h.height,
        output_width: h.width,
        output_height: h.height,
        aspect: h.width as f32 / h.height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Drive a `ViewportSession` with synthetic input events exactly the shape a
/// real mouse-drag handler would produce (pixel deltas + a sensitivity
/// constant), and prove: (1) navigation actually moves the camera and the
/// rendered pixels change with it — three PNGs at three orbit angles; (2) a
/// no-op `render_if_dirty` call (nothing moved) is a cache hit, not a
/// re-render — proven by content equality on consecutive calls with zero
/// camera delta in between; (3) `sync_def` on an unchanged def is a no-op
/// (doesn't reset the camera / force a spurious rebuild-driven redraw).
#[test]
fn viewport_session_navigates_and_debounces() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse scene def");
    let frame_ctx = ctx(h);

    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        &frame_ctx,
    )
    .expect("viewport session must open");

    let overlay_cfg = ViewportOverlayConfig::default();

    // ── Frame 1: default framing. ──
    assert!(session.is_dirty(), "a freshly-opened session must render its first frame");
    let frame1 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert!(!session.is_dirty(), "render_if_dirty must clear the dirty flag");

    // ── Debounce proof: call again with NO input in between — must be a
    //    cache hit (identical bytes, no new dispatch needed to prove it,
    //    since a stale/rebuilt render would still often look "similar" —
    //    what actually matters is `is_dirty()` staying false). ──
    let frame1_again = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_eq!(frame1, frame1_again, "no camera/def change ⇒ identical cached bytes");
    assert!(!session.is_dirty(), "no-op render_if_dirty must not mark dirty");

    // ── Frame 2: orbit — LMB-drag equivalent. ──
    session.orbit(220.0, 40.0, 0.01);
    assert!(session.is_dirty(), "orbit() must mark the session dirty");
    let frame2 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(frame1, frame2, "orbiting the camera must change the rendered pixels");

    // ── Frame 3: dolly in — scroll-wheel equivalent, from the orbited pose. ──
    session.dolly(1.0, 0.3);
    assert!(session.is_dirty(), "dolly() must mark the session dirty");
    let frame3 = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(frame2, frame3, "dollying must change the rendered pixels");

    // ── sync_def on the SAME def is a no-op: doesn't reset the camera or
    //    force a redraw. ──
    session.sync_def(&def, &registry, &frame_ctx).expect("sync_def on unchanged def must succeed");
    assert!(!session.is_dirty(), "sync_def on an unchanged def must not mark dirty");
    assert!(
        (session.camera().yaw - 0.6 - 220.0 * 0.01).abs() < 1e-4,
        "sync_def on an unchanged def must not reset the navigated camera"
    );

    let (vw, vh) = session.dimensions();
    for (label, frame) in [("open", &frame1), ("orbit", &frame2), ("dolly", &frame3)] {
        let png = encode_rgba8_png(frame, vw, vh);
        let path = format!("/tmp/viewport_session_{label}.png");
        std::fs::write(&path, &png).unwrap_or_else(|e| panic!("write {path}: {e}"));
        eprintln!("[P5b gate] wrote {path} ({vw}x{vh})");
    }
}

/// `sync_def` on a genuinely CHANGED def (a param edit, simulating the
/// performer tweaking the scene while the viewport is open) rebuilds and
/// re-renders — proven by pixel change with the camera held fixed.
#[test]
fn viewport_session_rebuilds_on_def_change() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse scene def");
    let frame_ctx = ctx(h);

    let mut session = ViewportSession::open(
        &def,
        &NodeId::new("scene"),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        &frame_ctx,
    )
    .expect("viewport session must open");
    let overlay_cfg = ViewportOverlayConfig::default();
    let before = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);

    // Edit the material color — a real "performer changed the graph" event.
    let mut edited = def.clone();
    let mat = edited
        .nodes
        .iter_mut()
        .find(|n| n.node_id.as_str() == "mat")
        .expect("mat node present");
    mat.params.insert(
        "color_r".to_string(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.05 },
    );

    session.sync_def(&edited, &registry, &frame_ctx).expect("sync_def must rebuild on a real change");
    assert!(session.is_dirty(), "a real def change must mark the session dirty");
    let after = session.render_if_dirty(&frame_ctx, &overlay_cfg, None, &[], &[]);
    assert_ne!(before, after, "a material color edit must change the rendered pixels");
}
