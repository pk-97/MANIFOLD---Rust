//! Native content → shared surface → editor painter acceptance. This does not
//! exercise the macOS window-event loop; navigation uses its production mapper.
use std::sync::Arc;

use manifold_core::{Beats, GraphTarget, NodeId, PresetTypeId};
use manifold_gpu::{GpuLoadAction, GpuTextureFormat};
use manifold_renderer::{
    headless_readback::{encode_rgba8_png, readback_raw_halves, readback_srgb_rgba8},
    node_graph::{fluid::FluidDomainState, grid_lines},
    render_target::RenderTarget,
    ui_renderer::UIRenderer,
};

use crate::{
    content_command::ContentCommand,
    content_state::ContentState,
    content_thread::ContentThread,
    scene_viewport::{
        SceneViewportFrame, SceneViewportNavigation, SceneViewportPaint, SceneViewportRequest,
    },
    shared_texture::{BridgeReadLease, SharedTextureBridge},
};

fn project() -> manifold_core::project::Project {
    use manifold_core::{clip::TimelineClip, layer::Layer};
    let mut project = manifold_core::project::Project::default();
    project.settings.output_width = 320;
    project.settings.output_height = 200;
    // Two live generators exercise request retention when a different layer
    // renders after the selected one. Keep the CPU liquid fixture small.
    for index in 0..2 {
        let mut layer = Layer::new_generator(
            format!("Water {index}"),
            PresetTypeId::new("WaterBasin"),
            index,
        );
        let mut def: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(
            include_str!("../../manifold-renderer/assets/generator-presets/WaterBasin.json"),
        )
        .unwrap();
        // The native solver remains real; freeze inputs so navigation can be
        // compared without time-varying obstacle or emission changes.
        let fluid = def
            .nodes
            .iter_mut()
            .find(|node| node.node_id.as_str() == "fluid_surface")
            .unwrap();
        fluid.params.insert(
            "fill_height".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
        );
        fluid.params.insert(
            "gravity".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 0.0 },
        );
        for (port, to_node, to_port) in [
            ("vertex_count", 9, "visible"),
            ("simulation_time", 11, "color_b"),
            ("lag_seconds", 14, "metallic"),
        ] {
            def.wires
                .push(manifold_core::effect_graph_def::EffectGraphWire {
                    from_node: 4,
                    from_port: port.into(),
                    to_node,
                    to_port: to_port.into(),
                });
        }
        let params = layer.gen_params_or_init();
        params.graph = Some(def);
        params.refresh_manifest_from_graph();
        params.set_base_param("resolution", 8.0);
        params.set_base_param("pour", 0.0);
        params.set_base_param("speed", 1.0);
        layer
            .clips
            .push(TimelineClip::new_generator(Beats::ZERO, Beats(16.0)));
        project.timeline.layers.push(layer);
    }
    project
}

fn tick(
    ct: &mut ContentThread,
    tx: &crossbeam_channel::Sender<ContentState>,
    rx: &crossbeam_channel::Receiver<ContentState>,
) -> ContentState {
    ct.tick_frame(tx);
    ct.content_pipeline.wait_for_render_complete();
    rx.try_iter().last().expect("content snapshot")
}

fn observation(
    state: &ContentState,
    bridge: &SharedTextureBridge,
    request: &SceneViewportRequest,
) -> (BridgeReadLease, SceneViewportFrame) {
    let lease = bridge.acquire_read();
    let frame_id = bridge.leased_frame(lease).expect("published preview frame");
    let frame = state.scene_viewport_frames[lease.slot()]
        .as_ref()
        .expect("metadata for published slot");
    assert!(
        frame.matches(request, frame_id, bridge.generation()),
        "image and metadata must be from the same owner/session/frame"
    );
    (lease, frame.clone())
}

fn selected_scene_pixels(ct: &ContentThread, device: &manifold_gpu::GpuDevice) -> Vec<u8> {
    scene_pixels(ct, device, 0)
}

fn scene_pixels(ct: &ContentThread, device: &manifold_gpu::GpuDevice, index: usize) -> Vec<u8> {
    let generator = ct
        .engine
        .renderers()
        .iter()
        .find_map(|renderer| {
            renderer
                .as_any()
                .downcast_ref::<manifold_renderer::generator_renderer::GeneratorRenderer>()
        })
        .unwrap();
    let clip = &ct.engine.project().unwrap().timeline.layers[index].clips[0].id;
    let texture = generator
        .get_clip_texture(clip.as_str())
        .expect("selected live scene output");
    readback_raw_halves(device, texture, 320, 200)
}

#[test]
fn shared_scene_viewport_watched_generator_preserves_liquid() {
    let mut project = project();
    // One scene uses a prepared mesh collider; the other retains legacy box
    // inputs. Editor rebuilds must preserve both the worker and its role Arc.
    let params = project.timeline.layers[0].gen_params_or_init();
    let def = params.graph.as_mut().unwrap();
    def.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": 40, "nodeId": "mesh_collider", "typeId": "node.fluid_role_source",
            "params": {"role": {"type": "Enum", "value": 3}}
        }))
        .unwrap(),
    );
    def.wires
        .retain(|wire| !(wire.to_node == 4 && wire.to_port == "obstacle"));
    for (from_node, from_port, to_node, to_port) in [
        (10, "source", 40, "mesh_0"),
        (7, "transform", 40, "transform"),
        (40, "role", 4, "role_0"),
    ] {
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
    }
    params.refresh_manifest_from_graph();
    let targets: Vec<_> = project
        .timeline
        .layers
        .iter()
        .map(|layer| GraphTarget::Generator(layer.layer_id.clone()))
        .collect();
    let mut ct = crate::headless_harness::headless_content_thread(project, 320, 200);
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    let device = ct.content_pipeline.native_device_handle().unwrap();
    let bridge = Arc::new(SharedTextureBridge::new(320, 200));
    ct.content_pipeline.set_node_preview_textures(
        std::array::from_fn(|slot| unsafe { bridge.import_texture_native(&device, slot) }),
        bridge.clone(),
    );
    let (tx, rx) = crossbeam_channel::unbounded();
    // Give the off-thread proxy preparation a bounded paused warmup.
    for _ in 0..4 {
        tick(&mut ct, &tx, &rx);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    ct.handle_command(ContentCommand::Play);
    for _ in 0..4 {
        tick(&mut ct, &tx, &rx);
    }
    ct.handle_command(ContentCommand::Pause);

    let inspect = |ct: &mut ContentThread, index: usize| {
        ct.handle_command(ContentCommand::WatchGraphTarget(Some(
            targets[index].clone(),
        )));
        ct.handle_command(ContentCommand::SetGraphPreviewNode(Some(NodeId::new(
            "fluid_surface",
        ))));
        let mut accepted = None;
        let mut last_outputs = Vec::new();
        for _ in 0..60 {
            let state = tick(ct, &tx, &rx);
            let outputs = &state
                .node_preview_info
                .as_ref()
                .expect("liquid diagnostics")
                .outputs;
            let value = |name: &str| {
                outputs
                    .iter()
                    .find(|(port, _)| port == name)
                    .map_or(0.0, |(_, value)| *value)
            };
            if value("vertex_count") > 0.0
                && value("simulation_time") > 0.0
                && value("lag_seconds") <= 1e-6
            {
                accepted = Some((
                    value("simulation_time"),
                    value("vertex_count"),
                    state.current_time.0,
                ));
                break;
            }
            last_outputs.clone_from(outputs);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let accepted = accepted.unwrap_or_else(|| {
            panic!("liquid {index} did not retain a complete frame: {last_outputs:?}")
        });
        ct.handle_command(ContentCommand::SetGraphPreviewNode(Some(NodeId::new(
            "scene",
        ))));
        let navigation = SceneViewportNavigation::new(320, 200);
        let request = navigation.request(&targets[index], &NodeId::new("scene"), None, None);
        ct.handle_command(ContentCommand::SetSceneViewport(Some(request.clone())));
        // A new owner/session may still have an older image in the surface
        // ring. Follow the UI's matching protocol before reading its metadata.
        let mut published = None;
        for _ in 0..4 {
            let state = tick(ct, &tx, &rx);
            let lease = bridge.acquire_read();
            if let Some(frame_id) = bridge.leased_frame(lease)
                && let Some(frame) = &state.scene_viewport_frames[lease.slot()]
                && frame.matches(&request, frame_id, bridge.generation())
            {
                published = Some(frame.clone());
            }
            bridge.retire_read(lease);
            if published.is_some() {
                break;
            }
        }
        let frame = published.expect("matching viewport frame after owner switch");
        let domain = frame
            .domains
            .iter()
            .find(|(node, _)| node.as_str() == "fluid_surface")
            .unwrap()
            .1;
        assert_eq!(domain.state, FluidDomainState::Ready);
        (accepted, domain, scene_pixels(ct, &device, index))
    };
    let baselines = [inspect(&mut ct, 0), inspect(&mut ct, 1)];
    for index in [0, 1, 0, 1] {
        let current = inspect(&mut ct, index);
        assert_eq!(
            current.0, baselines[index].0,
            "paused editor switch changed simulation time or geometry"
        );
        assert_eq!(
            current.1, baselines[index].1,
            "paused editor switch changed the accepted epoch"
        );
        assert!(
            current.2 == baselines[index].2,
            "paused editor switch changed the rendered liquid"
        );
    }
    ct.handle_command(ContentCommand::WatchGraphTarget(None));
    tick(&mut ct, &tx, &rx);
    assert_eq!(
        inspect(&mut ct, 0).0,
        baselines[0].0,
        "closing editor reset physics"
    );

    ct.handle_command(ContentCommand::Play);
    for index in [1, 0, 1, 0] {
        ct.handle_command(ContentCommand::WatchGraphTarget(Some(
            targets[index].clone(),
        )));
        tick(&mut ct, &tx, &rx);
    }
    ct.handle_command(ContentCommand::Pause);
    for (index, baseline) in baselines.iter().enumerate() {
        let current = inspect(&mut ct, index);
        assert_eq!(
            current.1.epoch, baseline.1.epoch,
            "playing editor switch restarted the worker"
        );
        let transport_delta = current.0.2 - baseline.0.2;
        assert!(transport_delta > 0.0);
        assert!(
            (f64::from(current.0.0 - baseline.0.0) - transport_delta).abs() < 1e-6,
            "playing rebuild lost simulation time: before={:?}, after={:?}",
            baseline.0,
            current.0
        );
    }
}

#[test]
fn shared_scene_viewport_content_bridge_and_editor_painter() {
    let project = project();
    let target = GraphTarget::Generator(project.timeline.layers[0].layer_id.clone());
    let mut ct = crate::headless_harness::headless_content_thread(project, 320, 200);
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    let device = ct.content_pipeline.native_device_handle().unwrap();
    let bridge = Arc::new(SharedTextureBridge::new(320, 200));
    // Both native devices normally import the same surfaces. This proof uses
    // one device but still traverses the production bridge/lease protocol.
    let textures =
        std::array::from_fn(|slot| unsafe { bridge.import_texture_native(&device, slot) });
    ct.content_pipeline
        .set_node_preview_textures(textures, bridge.clone());
    let ui_textures: [_; 3] =
        std::array::from_fn(|slot| unsafe { bridge.import_texture_native(&device, slot) });
    let (tx, rx) = crossbeam_channel::unbounded();
    ct.handle_command(ContentCommand::WatchGraphTarget(Some(target.clone())));
    ct.handle_command(ContentCommand::SetGraphPreviewNode(Some(NodeId::new(
        "fluid_surface",
    ))));
    tick(&mut ct, &tx, &rx);
    ct.handle_command(ContentCommand::Play);
    for _ in 0..3 {
        tick(&mut ct, &tx, &rx);
    }
    ct.handle_command(ContentCommand::Pause);
    let mut liquid_ready = false;
    let mut last_outputs = Vec::new();
    for _ in 0..60 {
        let state = tick(&mut ct, &tx, &rx);
        let outputs = &state
            .node_preview_info
            .as_ref()
            .expect("fluid diagnostic preview")
            .outputs;
        let scalar = |name: &str| {
            outputs
                .iter()
                .find(|(port, _)| port == name)
                .map(|(_, value)| *value)
                .unwrap_or(0.0)
        };
        if scalar("vertex_count") > 0.0
            && scalar("simulation_time") > 0.0
            && scalar("lag_seconds") <= 1e-6
        {
            liquid_ready = true;
            break;
        }
        last_outputs.clone_from(outputs);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        liquid_ready,
        "proof requires a meshed liquid at the paused transport time: {last_outputs:?}"
    );
    ct.handle_command(ContentCommand::WatchGraphTarget(Some(target.clone())));
    ct.handle_command(ContentCommand::SetGraphPreviewNode(Some(NodeId::new(
        "scene",
    ))));
    let mut navigation = SceneViewportNavigation::new(320, 200);
    navigation.config.camera.target = [0.0, 1.0, 0.0];
    let mut request = navigation.request(&target, &NodeId::new("scene"), None, None);
    ct.handle_command(ContentCommand::SetSceneViewport(Some(request.clone())));
    let mut ready = None;
    for _ in 0..60 {
        let state = tick(&mut ct, &tx, &rx);
        let (lease, frame) = observation(&state, &bridge, &request);
        let complete = frame.has_image()
            && frame
                .domains
                .iter()
                .any(|(_, domain)| domain.state == FluidDomainState::Ready);
        bridge.retire_read(lease);
        if complete {
            ready = Some(frame);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let ready = ready.expect("fluid must become ready within 60 bounded frames");
    let baseline = selected_scene_pixels(&ct, &device);
    let accepted = ready.domains;
    let mut ui = UIRenderer::new(&device, GpuTextureFormat::Rgba16Float);
    let painted = RenderTarget::new(
        &device,
        360,
        240,
        GpuTextureFormat::Rgba16Float,
        "shared viewport UI proof",
    );
    let mut lines = grid_lines(10.0, 1.0);
    for (_, domain) in accepted.iter() {
        if let Some(layout) = domain.accepted_layout {
            lines.extend(
                manifold_renderer::node_graph::viewport_overlay::fluid_domain_lines(layout),
            );
        }
    }
    let mut previous_pixels = None;
    for turn in 0..2 {
        if turn > 0 {
            crate::viewport_input::apply(
                navigation.camera_mut(),
                crate::viewport_input::ViewportGesture::Orbit {
                    dx: 100.0,
                    dy: 20.0,
                },
                &crate::viewport_input::ViewportInputSensitivity::default(),
            );
            request = navigation.request(&target, &NodeId::new("scene"), None, Some(&request));
            ct.handle_command(ContentCommand::SetSceneViewport(Some(request.clone())));
        }
        let state = tick(&mut ct, &tx, &rx);
        let (lease, frame) = observation(&state, &bridge, &request);
        assert!(frame.has_image(), "{:?}", frame.status);
        assert_eq!(frame.request.config, request.config);
        assert_eq!(
            &*frame.domains, &*accepted,
            "navigation changed accepted simulation state"
        );
        assert!(
            baseline == selected_scene_pixels(&ct, &device),
            "editor camera changed the selected scene output on turn {turn}"
        );
        ui.register_external_texture(
            crate::scene_viewport::texture_handle(),
            ui_textures[lease.slot()].clone(),
        );
        ui.begin_frame();
        SceneViewportPaint {
            rect: manifold_ui::Rect::new(20.0, 20.0, 320.0, 200.0),
            frame: &frame,
            lines: &lines,
        }
        .draw(&mut ui);
        assert!(ui.prepare(&device, 360, 240, 1.0));
        let mut enc = device.create_encoder("shared viewport editor paint");
        enc.clear_texture(&painted.texture, 0.015, 0.015, 0.02, 1.0);
        ui.render(&mut enc, &painted.texture, GpuLoadAction::Load);
        enc.commit_and_wait_completed();
        bridge.retire_read(lease);
        let pixels = readback_srgb_rgba8(&device, &painted.texture, 360, 240);
        assert!(
            pixels.chunks_exact(4).any(|pixel| pixel != &pixels[..4]),
            "editor image is uniform"
        );
        if let Some(previous) = previous_pixels {
            assert_ne!(previous, pixels, "orbit did not change editor framing");
        }
        std::fs::write(
            format!("/private/tmp/shared-scene-viewport-app-{turn}.png"),
            encode_rgba8_png(&pixels, 360, 240),
        )
        .unwrap();
        previous_pixels = Some(pixels);
    }
    ct.handle_command(ContentCommand::SetSceneViewport(None));
    let state = tick(&mut ct, &tx, &rx);
    assert!(
        state.scene_viewport_frames.iter().all(Option::is_none),
        "closing retained viewport metadata"
    );
    assert_eq!(baseline, selected_scene_pixels(&ct, &device));
    assert!(!ct.engine.is_playing(), "proof must cover paused authoring");
}

#[test]
fn shared_scene_viewport_effect_rebuild_and_inactive_owner() {
    scene_effect_rebuild_and_inactive_owner(true);
}

#[test]
fn shared_scene_viewport_source_independent_effect() {
    scene_effect_rebuild_and_inactive_owner(false);
}

fn scene_effect_rebuild_and_inactive_owner(consume_source: bool) {
    use manifold_core::{clip::TimelineClip, effects::PresetInstance, layer::Layer};
    let mut project = manifold_core::project::Project::default();
    project.settings.output_width = 160;
    project.settings.output_height = 100;
    let mut layer = Layer::new_generator("Source".into(), PresetTypeId::PLASMA, 0);
    layer
        .clips
        .push(TimelineClip::new_generator(Beats::ZERO, Beats(4.0)));
    let mut def: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(
        include_str!("../../manifold-renderer/assets/generator-presets/Scene.json"),
    )
    .unwrap();
    def.nodes
        .iter_mut()
        .find(|node| node.type_id == "system.generator_input")
        .unwrap()
        .type_id = "system.source".into();
    if consume_source {
        // The upstream image supplies the environment in a consuming effect.
        // Otherwise the scene uses its own environment and replaces the input.
        def.wires
            .retain(|wire| !(wire.to_node == 30 && wire.to_port == "envmap"));
        def.wires
            .push(manifold_core::effect_graph_def::EffectGraphWire {
                from_node: 0,
                from_port: "out".into(),
                to_node: 30,
                to_port: "envmap".into(),
            });
    }
    let mut effect = PresetInstance::new(PresetTypeId::new("Scene"));
    effect.graph = Some(def);
    effect.refresh_manifest_from_graph();
    let effect_id = effect.id.clone();
    layer.effects = Some(vec![effect]);
    project.timeline.layers.push(layer);
    let target = GraphTarget::Effect(effect_id.clone());
    let mut ct = crate::headless_harness::headless_content_thread(project, 160, 100);
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    let device = ct.content_pipeline.native_device_handle().unwrap();
    let bridge = Arc::new(SharedTextureBridge::new(160, 100));
    ct.content_pipeline.set_node_preview_textures(
        std::array::from_fn(|slot| unsafe { bridge.import_texture_native(&device, slot) }),
        bridge.clone(),
    );
    let (tx, rx) = crossbeam_channel::unbounded();
    ct.handle_command(ContentCommand::WatchGraphTarget(Some(target.clone())));
    ct.handle_command(ContentCommand::SetGraphPreviewNode(Some(NodeId::new(
        "scene",
    ))));
    let navigation = SceneViewportNavigation::new(160, 100);
    let request = navigation.request(&target, &NodeId::new("scene"), None, None);
    ct.handle_command(ContentCommand::SetSceneViewport(Some(request.clone())));
    let state = tick(&mut ct, &tx, &rx);
    let (lease, frame) = observation(&state, &bridge, &request);
    bridge.retire_read(lease);
    assert!(
        frame.has_image(),
        "first effect-chain frame: {:?}",
        frame.status
    );

    // Real topology edit forces a chain replacement; its first frame must
    // already carry the requested camera, without one stale/missing frame.
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
        let effect = project.timeline.layers[0]
            .effects
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|effect| effect.id == effect_id)
            .unwrap();
        effect.graph.as_mut().unwrap().nodes.push(
            serde_json::from_value(serde_json::json!({
                "id": 90, "nodeId": "new-transform", "typeId": "node.transform_3d"
            }))
            .unwrap(),
        );
        effect.bump_graph_structure_version();
    })));
    let state = tick(&mut ct, &tx, &rx);
    let (lease, frame) = observation(&state, &bridge, &request);
    bridge.retire_read(lease);
    assert!(
        frame.has_image(),
        "rebuilt effect-chain frame: {:?}",
        frame.status
    );
    assert_eq!(frame.request.config, request.config);
    ct.handle_command(ContentCommand::MutateProject(Box::new(|project| {
        project.timeline.layers[0].effects.as_mut().unwrap()[0].enabled = false;
    })));
    let state = tick(&mut ct, &tx, &rx);
    let (lease, frame) = observation(&state, &bridge, &request);
    bridge.retire_read(lease);
    assert!(!frame.has_image(), "inactive scene exposed an old capture");
    assert!(frame.diagnostic().is_some());
}
