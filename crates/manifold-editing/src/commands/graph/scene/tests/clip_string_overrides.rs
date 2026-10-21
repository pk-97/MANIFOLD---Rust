//! D-117: a duplicate is independent but starts at the file its source shows.
//! The card's Model File choice lives per clip, so Duplicate carries each
//! clip's override onto the copy's own id, in the same undo step.
use super::*;
use manifold_core::clip::TimelineClip;

#[test]
fn a_duplicated_object_keeps_its_clips_swapped_model_file() {
    let (mut project, layer) = project_with_generator_graph(render_scene_graph(0, 0));
    let target = GraphTarget::Generator(layer.clone());
    insert_template(&project, &target, 0, test_object_template((0.0, 0.0))).unwrap().execute(&mut project);
    let mesh = graph_of_generator(&project, &layer)
        .nodes
        .iter()
        .filter_map(|node| node.group.as_deref())
        .flat_map(|group| group.nodes.iter())
        .find(|node| node.type_id == "node.cube_mesh")
        .expect("the object's mesh")
        .node_id
        .clone();
    let (_, host_layer) = project.timeline.find_layer_by_id_mut(layer.as_str()).unwrap();
    let def = host_layer.gen_params_mut().unwrap().graph.as_mut().unwrap();
    let meta = def.preset_metadata.get_or_insert_with(|| {
        serde_json::from_value(serde_json::json!({
            "id": "test.scene", "displayName": "Test Scene", "category": "", "oscPrefix": "",
            "params": [], "bindings": [],
        }))
        .unwrap()
    });
    meta.string_params.push(
        serde_json::from_value(serde_json::json!({
            "id": "model_file", "name": "Model File", "defaultValue": "assets/hero.glb", "isFilePicker": true,
        }))
        .unwrap(),
    );
    meta.string_bindings.push(StringBindingDef {
        id: "model_file".into(),
        label: "Model File".into(),
        default_value: "assets/hero.glb".into(),
        target: BindingTarget::Node { node_id: mesh, param: "path".into() },
    });
    let mut swapped = TimelineClip::new_generator(manifold_core::Beats::ZERO, manifold_core::Beats::from_f32(4.0));
    swapped.string_params = Some([("model_file".to_string(), "assets/swapped.glb".to_string())].into());
    let untouched = TimelineClip::new_generator(manifold_core::Beats::from_f32(4.0), manifold_core::Beats::from_f32(4.0));
    host_layer.clips.extend([swapped, untouched]);
    let clips_before: Vec<_> = host_layer.clips.iter().map(|clip| clip.string_params.clone()).collect();

    let mut cmd = duplicate_object(&project, target, vec![], 0, 0, mirror_catalog_default());
    assert!(cmd.touches().contains(manifold_core::change::Part::Clips));
    cmd.execute(&mut project);
    let copy_id = graph_of_generator(&project, &layer)
        .preset_metadata
        .as_ref()
        .unwrap()
        .string_params
        .iter()
        .find(|spec| spec.id != "model_file")
        .expect("the copy's own Model File")
        .id
        .clone();
    let clips = |project: &Project| -> Vec<Option<std::collections::BTreeMap<String, String>>> {
        project.timeline.find_layer_by_id(&layer).unwrap().1.clips.iter().map(|clip| clip.string_params.clone()).collect()
    };
    let after = clips(&project);
    assert_eq!(
        after[0].as_ref().and_then(|values| values.get(&copy_id)).map(String::as_str),
        Some("assets/swapped.glb"),
        "the copy loads the scan its source shows on this clip"
    );
    assert_eq!(after[0].as_ref().and_then(|values| values.get("model_file")).map(String::as_str), Some("assets/swapped.glb"));
    assert_eq!(after[1], None, "a clip with no override gets none");

    cmd.undo(&mut project);
    assert_eq!(clips(&project), clips_before, "one undo step restores every clip");
}
