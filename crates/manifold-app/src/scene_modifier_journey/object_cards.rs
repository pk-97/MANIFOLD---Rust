//! Imported-object card edit, observed deformation, undo and persistence.
use super::*;
use crate::object_modifier_transfer::ObjectModifierAction;
use manifold_editing::commands::graph::RemoveMeshModifierCommand;
use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};

fn owner(project: &Project, layer: &LayerId) -> u32 {
    SceneVm::from_def(generator_graph(project, layer))
        .unwrap()
        .objects
        .iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) => Some(row.group_node_id.unwrap_or(row.object_node_id)),
            _ => None,
        })
        .unwrap()
}

fn angle_binding(project: &Project, layer: &LayerId, doc_id: u32) -> String {
    fn find(nodes: &[EffectGraphNode], doc_id: u32) -> Option<&EffectGraphNode> {
        nodes.iter().find_map(|node| {
            if node.id == doc_id {
                Some(node)
            } else {
                node.group
                    .as_ref()
                    .and_then(|group| find(&group.nodes, doc_id))
            }
        })
    }
    let graph = generator_graph(project, layer);
    let node = find(&graph.nodes, doc_id).unwrap();
    graph.preset_metadata.as_ref().unwrap().bindings.iter().find_map(|binding|
        matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &node.node_id && param == "angle")
            .then(|| binding.id.clone())).unwrap()
}

#[test]
fn imported_object_card_deformation_undo_remove_and_reopen() {
    let layer_id = LayerId::new("card-helmet");
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gltf/DamagedHelmet.glb"
    ));
    let mut project = Project::default();
    project.settings.output_width = 320;
    project.settings.output_height = 180;
    project
        .timeline
        .layers
        .push(imported_layer("Card Helmet", layer_id.as_str(), 0, path));
    let owner_id = owner(&project, &layer_id);
    let out = Path::new("target/journey-proofs/object-card-ux");
    std::fs::create_dir_all(out).unwrap();
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (tx, _rx) = crossbeam_channel::unbounded();
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    ct.handle_command(ContentCommand::ObjectModifier(ObjectModifierAction::Add {
        layer_id: layer_id.clone(),
        owner_id,
        type_id: "node.bend_mesh".into(),
        after: None,
    }));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "{:?}",
        ct.graph_edit_diagnostic
    );
    let doc_id = ct
        .object_modifier_selection_update
        .as_ref()
        .unwrap()
        .node_doc_id;
    let angle = angle_binding(ct.engine.project().unwrap(), &layer_id, doc_id);
    set_generator_param(&mut ct, &layer_id, &angle, 0.0);
    let (straight, _) = capture_output_when_ready(&mut ct, &tx, &out.join("straight.png"));
    set_generator_param(&mut ct, &layer_id, &angle, -std::f32::consts::FRAC_PI_2);
    let (bent, _) = capture_output_when_ready(&mut ct, &tx, &out.join("bent.png"));
    assert!(
        straight
            .iter()
            .zip(&bent)
            .filter(|(a, b)| a.abs_diff(**b) > 8)
            .count()
            > 100,
        "changing the card parameter must change observed geometry"
    );
    ct.handle_command(ContentCommand::Undo);
    capture_output_when_ready(&mut ct, &tx, &out.join("undo.png"));
    assert_eq!(
        ct.engine.project().unwrap().timeline.layers[0]
            .gen_params()
            .unwrap()
            .get_base_param(&angle),
        0.0
    );
    ct.handle_command(ContentCommand::Redo);
    let before_remove = serde_json::to_vec(
        ct.engine.project().unwrap().timeline.layers[0]
            .gen_params()
            .unwrap(),
    )
    .unwrap();
    let default = generator_graph(ct.engine.project().unwrap(), &layer_id).clone();
    ct.handle_command(ContentCommand::ExecuteOnContent(Box::new(
        RemoveMeshModifierCommand::new(
            manifold_core::GraphTarget::Generator(layer_id.clone()),
            Vec::new(),
            owner_id,
            doc_id,
            default,
        ),
    )));
    assert!(
        !ct.engine.project().unwrap().timeline.layers[0]
            .gen_params()
            .unwrap()
            .params
            .contains(&angle)
    );
    ct.handle_command(ContentCommand::Undo);
    assert_eq!(
        serde_json::to_vec(
            ct.engine.project().unwrap().timeline.layers[0]
                .gen_params()
                .unwrap()
        )
        .unwrap(),
        before_remove
    );
    // A scene-wide effect and an object deformation must coexist through save.
    send_modifier(
        &mut ct,
        SceneModifierAction::Add(layer_id.clone(), "SceneFog".into()),
    );
    let saved = out.join("object-card-ux.manifold");
    manifold_io::saver::save_project_v1(ct.engine.project().unwrap(), &saved).unwrap();
    let reopened = manifold_io::loader::load_project(&saved).unwrap();
    assert_eq!(
        reopened.timeline.layers[0]
            .gen_params()
            .unwrap()
            .get_base_param(&angle),
        -std::f32::consts::FRAC_PI_2
    );
    assert_eq!(
        generator_graph(&reopened, &layer_id).scene_modifiers.len(),
        1
    );
    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    warm_project(&mut ct, &tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats::ZERO));
    capture_output_when_ready(&mut ct, &tx, &out.join("reopened.png"));
    // A calibrated mesh stage must reject a later object-chain edit atomically.
    let default = generator_graph(ct.engine.project().unwrap(), &layer_id).clone();
    ct.handle_command(ContentCommand::ExecuteOnContent(Box::new(
        RemoveMeshModifierCommand::new(
            manifold_core::GraphTarget::Generator(layer_id.clone()),
            Vec::new(),
            owner_id,
            doc_id,
            default,
        ),
    )));
    send_modifier(
        &mut ct,
        SceneModifierAction::Add(layer_id.clone(), "VortexFragments".into()),
    );
    let before = serde_json::to_vec(ct.engine.project().unwrap()).unwrap();
    let undo = ct
        .editing_service
        .peek_undo_description()
        .map(str::to_owned);
    ct.handle_command(ContentCommand::ObjectModifier(ObjectModifierAction::Add {
        layer_id: layer_id.clone(),
        owner_id,
        type_id: "node.bend_mesh".into(),
        after: None,
    }));
    assert!(
        ct.graph_edit_diagnostic.is_some(),
        "calibrated frame must guard chain changes"
    );
    assert_eq!(
        serde_json::to_vec(ct.engine.project().unwrap()).unwrap(),
        before
    );
    assert_eq!(ct.editing_service.peek_undo_description(), undo.as_deref());
}
