//! Native render regression for additions to an imported scene while paused.
use super::*;
use manifold_editing::commands::graph::{AddSceneLayerPlaneCommand, AddSceneObjectCommand};
use manifold_renderer::node_graph::scene_exposure::metadata_for_node_type as metadata;

#[test]
fn scene_additions_render_while_paused_and_roundtrip_undo() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gltf/DamagedHelmet.glb"
    ));
    let mut project = Project::default();
    project.settings.output_width = 320;
    project.settings.output_height = 180;
    let mut layer = imported_layer("Helmet", "helmet", 0, path);
    let graph = layer.generator_graph().unwrap();
    let scene = graph
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .unwrap()
        .id;
    let binding = graph
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|b| matches!(&b.target, BindingTarget::Node { param, .. } if param == "visible"))
        .unwrap()
        .id
        .clone();
    // Hide the original model so existing geometry cannot satisfy the
    // visibility assertion for the newly added cube or plane.
    layer.gen_params_or_init().set_param(&binding, 0.0);
    let default = layer.generator_graph().unwrap().clone();
    project.timeline.layers.push(layer);
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (tx, _rx) = crossbeam_channel::unbounded();
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    let out = Path::new("/private/tmp/scene-add-visibility");
    std::fs::create_dir_all(out).unwrap();
    let target = manifold_core::GraphTarget::Generator(LayerId::new("helmet"));
    let object = AddSceneObjectCommand::new(
        target.clone(),
        vec![],
        scene,
        1,
        (900.0, 240.0),
        metadata("node.phong_material"),
        metadata("node.transform_3d"),
        metadata("node.scene_object"),
        default.clone(),
    );
    ct.handle_command(ContentCommand::Execute(Box::new(object)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "{:?}",
        ct.graph_edit_diagnostic
    );
    capture_output_when_ready(&mut ct, &tx, &out.join("object.png"));
    ct.handle_command(ContentCommand::Undo);
    assert_eq!(
        manifold_renderer::node_graph::scene_vm::SceneVm::from_def(
            ct.engine.project().unwrap().timeline.layers[0]
                .generator_graph()
                .unwrap()
        )
        .unwrap()
        .header.object_count,
        1
    );
    ct.handle_command(ContentCommand::Redo);
    capture_output_when_ready(&mut ct, &tx, &out.join("object-redo.png"));
    ct.handle_command(ContentCommand::Undo);
    let plane = AddSceneLayerPlaneCommand::new(
        target,
        vec![],
        scene,
        1,
        (900.0, 240.0),
        16.0 / 9.0,
        1.0,
        metadata("node.unlit_material"),
        metadata("node.transform_3d"),
        metadata("node.scene_object"),
        default,
    );
    ct.handle_command(ContentCommand::Execute(Box::new(plane)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "{:?}",
        ct.graph_edit_diagnostic
    );
    capture_output_when_ready(&mut ct, &tx, &out.join("plane.png"));
    ct.handle_command(ContentCommand::Undo);
    ct.handle_command(ContentCommand::Redo);
    capture_output_when_ready(&mut ct, &tx, &out.join("plane-redo.png"));
    assert!(
        !ct.engine.is_playing(),
        "additions must render without starting playback"
    );
}
