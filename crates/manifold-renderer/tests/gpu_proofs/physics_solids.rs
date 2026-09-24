//! Complete PhysicsSolids graph proof.
//!
//! This renders the shipped preset through the production `PresetRuntime`,
//! including CPU Box3D simulation, compact Platonic mesh upload, scene
//! objects, PBR materials, camera, lights, and final output. The same runtime
//! is advanced at a fixed 1/60 second from frame 0 through frame 120 so the
//! readback comparison measures actual simulated motion rather than a fresh
//! runtime's initialization difference.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const PHYSICS_SOLIDS_JSON: &str = include_str!("../../assets/generator-presets/PhysicsSolids.json");
const FRAME_COUNT: u32 = 120;

/// Production import, scene commands, saved graph and native physics together.
#[test]
fn physics_imported_flower_enable_split_render_and_reset() {
    use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, GraphTarget};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::{
        EnableSceneObjectPhysicsCommand, SplitSceneObjectCommand,
    };
    use manifold_renderer::node_graph::{
        gltf_import::assemble_import_graph, scene_exposure::metadata_for_node_type,
    };

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported, _) = assemble_import_graph(&fixture).expect("original flower imports");
    let render_id = imported
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .unwrap()
        .id;
    let registry = PrimitiveRegistry::with_builtin();
    let h = harness::shared();
    for split in [false, true] {
        let mut project = Project::default();
        let preset_id = imported.preset_metadata.as_ref().unwrap().id.clone();
        let layer_index =
            project
                .timeline
                .add_layer("Flower Physics", LayerType::Generator, preset_id);
        let layer = &mut project.timeline.layers[layer_index];
        layer.gen_params_or_init().graph = Some(imported.clone());
        layer
            .clips
            .push(manifold_core::clip::TimelineClip::new_generator(
                Beats(0.0),
                Beats(16.0),
            ));
        let target = GraphTarget::Generator(layer.layer_id.clone());
        let mut enable = EnableSceneObjectPhysicsCommand::new(
            target.clone(),
            render_id,
            0,
            metadata_for_node_type("node.rigid_body"),
            imported.clone(),
        )
        .with_world_metadata(metadata_for_node_type("node.physics_world"));
        enable.execute(&mut project);
        assert!(
            enable.was_applied(),
            "enable rejected: {:?}",
            enable.rejection_reason()
        );
        if split {
            let mut command = SplitSceneObjectCommand::new(
                target,
                render_id,
                0,
                metadata_for_node_type("node.rigid_body"),
                imported.clone(),
            );
            command.execute(&mut project);
            assert!(
                command.was_applied(),
                "split rejected: {:?}",
                command.rejection_reason()
            );
        }
        let mut def = project.timeline.layers[layer_index]
            .generator_graph()
            .unwrap()
            .clone();
        let world = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.physics_world")
            .unwrap()
            .id;
        // Same floor mesh and collider mapping as the shipped Physics Solids.
        let ground: EffectGraphDef = serde_json::from_str(PHYSICS_SOLIDS_JSON).unwrap();
        let object_index = def
            .wires
            .iter()
            .filter(|w| w.to_node == render_id && w.to_port.starts_with("object_"))
            .count();
        for mut node in ground
            .nodes
            .into_iter()
            .filter(|n| (100..105).contains(&n.id))
        {
            node.id += 100_000;
            if node.type_id == "node.transform_3d" {
                node.params
                    .insert("pos_y".into(), SerializedParamValue::Float { value: -1.5 });
                node.params.insert(
                    "scale_x".into(),
                    SerializedParamValue::Float { value: 20.0 },
                );
                node.params.insert(
                    "scale_z".into(),
                    SerializedParamValue::Float { value: 20.0 },
                );
            }
            def.nodes.push(node);
        }
        for mut wire in ground
            .wires
            .into_iter()
            .filter(|w| (100..105).contains(&w.from_node) || (100..105).contains(&w.to_node))
        {
            if (100..105).contains(&wire.from_node) {
                wire.from_node += 100_000;
            }
            if (100..105).contains(&wire.to_node) {
                wire.to_node += 100_000;
            }
            if wire.to_node == 40 {
                wire.to_node = world;
                wire.to_port = "body_63".into();
            }
            if wire.from_node == 40 {
                wire.from_node = world;
                wire.from_port = "pose_63".into();
            }
            if wire.to_node == 30 {
                wire.to_node = render_id;
                wire.to_port = format!("object_{object_index}");
            }
            def.wires.push(wire);
        }
        def.nodes
            .iter_mut()
            .find(|n| n.id == render_id)
            .unwrap()
            .params
            .insert(
                "objects".into(),
                SerializedParamValue::Float {
                    value: (object_index + 1) as f32,
                },
            );
        project.timeline.layers[layer_index]
            .gen_params_or_init()
            .graph = Some(def.clone());
        project.embedded_presets.push(EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def: def.clone(),
            origin: EmbeddedOrigin::Saved,
        });
        let label = if split { "split" } else { "intact" };
        let output = std::env::temp_dir().join(format!("manifold-standard-box3d-{label}.manifold"));
        manifold_io::saver::save_project_v1(&project, &output).unwrap();
        let reloaded = manifold_io::loader::load_project(&output).unwrap();
        let def = reloaded.timeline.layers[layer_index]
            .generator_graph()
            .unwrap()
            .clone();
        let mut runtime = PresetRuntime::from_def_with_device(
            def,
            &registry,
            h.device.clone(),
            h.width,
            h.height,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("edited and saved graph builds");
        let target = h.make_target("imported-physics");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            render_frame(&mut runtime, &target, 0, h.width, h.height, &h.device);
            assert!(
                runtime.errors().is_empty(),
                "imported physics errors: {:?}",
                runtime.errors()
            );
            if !runtime.warmup_pending() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "collider warmup timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        render_frame(&mut runtime, &target, 0, h.width, h.height, &h.device);
        let initial = h.readback(&target.texture);
        assert!(
            pixel_stats(&initial).0 > 1.0,
            "flower must render before simulation"
        );
        std::fs::write(
            format!("/tmp/standard-box3d-{label}-initial.png"),
            manifold_renderer::headless_readback::readback_to_srgb_png(
                &h.device,
                &target.texture,
                h.width,
                h.height,
            ),
        )
        .unwrap();
        for frame in 1..=120 {
            render_frame(&mut runtime, &target, frame, h.width, h.height, &h.device);
        }
        assert!(
            runtime.errors().is_empty(),
            "simulation errors: {:?}",
            runtime.errors()
        );
        let settled = h.readback(&target.texture);
        assert!(
            mean_abs_diff(&initial, &settled) > 0.001,
            "imported physics must move visible geometry"
        );
        std::fs::write(
            format!("/tmp/standard-box3d-{label}-settled.png"),
            manifold_renderer::headless_readback::readback_to_srgb_png(
                &h.device,
                &target.texture,
                h.width,
                h.height,
            ),
        )
        .unwrap();
        render_frame(&mut runtime, &target, 0, h.width, h.height, &h.device);
        let reset = h.readback(&target.texture);
        assert!(
            mean_abs_diff(&initial, &reset) < 0.002,
            "transport reset must restore imported poses"
        );
    }
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &manifold_renderer::render_target::RenderTarget,
    frame: u32,
    width: u32,
    height: u32,
    device: &manifold_gpu::GpuDevice,
) {
    let seconds = frame as f64 / 60.0;
    let context = PresetContext {
        time: seconds,
        beat: seconds,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    let mut encoder = device.create_encoder("physics-solids-render");
    {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        runtime.render(
            &mut gpu,
            &target.texture,
            &context,
            &manifold_core::params::ParamManifest::default(),
        );
    }
    encoder.commit_and_wait_completed();
}

fn pixel_stats(bytes: &[u8]) -> (f64, f32) {
    let mut luma_sum = 0.0f64;
    let mut peak = 0.0f32;
    for pixel in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([pixel[0], pixel[1]]).to_f32();
        let g = f16::from_le_bytes([pixel[2], pixel[3]]).to_f32();
        let b = f16::from_le_bytes([pixel[4], pixel[5]]).to_f32();
        let a = f16::from_le_bytes([pixel[6], pixel[7]]).to_f32();
        assert!(
            r.is_finite() && g.is_finite() && b.is_finite() && a.is_finite(),
            "PhysicsSolids produced a non-finite pixel"
        );
        luma_sum += (0.2126 * r + 0.7152 * g + 0.0722 * b) as f64;
        peak = peak.max(r.max(g).max(b));
    }
    (luma_sum, peak)
}

fn mean_abs_diff(before: &[u8], after: &[u8]) -> f64 {
    assert_eq!(before.len(), after.len());
    let mut sum = 0.0f64;
    for (a, b) in before.chunks_exact(2).zip(after.chunks_exact(2)) {
        let av = f16::from_le_bytes([a[0], a[1]]).to_f32();
        let bv = f16::from_le_bytes([b[0], b[1]]).to_f32();
        assert!(av.is_finite() && bv.is_finite());
        sum += f64::from((av - bv).abs());
    }
    sum / (before.len() / 2) as f64
}

#[test]
fn physics_solids_renders_finite_nonempty_scene_and_moves() {
    let harness = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        PHYSICS_SOLIDS_JSON,
        &registry,
        std::sync::Arc::clone(&harness.device),
        harness.width,
        harness.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("PhysicsSolids graph must build: {error}"));
    let target = harness.make_target("physics-solids-proof");

    render_frame(
        &mut runtime,
        &target,
        0,
        harness.width,
        harness.height,
        &harness.device,
    );
    let initial = harness.readback(&target.texture);
    std::fs::write(
        "/tmp/physics_solids_initial.png",
        manifold_renderer::headless_readback::readback_to_srgb_png(
            &harness.device,
            &target.texture,
            harness.width,
            harness.height,
        ),
    )
    .unwrap();

    for frame in 1..=FRAME_COUNT {
        render_frame(
            &mut runtime,
            &target,
            frame,
            harness.width,
            harness.height,
            &harness.device,
        );
    }
    let settled = harness.readback(&target.texture);

    std::fs::write(
        "/tmp/physics_solids_settled.png",
        manifold_renderer::headless_readback::readback_to_srgb_png(
            &harness.device,
            &target.texture,
            harness.width,
            harness.height,
        ),
    )
    .unwrap();

    let (initial_luma, initial_peak) = pixel_stats(&initial);
    let (settled_luma, settled_peak) = pixel_stats(&settled);
    let motion = mean_abs_diff(&initial, &settled);
    eprintln!(
        "PhysicsSolids GPU proof: initial_luma={initial_luma:.3} settled_luma={settled_luma:.3} \
         initial_peak={initial_peak:.3} settled_peak={settled_peak:.3} mean_abs_diff={motion:.6} \
         artifacts=/tmp/physics_solids_initial.png,/tmp/physics_solids_settled.png"
    );

    assert!(initial_peak > 0.02, "initial PhysicsSolids frame is empty");
    assert!(settled_peak > 0.02, "settled PhysicsSolids frame is empty");
    assert!(
        motion > 0.0005,
        "120 simulated frames must change rendered pixels; mean_abs_diff={motion:.6}"
    );
}

#[test]
fn physics_nonlinear_animated_graph_matches_irregular_frame_delivery() {
    let harness = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut def: serde_json::Value = serde_json::from_str(PHYSICS_SOLIDS_JSON).unwrap();
    for node in def["nodes"].as_array_mut().unwrap() {
        match node["id"].as_u64() {
            Some(111) => {
                node["params"]["motion"] = serde_json::json!({ "type": "Enum", "value": 2 })
            }
            Some(120) => {
                node["params"]["pos_x"] = serde_json::json!({ "type": "Float", "value": 0.0 });
                node["params"]["pos_y"] = serde_json::json!({ "type": "Float", "value": 3.8 });
            }
            _ => {}
        }
    }
    def["nodes"].as_array_mut().unwrap().push(serde_json::json!({
        "id": 500, "nodeId": "nonlinear_animated_x", "typeId": "node.lfo",
        "params": {
            "rate_mode": { "type": "Enum", "value": 1 },
            "angular_rate": { "type": "Float", "value": 188.49556 },
            "phase": { "type": "Float", "value": 0.75 },
            "min": { "type": "Float", "value": -2.0 },
            "max": { "type": "Float", "value": 2.0 }
        }
    }));
    def["wires"].as_array_mut().unwrap().push(serde_json::json!({
        "fromNode": 500, "fromPort": "out", "toNode": 110, "toPort": "pos_x"
    }));
    let json = serde_json::to_string(&def).unwrap();
    let build = || {
        PresetRuntime::from_json_str_with_device(
            &json,
            &registry,
            std::sync::Arc::clone(&harness.device),
            harness.width,
            harness.height,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("nonlinear PhysicsSolids graph builds")
    };
    let mut regular = build();
    let mut irregular = build();
    let regular_target = harness.make_target("physics-nonlinear-regular");
    let irregular_target = harness.make_target("physics-nonlinear-irregular");
    for frame in 0..=8 {
        render_frame(&mut regular, &regular_target, frame, harness.width, harness.height, &harness.device);
        if frame % 4 == 0 {
            render_frame(&mut irregular, &irregular_target, frame, harness.width, harness.height, &harness.device);
        }
    }
    let regular_image = harness.readback(&regular_target.texture);
    let irregular_image = harness.readback(&irregular_target.texture);
    let diff = mean_abs_diff(&regular_image, &irregular_image);
    assert!(diff < 0.002, "nonlinear Animated contact/render diverged under irregular delivery: mean_abs_diff={diff:.6}");
}
