//! Complete PhysicsSolids graph proof.
//!
//! This renders the shipped preset through the production `PresetRuntime`,
//! including CPU Box3D simulation, compact Platonic mesh upload, scene
//! objects, PBR materials, camera, lights, and final output. The same runtime
//! is advanced at a fixed 1/60 second from frame 0 through frame 120 so the
//! readback comparison measures actual simulated motion rather than a fresh
//! runtime's initialization difference.

use half::f16;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;


const PHYSICS_SOLIDS_JSON: &str = include_str!("../../assets/generator-presets/PhysicsSolids.json");
const FRAME_COUNT: u32 = 120;

/// Compound scan proof for the scene-modifier path.  The importer keeps the
/// tiger lily as one authored object with all material sources intact; Shatter
/// expands that row into internal pieces that follow the parent until the manual release control is raised.
#[test]
fn physics_imported_flower_shatter_release_preserves_authored_row_and_materials() {
    use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
    use manifold_core::project::Project;
    use manifold_core::scene_modifier_edit::insert_scene_modifier;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, GraphTarget, NodeId};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand;
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;
    use manifold_nodes_scene::node_graph::scene_modifier_authoring::{prepare_new_scene_modifier, scene_modifier_objects};
    use manifold_node_engine::load::expand::expand_scene_modifiers;
    use manifold_nodes_scene::node_graph::scene_vm::SceneVm;

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported, _report) = assemble_import_graph(&fixture).expect("original flower imports");
    let render = imported
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("render scene");
    let render_ref = SceneNodeRef {
        scope: Vec::new(),
        node: render.node_id.clone(),
    };
    let mut project = Project::default();
    let preset_id = imported.preset_metadata.as_ref().unwrap().id.clone();
    let layer_index = project
        .timeline
        .add_layer("Flower Shatter", LayerType::Generator, preset_id);
    project.timeline.layers[layer_index]
        .gen_params_or_init()
        .graph = Some(imported.clone());
    project.timeline.layers[layer_index].clips.push(
        manifold_core::clip::TimelineClip::new_generator(Beats(0.0), Beats(16.0)),
    );
    let target = GraphTarget::Generator(project.timeline.layers[layer_index].layer_id.clone());
    let mut enable = EnableSceneObjectPhysicsCommand::new(
        target.clone(),
        render.id,
        0,
        manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.rigid_body"),
        imported.clone(),
    )
    .with_world_metadata(
        manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.physics_world"),
    );
    enable.execute(&mut project);
    assert!(
        enable.was_applied(),
        "enable rejected: {:?}",
        enable.rejection_reason()
    );
    let mut enabled = project.timeline.layers[layer_index]
        .generator_graph()
        .unwrap()
        .clone();
    // A fixed intact parent makes post-trigger falling evidence of release,
    // rather than merely the intact flower continuing its existing fall.
    for group in enabled.nodes.iter_mut().filter_map(|n| n.group.as_mut()) {
        for body in group
            .nodes
            .iter_mut()
            .filter(|n| n.type_id == "node.rigid_body")
        {
            body.params
                .insert("motion".into(), SerializedParamValue::Enum { value: 0 });
        }
    }
    let authored_objects =
        scene_modifier_objects(&enabled, &render_ref).expect("scene objects resolve");
    assert!(
        authored_objects.len() >= 2,
        "compound scan preserves each material scene object"
    );
    let primary_object = authored_objects[0].clone();
    let authored_vm = SceneVm::from_def(&enabled).expect("authored scene resolves");
    assert_eq!(authored_vm.header.object_count, 1);
    assert_eq!(authored_vm.objects.len(), authored_objects.len() + 1);
    assert!(
        authored_vm.header.vertex_count > 0,
        "imported material triangle totals remain visible"
    );
    assert!(
        authored_vm.header.vertex_count_exact,
        "imported source totals remain exact"
    );
    let recipe: EffectGraphDef = serde_json::from_str(include_str!(
        "../../assets/scene-modifier-presets/Shatter.json"
    ))
    .expect("Shatter recipe parses");
    let instance = prepare_new_scene_modifier(
        &enabled,
        &recipe,
        NodeId::new("flower_shatter"),
        render_ref,
        SceneTargetSelection::Explicit {
            objects: vec![primary_object],
        },
    )
    .expect("Shatter captures imported source frames");
    assert!(
        !instance.mesh_frames.is_empty(),
        "Shatter captures source mesh frames"
    );
    let attached = insert_scene_modifier(&enabled, enabled.scene_modifiers.len(), instance)
        .expect("attach Shatter")
        .graph;
    let registry = PrimitiveRegistry::with_builtin();
    let expanded = expand_scene_modifiers(&attached, &registry).expect("Shatter expands");
    assert!(
        expanded
            .nodes
            .iter()
            .any(|node| node.type_id == "node.rigid_body")
    );
    let object_count = expanded
        .nodes
        .iter()
        .find(|node| node.node_id == render.node_id)
        .and_then(|node| node.params.get("objects"));
    assert_eq!(
        object_count,
        Some(&SerializedParamValue::Float { value: 16.0 })
    );
    assert_eq!(
        expanded
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.rigid_body")
            .filter(|node| node.params.contains_key("fragment_parent"))
            .count(),
        16,
        "Shatter creates sixteen dormant child bodies"
    );

    let metadata = attached
        .preset_metadata
        .as_ref()
        .expect("authored metadata")
        .clone();
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mut runtime = PresetRuntime::from_def_with_device(
        attached,
        &registry,
        h.device.clone(),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("Shatter runtime");
    let target = h.make_target("imported-flower-shatter");
    let idle_params = ParamManifest::from_params(
        metadata
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    let mut released_params = idle_params.clone();
    let shatter_binding = metadata
        .bindings
        .iter()
        .find(|binding| {
            matches!(
                &binding.target,
                manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                    modifier_id,
                    param_id
                } if *modifier_id == NodeId::new("flower_shatter") && param_id == "shatter"
            )
        })
        .expect("Shatter trigger binding");
    let trigger = released_params
        .get_mut(&shatter_binding.id)
        .expect("Shatter trigger manifest slot");
    trigger.value = 1.0;
    trigger.base = 1.0;

    // Warm the imported convex hull on the same runtime that will receive the
    // release. A fresh released runtime would bypass the baseline latch.
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("collider warmup");
    let mut frame = 0;
    loop {
        render_frame_with_params(
            &mut runtime,
            &target,
            frame,
            h.width,
            h.height,
            &h.device,
            &idle_params,
        );
        assert!(
            runtime.errors().is_empty(),
            "Shatter errors: {:?}",
            runtime.errors()
        );
        if !runtime.warmup_pending() {
            break;
        }
        wait.hold();
    }
    let idle_image = h.readback(&target.texture);
    assert!(
        pixel_stats(&idle_image).0 > 1.0,
        "the intact flower must remain visible before release"
    );

    for hold_frame in 1..=12 {
        render_frame_with_params(
            &mut runtime,
            &target,
            hold_frame,
            h.width,
            h.height,
            &h.device,
            &idle_params,
        );
    }
    assert!(
        mean_abs_diff(&idle_image, &h.readback(&target.texture)) < 0.002,
        "prepared fragments must stay with the fixed intact parent"
    );
    frame = 12;
    // Raise the authored Shatter trigger through its normal binding on the
    // warmed runtime, then advance that same instance.
    frame += 1;
    render_frame_with_params(
        &mut runtime,
        &target,
        frame,
        h.width,
        h.height,
        &h.device,
        &released_params,
    );
    assert!(
        runtime.errors().is_empty(),
        "released Shatter errors: {:?}",
        runtime.errors()
    );
    let released_initial = h.readback(&target.texture);
    assert!(
        pixel_stats(&released_initial).0 > 1.0,
        "released fragments must render"
    );
    for _step in 1..=24 {
        frame += 1;
        render_frame_with_params(
            &mut runtime,
            &target,
            frame,
            h.width,
            h.height,
            &h.device,
            &released_params,
        );
    }
    assert!(
        runtime.errors().is_empty(),
        "simulation errors: {:?}",
        runtime.errors()
    );
    std::fs::write(
        "/tmp/standard-box3d-shatter-released.png",
        manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(
            &h.device,
            &target.texture,
            h.width,
            h.height,
        ),
    )
    .unwrap();
    let released_settled = h.readback(&target.texture);
    assert!(
        mean_abs_diff(&released_initial, &released_settled) > 0.0005,
        "released pieces must move after trigger"
    );
    render_frame_with_params(
        &mut runtime,
        &target,
        0,
        h.width,
        h.height,
        &h.device,
        &idle_params,
    );
    assert!(
        mean_abs_diff(&idle_image, &h.readback(&target.texture)) < 0.002,
        "reset must restore the intact flower"
    );
    std::fs::write(
        "/tmp/standard-box3d-shatter-intact.png",
        manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(
            &h.device,
            &target.texture,
            h.width,
            h.height,
        ),
    )
    .unwrap();
}

#[test]
fn imported_flower_empty_scene_clears_and_restores() {
    use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported, _) = assemble_import_graph(&fixture).expect("original flower imports");
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mut visible = manifold_renderer::generators::registry::GeneratorRegistry::new(GpuTextureFormat::Rgba16Float)
        .create_with_override(h.device.clone(), &imported.preset_metadata.as_ref().unwrap().id,
            Some(&imported), h.width, h.height, false, None, None)
    .expect("visible flower graph builds");
    let metadata = imported.preset_metadata.as_ref().unwrap();
    let shown_params = ParamManifest::from_params(
        metadata
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    let mut hidden_params = shown_params.clone();
    let mut visibility_bindings = 0;
    for binding in &metadata.bindings {
        if matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { param, .. } if param == "visible")
        {
            let value = hidden_params
                .get_mut(&binding.id)
                .expect("visibility control");
            value.base = 0.0;
            value.value = 0.0;
            visibility_bindings += 1;
        }
    }
    assert!(
        visibility_bindings >= 2,
        "compound visibility fans out across materials"
    );
    let target = h.make_target("imported-flower-empty-scene");

    warm_imported_runtime(&mut visible, &target, &shown_params);
    assert!(
        visible.errors().is_empty(),
        "visible flower errors: {:?}",
        visible.errors()
    );
    let normal = h.readback(&target.texture);
    assert!(
        pixel_stats(&normal).0 > 1.0,
        "flower must render before hiding"
    );

    visible.apply_inner_param_overrides(&imported);
    render_frame_with_params(
        &mut visible,
        &target,
        1,
        h.width,
        h.height,
        &h.device,
        &hidden_params,
    );
    assert!(
        visible.errors().is_empty(),
        "hidden flower errors: {:?}",
        visible.errors()
    );
    let empty = h.readback(&target.texture);
    assert!(
        max_abs_pixel(&empty) < 1e-4,
        "all-hidden flower frame must clear the prior image, max_abs={:.6}",
        max_abs_pixel(&empty)
    );

    render_frame_with_params(
        &mut visible,
        &target,
        2,
        h.width,
        h.height,
        &h.device,
        &shown_params,
    );
    assert!(
        visible.errors().is_empty(),
        "restored flower errors: {:?}",
        visible.errors()
    );
    let restored = h.readback(&target.texture);
    assert!(mean_abs_diff(&normal, &restored) < 0.002, "restoring visibility must preserve the original appearance");
    assert!(
        pixel_stats(&restored).0 > 1.0,
        "flower must render after restoring visibility"
    );
}

#[test]
fn imported_flower_submesh_controls_preserve_siblings_and_parent_visibility() {
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_nodes_scene::node_graph::{gltf_import::assemble_import_graph, scene_vm::SceneVm, scene_vm::SceneObjectVm};
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported, _) = assemble_import_graph(&fixture).unwrap();
    let vm = SceneVm::from_def(&imported).unwrap();
    let SceneObjectVm::Known(child) = &vm.objects[1] else { panic!("flower child") };
    let group = imported.nodes.iter().find(|n| Some(n.id) == child.parent_group_id).unwrap().group.as_ref().unwrap();
    let object = group.nodes.iter().find(|n| n.id == child.object_node_id).unwrap();
    let transform = group.nodes.iter().find(|n| n.id == child.transform.as_ref().unwrap().node_doc_id).unwrap();
    let metadata = imported.preset_metadata.as_ref().unwrap();
    let binding_id = |node: &manifold_core::NodeId, param_name: &str| metadata.bindings.iter().find_map(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == node && param == param_name).then(|| binding.id.clone())
    }).unwrap();
    let visible_id = binding_id(&object.node_id, "visible");
    let parent_id = binding_id(&object.node_id, "parent_visible");
    let position_id = binding_id(&transform.node_id, "pos_x");
    let shown = ParamManifest::from_params(metadata.params.iter().cloned().map(Param::bundled).collect());
    let set = |params: &mut ParamManifest, id: &str, value: f32| {
        let param = params.get_mut(id).unwrap(); param.base = value; param.value = value;
    };
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mut runtime = manifold_renderer::generators::registry::GeneratorRegistry::new(GpuTextureFormat::Rgba16Float)
        .create_with_override(h.device.clone(), &metadata.id, Some(&imported), h.width, h.height, false, None, None).unwrap();
    let target = h.make_target("flower-submesh-controls");
    warm_imported_runtime(&mut runtime, &target, &shown);
    let original = h.readback(&target.texture);
    let mut edited = shown.clone();
    set(&mut edited, &visible_id, 0.0);
    render_frame_with_params(&mut runtime, &target, 1, h.width, h.height, &h.device, &edited);
    let sibling = h.readback(&target.texture);
    assert!(mean_abs_diff(&original, &sibling) > 0.001, "child eye must hide the flower");
    assert!(max_abs_pixel(&sibling) > 0.01, "calibration mesh remains visible");
    set(&mut edited, &parent_id, 0.0);
    render_frame_with_params(&mut runtime, &target, 2, h.width, h.height, &h.device, &edited);
    assert!(max_abs_pixel(&h.readback(&target.texture)) < 1e-4, "parent eye hides every child");
    set(&mut edited, &parent_id, 1.0);
    render_frame_with_params(&mut runtime, &target, 3, h.width, h.height, &h.device, &edited);
    assert!(mean_abs_diff(&sibling, &h.readback(&target.texture)) < 0.002, "parent eye preserves child visibility");
    set(&mut edited, &visible_id, 1.0);
    set(&mut edited, &position_id, 0.3);
    render_frame_with_params(&mut runtime, &target, 4, h.width, h.height, &h.device, &edited);
    assert!(mean_abs_diff(&original, &h.readback(&target.texture)) > 0.001, "local child transform changes the render");
    assert!(runtime.errors().is_empty(), "{:?}", runtime.errors());
    std::fs::write("/tmp/flower-submesh-controls.png", manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(&h.device, &target.texture, h.width, h.height)).unwrap();
}

#[test]
fn imported_flower_physics_off_renders_authored_transform() {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, GraphTarget};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::{
        EnableSceneObjectPhysicsCommand, SetGraphNodeParamCommand,
    };
    use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;
    use manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type;

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (imported, _) = assemble_import_graph(&fixture).expect("original flower imports");
    let render_id = imported
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render scene")
        .id;
    let mut project = Project::default();
    let preset_id = imported.preset_metadata.as_ref().unwrap().id.clone();
    let layer_index =
        project
            .timeline
            .add_layer("Flower Physics Off", LayerType::Generator, preset_id);
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
    let enabled_graph = project.timeline.layers[layer_index]
        .generator_graph()
        .expect("enabled graph").clone();
    let group = enabled_graph
        .nodes
        .iter()
        .find(|node| node.type_id == "group")
        .expect("imported object group");
    let body_id = group
        .group
        .as_ref()
        .and_then(|group| {
            group
                .nodes
                .iter()
                .find(|node| node.type_id == "node.rigid_body")
        })
        .expect("enabled rigid body")
        .id;
    let mut disable = SetGraphNodeParamCommand::new(
        target.clone(),
        body_id,
        "enabled".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false },
        imported.clone(),
    )
    .with_scope(vec![group.id]);
    disable.execute(&mut project);
    assert!(disable.was_applied(), "Physics OFF bool write was rejected");
    let def: EffectGraphDef = project.timeline.layers[layer_index]
        .generator_graph()
        .expect("disabled graph")
        .clone();
    let body = def
        .nodes
        .iter()
        .find(|node| node.type_id == "group")
        .and_then(|group| group.group.as_ref())
        .and_then(|group| {
            group
                .nodes
                .iter()
                .find(|node| node.type_id == "node.rigid_body")
        })
        .expect("Physics OFF keeps the body definition");
    assert_eq!(
        body.params.get("enabled"),
        Some(&manifold_core::effect_graph_def::SerializedParamValue::Bool { value: false }),
        "Physics OFF must disable the authored body through its bool parameter"
    );

    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let runtime = manifold_renderer::generators::registry::GeneratorRegistry::new(GpuTextureFormat::Rgba16Float)
        .create_with_override(h.device.clone(), &enabled_graph.preset_metadata.as_ref().unwrap().id,
            Some(&enabled_graph), h.width, h.height, false, None, None)
    .expect("physics-off flower graph builds");
    let target = h.make_target("imported-flower-physics-off");
    let mut runtime = runtime;
    warm_imported_runtime(&mut runtime, &target, &ParamManifest::default());
    let before = h.readback(&target.texture);
    runtime.apply_inner_param_overrides(&def);
    render_frame(&mut runtime, &target, 0, h.width, h.height, &h.device);
    let after = h.readback(&target.texture);
    std::fs::write("/tmp/flower-live-physics-off.png", manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(&h.device, &target.texture, h.width, h.height)).unwrap();
    assert!(mean_abs_diff(&before, &after) < 0.002,
        "paused Physics OFF must preserve appearance: diff={}", mean_abs_diff(&before, &after));
    assert!(
        runtime.errors().is_empty(),
        "Physics Off errors: {:?}",
        runtime.errors()
    );
    let output = h.readback(&target.texture);
    assert!(
        pixel_stats(&output).0 > 1.0,
        "Physics Off flower must render"
    );
}

fn warm_imported_runtime(
    runtime: &mut PresetRuntime,
    target: &manifold_node_engine::gpu::render_target::RenderTarget,
    params: &ParamManifest,
) {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("import warmup");
    loop {
        render_frame_with_params(runtime, target, 0, h.width, h.height, &h.device, params);
        assert!(runtime.errors().is_empty(), "import errors: {:?}", runtime.errors());
        if !runtime.warmup_pending() { break; }
        wait.hold();
    }
}

fn render_frame(
    runtime: &mut PresetRuntime,
    target: &manifold_node_engine::gpu::render_target::RenderTarget,
    frame: u32,
    width: u32,
    height: u32,
    device: &manifold_gpu::GpuDevice,
) {
    render_frame_with_params(
        runtime,
        target,
        frame,
        width,
        height,
        device,
        &ParamManifest::default(),
    );
}

fn render_frame_with_params(
    runtime: &mut PresetRuntime,
    target: &manifold_node_engine::gpu::render_target::RenderTarget,
    frame: u32,
    width: u32,
    height: u32,
    device: &manifold_gpu::GpuDevice,
    params: &ParamManifest,
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
        runtime.render(&mut gpu, &target.texture, &context, params);
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

fn max_abs_pixel(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(2)
        .map(|pixel| f16::from_le_bytes([pixel[0], pixel[1]]).to_f32().abs())
        .fold(0.0, f32::max)
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
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
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
        manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(
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
        manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(
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
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
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
    def["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": 500, "nodeId": "nonlinear_animated_x", "typeId": "node.lfo",
            "params": {
                "rate_mode": { "type": "Enum", "value": 1 },
                "angular_rate": { "type": "Float", "value": 188.49556 },
                "phase": { "type": "Float", "value": 0.75 },
                "min": { "type": "Float", "value": -2.0 },
                "max": { "type": "Float", "value": 2.0 }
            }
        }));
    def["wires"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
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
        render_frame(
            &mut regular,
            &regular_target,
            frame,
            harness.width,
            harness.height,
            &harness.device,
        );
        if frame % 4 == 0 {
            render_frame(
                &mut irregular,
                &irregular_target,
                frame,
                harness.width,
                harness.height,
                &harness.device,
            );
        }
    }
    let regular_image = harness.readback(&regular_target.texture);
    let irregular_image = harness.readback(&irregular_target.texture);
    let diff = mean_abs_diff(&regular_image, &irregular_image);
    assert!(
        diff < 0.002,
        "nonlinear Animated contact/render diverged under irregular delivery: mean_abs_diff={diff:.6}"
    );
}
