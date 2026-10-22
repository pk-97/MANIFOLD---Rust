//! Every document edit undoes exactly; a rename or a move keeps an item's
//! params, and so everything attached to them; a param write never
//! recompiles. The compiler here is a stand-in with one node and one
//! `<item id>.value` card per item, so the tests read the editing contract,
//! not the scene compiler.

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::{ParameterDriver, PresetInstance};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_doc::{ItemFields, ItemId, ItemKind, PhysicsSetting, SceneCompiler, SceneDoc, param_id};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{LayerId, PresetTypeId};

use super::*;
use crate::command::{Command, CompositeCommand};
use crate::commands::effects::ChangeGraphParamCommand;
use crate::undo::UndoRedoManager;

fn stand_in(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    let ids = doc.ids();
    let node = |id: &ItemId| format!("{id}:item");
    let nodes: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(k, id)| serde_json::json!({"id": k, "nodeId": node(id), "typeId": "test.item"}))
        .collect();
    let params: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"id": param_id(id, "value"), "name": "Value", "min": 0.0, "max": 1.0, "defaultValue": 0.5}))
        .collect();
    let bindings: Vec<_> = ids
        .iter()
        .map(|id| {
            serde_json::json!({"id": param_id(id, "value"), "label": "Value", "defaultValue": 0.5,
                "target": {"kind": "node", "nodeId": node(id), "param": "value"}})
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Scene",
        "presetMetadata": {"id": "Scene", "displayName": "Scene", "category": "Test", "oscPrefix": "scene",
            "available": true, "params": params, "bindings": bindings},
        "nodes": nodes, "wires": []
    }))
    .map_err(|error| error.to_string())
}

inventory::submit! { SceneCompiler { compile: stand_in } }

const CUBE: &str = "cube";

fn project() -> (Project, LayerId) {
    let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
    layer.layer_id = LayerId::new("scene");
    let id = layer.layer_id.clone();
    let fx = layer.gen_params_mut().unwrap();
    fx.install_scene_document(SceneDoc::base()).unwrap();
    assert!(fx.set_base_param(&param_id(&ItemId::new(CUBE), "value"), 0.8));
    fx.drivers = Some(vec![ParameterDriver::new(param_id(&ItemId::new(CUBE), "value"), BeatDivision::Quarter, DriverWaveform::Sine)]);
    let mut project = Project::default();
    project.timeline.layers.push(layer);
    (project, id)
}

fn scene(project: &Project) -> &PresetInstance {
    project.timeline.layers[0].gen_params().unwrap()
}

/// Everything an edit may change: the document, the graph, and every param
/// with its value and modulators.
fn state(project: &Project) -> String {
    let fx = scene(project);
    let params: Vec<_> = fx.params.iter().map(|p| (p.id().to_owned(), p.value, p.base)).collect();
    format!("{:?}\n{:?}\n{params:?}\n{:?}", fx.scene, fx.graph, fx.drivers)
}

fn id(s: &str) -> ItemId {
    ItemId::new(s)
}

#[test]
fn every_scene_edit_undoes_exactly() {
    let (project, layer) = project();
    let doc = scene(&project).scene.clone().unwrap();
    let body = ItemFields::Object(manifold_core::scene_doc::ObjectFields {
        physics: PhysicsSetting::Body,
        ..Default::default()
    });
    let edits: Vec<(&str, Box<dyn Command>)> = vec![
        ("add", Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Light, None, 0, None, id("lamp")))),
        ("duplicate", Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Object, None, 0, Some(id(CUBE)), id("cube2")))),
        ("remove", Box::new(RemoveSceneItemCommand::new(layer.clone(), id(CUBE)))),
        ("move", Box::new(ReparentSceneItemCommand::new(layer.clone(), id(CUBE), None, 0))),
        ("rename", Box::new(RenameSceneItemCommand::new(layer.clone(), id(CUBE), "Box"))),
        ("set", Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id(CUBE), body))),
        ("group", Box::new(CompositeCommand::new(group_commands(&layer, &doc, &[id(CUBE), id("sun")], id("rig")), "Group".into()))),
    ];
    for (name, edit) in edits {
        let (mut project, _) = self::project();
        let before = state(&project);
        let mut undo = UndoRedoManager::new();
        assert!(undo.execute(edit, &mut project), "{name} applies");
        let after = state(&project);
        assert_ne!(after, before, "{name} changes the scene");
        assert!(undo.undo(&mut project));
        assert_eq!(state(&project), before, "{name} undoes exactly");
        assert!(undo.redo(&mut project));
        assert_eq!(state(&project), after, "{name} redoes exactly");
    }

    // Ungroup, from a grouped scene.
    let (mut project, layer) = self::project();
    let mut undo = UndoRedoManager::new();
    let group = CompositeCommand::new(group_commands(&layer, &doc, &[id(CUBE)], id("rig")), "Group".into());
    assert!(undo.execute(Box::new(group), &mut project));
    let before = state(&project);
    let grouped = scene(&project).scene.clone().unwrap();
    let ungroup = CompositeCommand::new(ungroup_commands(&layer, &grouped, &id("rig")), "Ungroup".into());
    assert!(undo.execute(Box::new(ungroup), &mut project), "ungroup applies");
    assert!(scene(&project).scene.as_ref().unwrap().find(&id("rig")).is_none(), "the group is gone");
    assert!(undo.undo(&mut project));
    assert_eq!(state(&project), before, "ungroup undoes exactly");
}

/// A rename or a move compiles the same item under the same id, so its
/// params keep their ids, values and modulators: every MIDI map, OSC
/// address and LFO keyed on `<item id>.<property>` stays attached.
#[test]
fn rename_and_move_keep_an_items_params_attached() {
    let (mut project, layer) = project();
    let value = param_id(&id(CUBE), "value");
    let doc = scene(&project).scene.clone().unwrap();
    let edits: Vec<Box<dyn Command>> = vec![
        Box::new(RenameSceneItemCommand::new(layer.clone(), id(CUBE), "Box")),
        Box::new(CompositeCommand::new(group_commands(&layer, &doc, &[id(CUBE)], id("rig")), "Group".into())),
        Box::new(ReparentSceneItemCommand::new(layer.clone(), id(CUBE), None, 0)),
    ];
    let mut undo = UndoRedoManager::new();
    for edit in edits {
        let description = edit.description().to_owned();
        assert!(undo.execute(edit, &mut project), "{description} applies");
        let fx = scene(&project);
        assert_eq!(fx.get_base_param(&value), 0.8, "{description} kept the cube's value");
        assert_eq!(fx.drivers.as_ref().unwrap()[0].param_id, value, "{description} kept the cube's LFO");
    }
    assert_eq!(scene(&project).scene.as_ref().unwrap().find(&id(CUBE)).unwrap().name, "Box");
}

/// A param write changes a value, never the scene's structure.
#[test]
fn a_param_write_never_recompiles() {
    let (mut project, layer) = project();
    let (graph, version) = (scene(&project).graph.clone(), scene(&project).graph_structure_version);
    let write = ChangeGraphParamCommand::new(GraphTarget::Generator(layer), param_id(&id(CUBE), "value"), 0.8, 0.3);
    assert!(UndoRedoManager::new().execute(Box::new(write), &mut project));
    let fx = scene(&project);
    assert_eq!(fx.get_base_param(&param_id(&id(CUBE), "value")), 0.3);
    assert_eq!((&fx.graph, fx.graph_structure_version), (&graph, version), "the write recompiled the scene");
}

fn part(path: &str) -> manifold_core::scene_doc::ImportedNode {
    manifold_core::scene_doc::ImportedNode {
        key: format!("{path}#mesh"),
        name: path.to_owned(),
        mesh_node: Some(path.to_owned()),
        primitives: manifold_core::scene_doc::MeshPrimitives::All,
        material: None,
        vertex_count: 3,
        center: [0.0; 3],
        children: Vec::new(),
    }
}

fn model(paths: &[&str]) -> manifold_core::scene_doc::ImportedModel {
    manifold_core::scene_doc::ImportedModel {
        name: "rig".into(),
        roots: vec![manifold_core::scene_doc::ImportedNode {
            key: "rig".into(),
            name: "rig".into(),
            mesh_node: None,
            primitives: manifold_core::scene_doc::MeshPrimitives::All,
            material: None,
            vertex_count: 0,
            center: [0.0; 3],
            children: paths.iter().map(|p| part(p)).collect(),
        }],
        materials: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        clips: 0,
        bounds: None,
    }
}

/// A Model File swap keeps the edits on parts whose file node path is still
/// there, drops the parts that vanished, adds new ones, and is one undo step.
#[test]
fn a_model_file_swap_keeps_edits_on_surviving_parts() {
    use manifold_core::scene_doc::{Shape, import_child_id};

    let (mut project, layer) = project();
    let group = id("model");
    let mut undo = UndoRedoManager::new();
    let first = model(&["rig/a", "rig/b"]);
    let add = import_model_commands(&layer, None, 0, group.clone(), "a.glb", &first);
    assert!(undo.execute(Box::new(CompositeCommand::new(add, "Import".into())), &mut project));
    let a = import_child_id(&group, "rig/a#mesh");
    let b = import_child_id(&group, "rig/b#mesh");
    let doc = scene(&project).scene.clone().unwrap();
    assert!(doc.find(&a).is_some() && doc.find(&b).is_some(), "both parts imported");

    // Edits on part a: a transform value and a material binding.
    let value = param_id(&a, "value");
    assert!(UndoRedoManager::new().execute(
        Box::new(ChangeGraphParamCommand::new(GraphTarget::Generator(layer.clone()), value.clone(), 0.5, 0.9)),
        &mut project
    ));
    assert!(undo.execute(Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Material, None, 0, None, id("paint"))), &mut project));
    let ItemFields::Object(mut fields) = scene(&project).scene.as_ref().unwrap().find(&a).unwrap().fields.clone() else {
        panic!("a part is an Object");
    };
    fields.material = Some(id("paint"));
    assert!(undo.execute(Box::new(SetSceneItemFieldsCommand::new(layer.clone(), a.clone(), ItemFields::Object(fields))), &mut project));

    // Swap to a file that keeps a, loses b and gains c.
    let before = state(&project);
    let doc = scene(&project).scene.clone().unwrap();
    let plan = swap_model_file_commands(&layer, &doc, &group, "b.glb", &model(&["rig/a", "rig/c"])).unwrap();
    assert_eq!(plan.removed, 1);
    assert!(undo.execute(Box::new(CompositeCommand::new(plan.commands, "Swap".into())), &mut project));

    let after = scene(&project).scene.clone().unwrap();
    let c = import_child_id(&group, "rig/c#mesh");
    assert!(after.find(&b).is_none(), "the vanished part is gone");
    assert!(after.find(&c).is_some(), "the new part is added");
    let kept = after.find(&a).expect("the surviving part stays");
    let ItemFields::Object(kept) = &kept.fields else { panic!("a part is an Object") };
    assert_eq!(kept.material, Some(id("paint")), "the material binding survives");
    assert!(matches!(&kept.shape, Shape::Mesh { file, node_path, .. } if file == "b.glb" && node_path == "rig/a"));
    assert_eq!(scene(&project).get_base_param(&value), 0.9, "the transform edit survives");

    // One undo step restores the scene as it was before the swap.
    assert!(undo.undo(&mut project));
    assert_eq!(state(&project), before, "the swap undoes in one step");
}
