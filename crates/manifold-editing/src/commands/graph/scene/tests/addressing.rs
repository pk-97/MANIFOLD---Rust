//! Scene commands name their target by `SceneNodeRef`: a doc id repeated across
//! import groups, or a render slot that shifted under an earlier edit, never
//! redirects a command to a different node.
use super::*;
use manifold_core::SceneNodeRefExt;

/// Two import groups with the same inner document ids (11 to 15, the scene
/// object is 14 in both) and different stable ids: how two imports of one
/// model sit in a scene.
fn two_imports_sharing_inner_ids() -> EffectGraphDef {
    let mut def = imported_group_scene_graph();
    def.nodes.iter_mut().find(|node| node.id == 0).unwrap().params.insert("objects".into(), SerializedParamValue::Float { value: 2.0 });
    let mut second = def.nodes.iter().find(|node| node.id == 10).unwrap().clone();
    second.id = 20;
    second.node_id = NodeId::new("second_group");
    second.handle = Some("Imported 2".into());
    for inner in &mut second.group.as_deref_mut().unwrap().nodes {
        inner.node_id = NodeId::new(format!("second_{}", inner.node_id.as_str()));
        if inner.id == 14 {
            inner.handle = Some("Imported 2".into());
        }
    }
    def.nodes.push(second);
    def.wires.push(EffectGraphWire { from_node: 20, from_port: "object".into(), to_node: 0, to_port: "object_1".into() });
    def
}

/// The handle of the scene object (document id 14) inside group `group`.
fn inner_object_handle(def: &EffectGraphDef, group: u32) -> Option<String> {
    let body = def.nodes.iter().find(|node| node.id == group)?.group.as_deref()?;
    body.nodes.iter().find(|node| node.id == 14).and_then(|node| node.handle.clone())
}

#[test]
fn import_groups_sharing_inner_doc_ids_rename_and_remove_the_addressed_one() {
    let graph = two_imports_sharing_inner_ids();
    let first = object_at(&graph, 0, 0);
    let second = object_at(&graph, 0, 1);
    assert_ne!(first, second, "the rows have their own addresses");
    let first_doc = first.resolve_doc_path(&graph).unwrap();
    let second_doc = second.resolve_doc_path(&graph).unwrap();
    assert_eq!(first_doc.1, second_doc.1, "both inner scene objects are document id 14");

    let (mut project, fx) = project_with_graph(graph.clone());
    let target = GraphTarget::Effect(fx.clone());
    let mut rename = RenameSceneObjectCommand::new(target.clone(), second.clone(), "Renamed".into(), mirror_catalog_default());
    rename.execute(&mut project);
    assert!(rename.was_applied(), "{:?}", rename.rejection_reason());
    let renamed = graph_of(&project, &fx);
    assert_eq!(inner_object_handle(renamed, 20).as_deref(), Some("Renamed"), "the second import takes the name");
    assert_eq!(inner_object_handle(renamed, 10).as_deref(), Some("Imported"), "the first import keeps its own");
    rename.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &graph);

    let mut remove = RemoveSceneObjectCommand::new(target, second, mirror_catalog_default());
    remove.execute(&mut project);
    assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
    let after = graph_of(&project, &fx);
    assert!(after.nodes.iter().any(|node| node.id == 10), "the first import stays");
    assert!(!after.nodes.iter().any(|node| node.id == 20), "the second import goes");
    assert!(after.wires.iter().any(|wire| wire.from_node == 10 && wire.to_port == "object_0"));
}

#[test]
fn a_command_for_an_object_that_is_gone_refuses_with_a_reason() {
    let graph = two_imports_sharing_inner_ids();
    let second = object_at(&graph, 0, 1);
    let (mut project, fx) = project_with_graph(graph);
    let target = GraphTarget::Effect(fx.clone());

    let mut remove = RemoveSceneObjectCommand::new(target.clone(), second.clone(), mirror_catalog_default());
    remove.execute(&mut project);
    assert!(remove.was_applied());
    let after_remove = graph_of(&project, &fx).clone();

    for mut stale in [
        Box::new(RemoveSceneObjectCommand::new(target.clone(), second.clone(), mirror_catalog_default())) as Box<dyn Command>,
        Box::new(RenameSceneObjectCommand::new(target.clone(), second.clone(), "Late".into(), mirror_catalog_default())),
    ] {
        stale.execute(&mut project);
        assert!(!stale.was_applied());
        assert!(stale.rejection_reason().unwrap().contains("no longer"), "{:?}", stale.rejection_reason());
        assert_eq!(graph_of(&project, &fx), &after_remove, "a refused command changes nothing");
    }
    let duplicate = duplicate_copies(&project, target, vec![], 0, 1, 1, &mirror_catalog_default());
    assert!(duplicate.unwrap_err().message.contains("no longer"), "a duplicate prepared for a removed object refuses");
}

#[test]
fn remove_light_by_address_survives_another_lights_removal() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    let target = GraphTarget::Effect(fx.clone());
    for x in [0.0, 1.0, 2.0] {
        insert_template(&project, &target, 0, test_light_template((x, 0.0))).unwrap().execute(&mut project);
    }
    let def = graph_of(&project, &fx).clone();
    let [first, middle, last] = [0, 1, 2].map(|slot| light_at(&def, 0, slot));
    let node_of = |reference: &SceneNodeRef| reference.resolve(&def).unwrap().id;
    let (middle_id, last_id) = (node_of(&middle), node_of(&last));

    remove_light(&project, target.clone(), vec![], 0, 0, mirror_catalog_default()).execute(&mut project);
    // The slots renumbered under the first removal. A click that still names
    // the third light by its old position finds nothing; its address finds it.
    let mut stale_position = remove_light(&project, target.clone(), vec![], 0, 2, mirror_catalog_default());
    stale_position.execute(&mut project);
    assert!(!stale_position.was_applied(), "slot 2 holds nothing now");

    let mut remove_last = RemoveSceneLightCommand::new(target.clone(), last, mirror_catalog_default());
    remove_last.execute(&mut project);
    assert!(remove_last.was_applied(), "{:?}", remove_last.rejection_reason());
    let lights: Vec<u32> = graph_of(&project, &fx).nodes.iter().filter(|node| node.type_id == "node.light").map(|node| node.id).collect();
    assert_eq!(lights, vec![middle_id], "only the middle light is left");
    assert!(!lights.contains(&last_id));

    let mut gone = RemoveSceneLightCommand::new(target, first, mirror_catalog_default());
    gone.execute(&mut project);
    assert!(!gone.was_applied(), "the first light was removed already");
    assert!(gone.rejection_reason().unwrap().contains("no longer"));
}

#[test]
fn graph_editor_paste_of_a_group_gives_every_inner_node_a_new_stable_id() {
    let graph = imported_group_scene_graph();
    let group = graph.nodes.iter().find(|node| node.id == 10).unwrap().clone();
    let original = graph.clone();
    let (mut project, fx) = project_with_graph(graph);
    let mut paste = PasteNodesCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        vec![group],
        vec![],
        (30.0, 30.0),
        mirror_catalog_default(),
    );
    paste.execute(&mut project);
    assert!(paste.was_applied());
    let pasted = graph_of(&project, &fx);

    fn collect(nodes: &[EffectGraphNode], seen: &mut std::collections::HashSet<NodeId>, repeated: &mut Vec<NodeId>) {
        for node in nodes {
            if !seen.insert(node.node_id.clone()) {
                repeated.push(node.node_id.clone());
            }
            if let Some(group) = node.group.as_deref() {
                collect(&group.nodes, seen, repeated);
            }
        }
    }
    let (mut seen, mut repeated) = (std::collections::HashSet::new(), Vec::new());
    collect(&pasted.nodes, &mut seen, &mut repeated);
    assert!(repeated.is_empty(), "a pasted group repeats stable ids: {repeated:?}");
    manifold_core::scene_index::FlatSceneIndex::build(pasted).expect("the scene index builds after the paste");

    paste.undo(&mut project);
    let (mut seen_after, mut none) = (std::collections::HashSet::new(), Vec::new());
    collect(&graph_of(&project, &fx).nodes, &mut seen_after, &mut none);
    let (mut seen_before, mut none_before) = (std::collections::HashSet::new(), Vec::new());
    collect(&original.nodes, &mut seen_before, &mut none_before);
    assert_eq!(seen_after, seen_before, "undo takes every id the paste added");
}
