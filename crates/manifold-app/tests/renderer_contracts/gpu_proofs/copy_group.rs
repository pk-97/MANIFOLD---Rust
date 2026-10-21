//! A copy group after Make Copies (copy_group_design.md section 5): one added
//! object becomes a 12-copy Ring at 1.5x spacing through the production edit,
//! renders through the production `PresetRuntime`, and the gizmo draws over
//! the picture at the group's transform.
//!
//! Set `MANIFOLD_PROOF_OUT` to a directory to also write the PNG.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{GraphTarget, PresetTypeId, SceneNodeRefExt};
use manifold_editing::command::Command;
use manifold_foundation::scene_surface::GizmoDrawMode;
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_nodes_scene::node_graph::gizmo_pass::GizmoPass;
use manifold_nodes_scene::node_graph::scene_exposure::{metadata_for_node_type, migrate_scene_exposures};
use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm, ShapeOwner};
use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoMode, GizmoTargetId, gizmo_lines, gizmo_target_for, handle_length};

const COUNT: f32 = 12.0;
const RING: f32 = 3.0;
const SPACING: f32 = 1.5;
const FRAMES: u32 = 3;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;

/// The Scene preset with one added object turned into a copy group of
/// `COUNT` copies in a Ring at `SPACING`.
fn copy_group_scene() -> EffectGraphDef {
    let mut project = Project::default();
    let index = project.timeline.add_layer("Scene", LayerType::Generator, PresetTypeId::from_string("Scene".to_string()));
    let layer = project.timeline.layers[index].layer_id.clone();
    let mut default = (*manifold_nodes::bundled_presets::bundled_preset_def(&project.timeline.layers[index].generator_type().clone())
        .expect("Scene is a bundled preset"))
    .clone();
    migrate_scene_exposures(&mut default);
    let target = GraphTarget::Generator(layer.clone());
    let render = default.nodes.iter().find(|node| node.type_id == "node.render_scene").expect("a render scene").clone();
    let objects = match render.params.get("objects") {
        Some(SerializedParamValue::Float { value }) => *value as u32,
        _ => 0,
    };
    let scene = manifold_core::SceneNodeRef::locate(&default, &render.node_id).expect("the scene is addressable");

    let template = manifold_nodes_scene::node_graph::scene_templates::object_template(objects, (900.0, 200.0));
    let mut insert = manifold_editing::scene_template::InsertSceneTemplateCommand::new_preparing(
        &project,
        target.clone(),
        scene,
        None,
        template,
        &default,
        &manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_template_force,
    )
    .expect("the object inserts");
    insert.execute(&mut project);
    assert!(insert.was_applied(), "{:?}", insert.rejection_reason());

    let inserted = project.graph_for_target(&target, Some(&default)).expect("the scene graph").clone();
    let object = manifold_core::scene_slots::object_at_slot(&inserted, render.id, objects).expect("the added object");
    let copies: Vec<_> = manifold_core::param_lookup::stamped_params_for("node.rigid_body", true)
        .into_iter()
        .filter(|meta| manifold_core::copies_body::is_copies_body_param("node.rigid_body", &meta.name))
        .collect();
    let mut make = manifold_editing::commands::graph::MakeCopyGroupCommand::new(
        &project,
        target.clone(),
        object,
        &metadata_for_node_type("node.rigid_body"),
        &metadata_for_node_type("node.physics_world"),
        &copies,
        &default,
    )
    .expect("Make Copies builds");
    make.execute(&mut project);
    assert!(make.was_applied(), "{:?}", make.rejection_reason());

    let mut def = project.graph_for_target(&target, Some(&default)).expect("the scene graph").clone();
    // The card rows own these values at build time, so the card default is
    // set with the node's.
    let values = [("copy_count", COUNT), ("copy_layout", RING), ("copy_size_spacing", SPACING)];
    let mut body_ids = Vec::new();
    for group in def.nodes.iter_mut().filter_map(|node| node.group.as_deref_mut()) {
        for body in group.nodes.iter_mut().filter(|node| node.type_id == "node.rigid_body" && node.params.contains_key("copy_count")) {
            for (name, value) in values {
                let stored = if name == "copy_layout" {
                    SerializedParamValue::Enum { value: value as u32 }
                } else {
                    SerializedParamValue::Float { value }
                };
                body.params.insert(name.into(), stored);
            }
            body_ids.push(body.node_id.clone());
        }
    }
    assert_eq!(body_ids.len(), 1, "one copies body");
    let meta = def.preset_metadata.as_mut().expect("scene metadata");
    for binding in &mut meta.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else { continue };
        let Some((_, value)) = values.iter().find(|(name, _)| name == param && body_ids.contains(node_id)) else { continue };
        binding.default_value = *value;
        meta.params.iter_mut().find(|spec| spec.id == binding.id).expect("the card row").default_value = *value;
    }
    def
}

#[test]
fn a_copy_group_renders_its_ring_with_the_gizmo_at_the_group() {
    let def = copy_group_scene();
    let registry = PrimitiveRegistry::with_builtin();
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let (width, height) = (WIDTH, HEIGHT);
    let mut runtime =
        PresetRuntime::from_def_with_device(def.clone(), &registry, h.device.clone(), width, height, GpuTextureFormat::Rgba16Float, None)
            .expect("the copy group scene builds");
    let target = manifold_node_engine::gpu::render_target::RenderTarget::new(&h.device, width, height, GpuTextureFormat::Rgba16Float, "copy-group-proof");
    let manifest = manifold_core::params::ParamManifest::from_params(
        def.preset_metadata.as_ref().expect("metadata").params.iter().cloned().map(manifold_core::params::Param::bundled).collect(),
    );
    for frame in 0..FRAMES {
        let seconds = f64::from(frame) / 60.0;
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
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = h.device.create_encoder("copy-group-proof");
        {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
            runtime.render(&mut gpu, &target.texture, &context, &manifest);
        }
        encoder.commit_and_wait_completed();
    }
    assert!(runtime.errors().is_empty(), "scene errors: {:?}", runtime.errors());
    let base = read(&h.device, &target.texture);

    // The gizmo of the copy group: its transform's handles, one uniform scale.
    let vm = SceneVm::from_def(&def).expect("a scene");
    let row = vm
        .objects
        .iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) if row.copy_group().is_some() => Some(row),
            _ => None,
        })
        .expect("the scene holds a copy group");
    let id = GizmoTargetId { owner: ShapeOwner::Object(row.address.clone()), shape: 0 };
    let gizmo = gizmo_target_for(&vm, &id).expect("the copy group has a gizmo");
    let camera = runtime.write_rendered_view(&mut Vec::new()).expect("a rendered scene reports its camera").1;
    let len = handle_length(gizmo.origin, &camera, height);
    let mut lines = Vec::new();
    gizmo_lines(GizmoMode::Move, &gizmo, len, &mut lines);
    gizmo_lines(GizmoMode::Scale, &gizmo, len, &mut lines);
    assert!(!lines.is_empty(), "the gizmo draws");
    let mut encoder = h.device.create_encoder("copy-group-gizmo");
    GizmoPass::default().draw(&h.device, &mut encoder, &target.texture, None, &camera, &lines, GizmoDrawMode::OnTop);
    encoder.commit_and_wait_completed();
    let with_gizmo = read(&h.device, &target.texture);

    let lit = |bytes: &[u8]| bytes.chunks_exact(8).filter(|px| half::f16::from_le_bytes([px[0], px[1]]).to_f32() > 0.05).count();
    assert!(lit(&base) > 200, "the copies draw ({} lit pixels)", lit(&base));
    assert_ne!(base, with_gizmo, "the gizmo draws over the copies");

    if let Ok(dir) = std::env::var("MANIFOLD_PROOF_OUT") {
        let png = manifold_node_engine::gpu::headless_readback::readback_to_srgb_png(&h.device, &target.texture, width, height);
        std::fs::write(std::path::Path::new(&dir).join("copy_group_ring_with_gizmo.png"), png).expect("write the proof png");
    }
}

/// The target's Rgba16Float bytes.
fn read(device: &manifold_gpu::GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<u8> {
    let bytes_per_row = texture.width * 8;
    let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
    let mut encoder = device.create_encoder("copy-group-readback");
    encoder.copy_texture_to_buffer(texture, &buffer, texture.width, texture.height, bytes_per_row);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("shared readback buffer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (texture.height * bytes_per_row) as usize) }.to_vec()
}
