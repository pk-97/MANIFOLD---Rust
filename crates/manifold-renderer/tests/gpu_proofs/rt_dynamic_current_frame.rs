//! First-frame current-geometry dispatch proof for SCENE_MODIFIER_RT_DESIGN P5.
//!
//! This is a dispatch and frame-validity proof only: it does not claim
//! numerical ray-hit or image correctness. A tiny generated mesh is rendered
//! through the production `PresetRuntime` path exactly once, and the test
//! requires both a complete frame status and a real RT capture produced by
//! `render_scene`'s trace branch.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn modifier_combo_scene() -> manifold_core::effect_graph_def::EffectGraphDef {
    use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
    use manifold_core::scene_modifier_preset::{
        SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(
        "../fixtures/scene-modifiers/nested_multimaterial_v2.json"
    )).unwrap();
    owner.version = 3;
    let scene = owner.nodes.iter_mut().find(|node| node.type_id == "node.render_scene").unwrap();
    scene.params.insert("rt_enabled".into(), SerializedParamValue::Bool { value: true });
    let scene_ref = SceneNodeRef { scope: Vec::new(), node: scene.node_id.clone() };
    let scene_id = scene.id;
    // The structural fixture's PBR material needs a real environment when
    // rendered. Reuse the small procedural environment from the RT proofs.
    owner.nodes.push(serde_json::from_value(serde_json::json!({
        "id": 40, "nodeId": "combo_environment", "typeId": "node.bake_environment",
        "params": {
            "width": {"type": "Int", "value": 64},
            "height": {"type": "Int", "value": 32},
            "uniform": {"type": "Bool", "value": true}
        }
    })).unwrap());
    owner.wires.push(manifold_core::effect_graph_def::EffectGraphWire {
        from_node: 40, from_port: "envmap".into(), to_node: scene_id, to_port: "envmap".into(),
    });

    // Reuse the saved-frame convention of scene_modifier_expand::math_view's
    // deterministic fixture. Real stock recipes run on two tiny cube sources,
    // with no asynchronous import or private project asset dependency.
    let mut frames = Vec::new();
    for container in &owner.nodes {
        let Some(group) = &container.group else { continue };
        let source = group.nodes.iter().find(|node| node.type_id == "node.cube_mesh").unwrap();
        let object = group.nodes.iter().find(|node| node.type_id == "node.scene_object").unwrap();
        let transform = group.nodes.iter().find(|node| node.type_id == "node.transform_3d").unwrap();
        let scope = vec![container.node_id.clone()];
        frames.push(SceneMeshReferenceFrame {
            target: SceneNodeRef { scope: scope.clone(), node: object.node_id.clone() },
            source: SceneNodeRef { scope, node: source.node_id.clone() },
            source_definition_hash: manifold_core::scene_source_identity::scene_source_definition_hash(&owner, source).unwrap(),
            source_offset: ["pos_x", "pos_y", "pos_z"].map(|param| match transform.params.get(param) {
                Some(SerializedParamValue::Float { value }) => f64::from(*value),
                _ => 0.0,
            }),
            scene_radius: 3.0,
        });
    }
    for (id, json) in [
        ("vortex", include_str!("../../assets/scene-modifier-presets/VortexFragments.json")),
        ("recon", include_str!("../../assets/scene-modifier-presets/OrderedRecon.json")),
    ] {
        let recipe = serde_json::from_str(json).unwrap();
        let graph = manifold_renderer::node_graph::scene_modifier_authoring::initialize_scene_modifier_graph(&owner, &recipe).unwrap();
        let instance = SceneModifierInstanceDef {
            id: id.into(), scene: scene_ref.clone(), targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames.clone(), graph: Box::new(graph),
        };
        owner = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &owner, owner.scene_modifiers.len(), instance,
        ).unwrap().graph;
    }
    owner
}

fn scene_json() -> &'static str {
    r#"{"version":2,"name":"RtDynamicCurrentFrame","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":16},
            "resolution_x":{"type":"Int","value":2},
            "resolution_y":{"type":"Int","value":2},
            "size_x":{"type":"Float","value":2.0},
            "size_y":{"type":"Float","value":2.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"triangles","params":{
            "src_cols":{"type":"Int","value":2},
            "src_rows":{"type":"Int","value":2}}},
        {"id":3,"typeId":"node.phong_material","nodeId":"material","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.05}}},
        {"id":4,"typeId":"node.scene_object","nodeId":"object"},
        {"id":5,"typeId":"node.orbit_camera","nodeId":"camera","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":6.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":6,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_y":{"type":"Float","value":10.0},
            "aim_y":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1},
            "rt_enabled":{"type":"Bool","value":true}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
    ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":4,"toPort":"vertices"},
        {"fromNode":3,"fromPort":"out","toNode":4,"toPort":"material"},
        {"fromNode":4,"fromPort":"object","toNode":20,"toPort":"object_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
    ]}"#
}

#[test]
fn rt_dynamic_current_frame_first_frame_dispatches() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        scene_json(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("current-frame RT scene graph must build");
    let target = h.make_target("rt-dynamic-current-frame");
    let ctx = PresetContext {
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
    };
    let mut status = None;
    let captures = harness::capture_rt_channels(|| {
        let mut enc = h.device.create_encoder("rt-dynamic-current-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
            status = Some(gpu.frame_status());
        }
        enc.commit_and_wait_completed();
    });

    assert_eq!(
        status,
        Some(FrameRenderStatus::Complete),
        "the first current-frame RT update must produce a valid frame"
    );
    assert!(
        !captures.is_empty(),
        "the first evaluated frame must produce real RT captures"
    );
}

#[test]
fn rt_dynamic_current_frame_stock_modifier_combo_accepts_rt_and_dispatches() {
    use manifold_core::NodeId;
    use manifold_renderer::node_graph::ParamValue;
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let owner = modifier_combo_scene();
    assert!(manifold_core::scene_modifier_preset::scene_modifier_parameter_lock_reason(
        &owner, &NodeId::new("scan_render"), "rt_enabled",
    ).is_none(), "the editor must allow this stock vertex-modifier stack's RT control");
    let mut runtime = PresetRuntime::from_def_with_device(
        owner, &registry, std::sync::Arc::clone(&h.device), h.width, h.height,
        GpuTextureFormat::Rgba16Float, None,
    ).expect("Vortex Fragments + Ordered Recon must load with RT enabled");
    let scene = runtime.graph.instance_by_node_id(&NodeId::new("scan_render")).unwrap();
    let target = h.make_target("rt-modifier-combo");
    for (frame, enabled) in [true, false, true].into_iter().enumerate() {
        runtime.graph.set_param(scene, "rt_enabled", ParamValue::Bool(enabled))
            .expect("the prepared modifier runtime must allow live RT toggles");
        let ctx = PresetContext {
            time: frame as f64 / 24.0, beat: frame as f64 / 12.0, dt: 1.0 / 24.0,
            width: h.width, height: h.height, output_width: h.width, output_height: h.height,
            aspect: h.width as f32 / h.height as f32, owner_key: 0, is_clip_level: false,
            frame_count: frame as i64, anim_progress: 0.0, trigger_count: 0,
        };
        let mut status = None;
        let captures = harness::capture_rt_channels(|| {
            let mut enc = h.device.create_encoder("rt-modifier-combo");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
                runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
                status = Some(gpu.frame_status());
            }
            enc.commit_and_wait_completed();
        });
        assert_eq!(status, Some(FrameRenderStatus::Complete), "combo frame {frame}");
        assert_eq!(!captures.is_empty(), enabled, "RT dispatch must follow the live toggle on frame {frame}");
    }
}
