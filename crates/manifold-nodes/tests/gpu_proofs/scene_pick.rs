//! Promise 13 (SCENE_DOCUMENT_DESIGN.md section 7.14): clicking the picture
//! picks the item drawn there through `render_scene`'s item-id buffer. Here,
//! not beside the selection mask proof, because a compiled scene document
//! needs every crate's nodes in the registry.

use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

/// Promise 13 (SCENE_DOCUMENT_DESIGN.md section 7.14): a click returns the
/// item drawn there through the item-id buffer, a part inside a group
/// included, and empty space returns nothing.
#[test]
fn a_click_returns_the_drawn_item_a_part_in_a_group_included() {
    use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, SceneDoc};
    use manifold_node_engine::scene::pick::PickRequest;

    let mut doc = SceneDoc::base();
    let (rig, cube, part) = (ItemId::new("rig"), ItemId::new("cube"), ItemId::new("part"));
    doc.insert(
        None,
        0,
        Item::new(rig.clone(), "Rig", ItemFields::default_for(ItemKind::Group)),
    )
    .unwrap();
    doc.reparent(&cube, Some(&rig), 0).unwrap();
    let fields = doc.find(&cube).unwrap().fields.clone();
    doc.insert(Some(&rig), 1, Item::new(part.clone(), "Part", fields))
        .unwrap();
    let mut fx = manifold_core::effects::PresetInstance::new_generator(
        manifold_core::PresetTypeId::from_string("Scene".into()),
    );
    fx.install_scene_document(doc.clone()).unwrap();
    assert!(
        fx.set_base_param("part.pos_x", 2.5),
        "the part has a Position X"
    );

    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_with_device(
        fx.graph.clone().unwrap(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        Some(&fx.params),
    )
    .expect("the scene document's graph must build");
    runtime.apply_param_values(&fx.params);
    let target = h.make_target("render-scene-pick");
    let mut frame = 0;
    let mut render = |runtime: &mut PresetRuntime| {
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
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        frame += 1;
        let mut enc = h.device.create_encoder("render-scene-pick-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &fx.params);
        }
        enc.commit_and_wait_completed();
    };
    render(&mut runtime);
    let (_, camera) = runtime
        .write_rendered_view(&mut Vec::new())
        .expect("the scene reports its camera");
    let at = |item: &str| {
        let world =
            ["pos_x", "pos_y", "pos_z"].map(|axis| fx.get_base_param(&format!("{item}.{axis}")));
        let p = camera
            .project_to_pixel(world, h.width, h.height)
            .expect("the item is in front of the camera");
        [p.px / h.width as f32, p.py / h.height as f32]
    };
    let drawn = manifold_nodes_scene::node_graph::scene_compile::drawn_items(&doc);
    let mut click = |id: u64, uv: [f32; 2]| {
        runtime.request_pick(PickRequest { id, uv });
        render(&mut runtime);
        render(&mut runtime);
        let hit = runtime
            .take_pick()
            .expect("a pick is answered the frame after it draws");
        assert_eq!(hit.id, id);
        hit.slot.map(|slot| drawn[slot as usize].clone())
    };
    assert_eq!(
        click(1, at("part")),
        Some(part),
        "the part inside the group"
    );
    assert_eq!(click(2, at("cube")), Some(cube), "its sibling");
    assert_eq!(click(3, [0.02, 0.02]), None, "empty space");
}
