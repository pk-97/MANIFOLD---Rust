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
use manifold_core::material_inspector::MaterialFeature;
use manifold_core::scene_doc::{FeatureSetting, ItemFields, ItemId, ItemKind, MaterialFields, MaterialType, ModifierFields, ModifierTargets, ModifierType, PhysicsSetting, SceneCompiler, SceneDoc, param_id};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{LayerId, PresetTypeId};

use super::*;
use crate::command::{Command, CompositeCommand};
use crate::commands::effects::ChangeGraphParamCommand;
use crate::undo::UndoRedoManager;

thread_local! {
    /// How many times this thread compiled a scene.
    static COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn stand_in(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    COMPILES.with(|count| count.set(count.get() + 1));
    let ids = doc.ids();
    let node = |id: &ItemId| format!("{id}:item");
    let mut nodes: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(k, id)| serde_json::json!({"id": k, "nodeId": node(id), "typeId": "test.item"}))
        .collect();
    // The scene's own input, render and output, which the Scene node seals.
    let k = ids.len();
    for (offset, role, type_id) in
        [(0, "input", "system.generator_input"), (1, "render", "node.render_scene"), (2, "output", "system.final_output")]
    {
        nodes.push(serde_json::json!({"id": k + offset, "nodeId": format!("scene:{role}"), "typeId": type_id}));
    }
    let wires = [serde_json::json!({"fromNode": k + 1, "fromPort": "color", "toNode": k + 2, "toPort": "in"})];
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
        "nodes": nodes, "wires": wires
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

/// A Material and a feature added to it, with the feature's seed value, as the
/// Scene tab sends it: one undo step puts the scene back exactly.
fn material_feature(layer: &LayerId) -> CompositeCommand {
    let fields = ItemFields::Material(MaterialFields {
        features: vec![FeatureSetting { feature: MaterialFeature::Coat, mode: 2 }],
        ..MaterialFields::default()
    });
    let steps: Vec<Box<dyn Command>> = vec![
        Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Material, None, 0, None, id("mat"))),
        Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id("mat"), fields)),
        Box::new(ChangeGraphParamCommand::new(GraphTarget::Generator(layer.clone()), param_id(&id("mat"), "value"), 0.5, 0.9)),
    ];
    CompositeCommand::new(steps, "Add Material Feature".into())
}

#[test]
fn every_scene_edit_undoes_exactly() {
    let (project, layer) = project();
    let doc = scene(&project).scene.clone().unwrap();
    let body = ItemFields::Object(manifold_core::scene_doc::ObjectFields {
        physics: PhysicsSetting::Body,
        ..Default::default()
    });
    let ball = manifold_core::scene_doc::Item::new(
        id("ball"),
        "Icosahedron",
        ItemFields::Object(manifold_core::scene_doc::ObjectFields {
            shape: manifold_core::scene_doc::Shape::Icosahedron,
            ..Default::default()
        }),
    );
    let ball_material = new_material(id("ball-material"), "Material 2");
    let make_unique = own_material_commands(&layer, &doc, &id(CUBE), id("paint"), "Material 2").unwrap();
    let edits: Vec<(&str, Box<dyn Command>)> = vec![
        ("add", Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Light, None, 0, None, id("lamp")))),
        ("duplicate", Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Object, None, 0, Some(id(CUBE)), id("cube2")))),
        ("remove", Box::new(RemoveSceneItemCommand::new(layer.clone(), id(CUBE)))),
        ("move", Box::new(ReparentSceneItemCommand::new(layer.clone(), id(CUBE), None, 0))),
        ("rename", Box::new(RenameSceneItemCommand::new(layer.clone(), id(CUBE), "Box"))),
        ("set", Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id(CUBE), body))),
        ("group", Box::new(CompositeCommand::new(group_commands(&layer, &doc, &[id(CUBE), id("sun")], id("rig")), "Group".into()))),
        ("material feature", Box::new(material_feature(&layer))),
        (
            "material type",
            Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                id(manifold_core::scene_doc::BASE_CUBE_MATERIAL),
                ItemFields::Material(MaterialFields { material_type: MaterialType::Unlit, ..MaterialFields::default() }),
            )),
        ),
        ("add object", Box::new(CompositeCommand::new(add_object_commands(&layer, None, 0, ball, ball_material), "Add".into()))),
        ("make unique", Box::new(CompositeCommand::new(make_unique, "Make Unique".into()))),
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

    // The Modifiers chain, from a scene with two cards: the first aimed at
    // the cube, the second at the whole scene.
    let carded = || {
        let (mut project, layer) = self::project();
        let doc = scene(&project).scene.clone().unwrap();
        let peel = new_modifier(&doc, id("peel"), ModifierType::SurfacePeel, &[id(CUBE)]);
        assert!(UndoRedoManager::new().execute(Box::new(add_modifier(&layer, &doc, peel)), &mut project));
        let doc = scene(&project).scene.clone().unwrap();
        let waves = new_modifier(&doc, id("waves"), ModifierType::SurfaceWaves, &[]);
        assert!(UndoRedoManager::new().execute(Box::new(add_modifier(&layer, &doc, waves)), &mut project));
        project
    };
    let card = |project: &Project, card: &str| match scene(project).scene.as_ref().unwrap().find(&id(card)).map(|item| &item.fields) {
        Some(ItemFields::Modifier(fields)) => fields.clone(),
        _ => panic!("{card} is a card"),
    };
    let chain = |project: &Project| -> Vec<String> {
        let doc = scene(project).scene.as_ref().unwrap();
        doc.items.iter().filter(|item| item.kind() == ItemKind::Modifier).map(|item| item.id.to_string()).collect()
    };
    let base = carded();
    assert_eq!(chain(&base), ["peel", "waves"]);
    assert_eq!(card(&base, "peel").targets, ModifierTargets::Items(vec![id(CUBE)]), "Add aims at the selection");
    assert_eq!(card(&base, "waves").targets, ModifierTargets::Scene, "Add with nothing selected aims at the scene");
    let doc = scene(&base).scene.clone().unwrap();
    let off = ModifierFields { on: false, ..card(&base, "peel") };
    let swapped = ModifierFields { modifier: ModifierType::OrderedRecon, ..card(&base, "peel") };
    let third = new_modifier(&doc, id("echo"), ModifierType::SpatialEchoes, &[]);
    let card_edits: Vec<(&str, Box<dyn Command>)> = vec![
        ("add card", Box::new(add_modifier(&layer, &doc, third))),
        ("reorder", Box::new(move_modifier(&layer, &doc, &id("waves"), 0).unwrap())),
        ("toggle", Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id("peel"), ItemFields::Modifier(off)))),
        ("swap", Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id("peel"), ItemFields::Modifier(swapped)))),
        ("remove target", Box::new(RemoveSceneItemCommand::new(layer.clone(), id(CUBE)))),
    ];
    for (name, edit) in card_edits {
        let mut project = carded();
        // A row both types share starts at the new type's default on a swap.
        let shared = param_id(&id("peel"), "value");
        let fx = project.timeline.layers[0].gen_params_mut().unwrap();
        assert!(fx.set_base_param(&shared, 0.9));
        // First, so an undo that re-adds the cube's LFO keeps the order.
        fx.drivers.get_or_insert_with(Vec::new).insert(0, ParameterDriver::new(shared.clone(), BeatDivision::Quarter, DriverWaveform::Sine));
        let before = state(&project);
        let mut undo = UndoRedoManager::new();
        assert!(undo.execute(edit, &mut project), "{name} applies");
        let after = state(&project);
        assert_ne!(after, before, "{name} changes the scene");
        match name {
            "add card" => assert_eq!(chain(&project), ["peel", "waves", "echo"]),
            "reorder" => assert_eq!(chain(&project), ["waves", "peel"]),
            "swap" => {
                let peel = card(&project, "peel");
                assert_eq!(chain(&project), ["peel", "waves"], "a swap keeps the card's place");
                assert!(peel.on && peel.targets == ModifierTargets::Items(vec![id(CUBE)]), "and its toggle and targets");
                assert_eq!(scene(&project).get_base_param(&shared), 0.5, "its rows start at the new type's defaults");
                let kept = scene(&project).drivers.iter().flatten().any(|d| d.param_id == shared);
                assert!(kept, "a row both types share keeps its LFO");
            }
            "remove target" => assert_eq!(card(&project, "peel").targets, ModifierTargets::Items(Vec::new())),
            _ => {}
        }
        assert!(undo.undo(&mut project));
        assert_eq!(state(&project), before, "{name} undoes exactly");
        assert!(undo.redo(&mut project));
        assert_eq!(state(&project), after, "{name} redoes exactly");
    }

    // Ungroup, from a grouped scene.
    let (mut project, layer) = self::project();
    let mut undo = UndoRedoManager::new();
    let compiled = COMPILES.with(std::cell::Cell::get);
    let group = CompositeCommand::new(group_commands(&layer, &doc, &[id(CUBE)], id("rig")), "Group".into());
    assert!(undo.execute(Box::new(group), &mut project));
    assert_eq!(COMPILES.with(std::cell::Cell::get) - compiled, 1, "a composite of scene edits compiles once");
    let before = state(&project);
    let grouped = scene(&project).scene.clone().unwrap();
    let ungroup = CompositeCommand::new(ungroup_commands(&layer, &grouped, &id("rig")), "Ungroup".into());
    assert!(undo.execute(Box::new(ungroup), &mut project), "ungroup applies");
    assert!(scene(&project).scene.as_ref().unwrap().find(&id("rig")).is_none(), "the group is gone");
    assert!(undo.undo(&mut project));
    assert_eq!(state(&project), before, "ungroup undoes exactly");

    // The graph editor's side and the Scene panel's side undo on their own
    // (ruling 18): undoing a scene edit keeps a node wired after the Scene
    // node, and no edit writes inside the Scene node or removes it.
    use crate::commands::graph::{AddGraphNodeCommand, RemoveGraphNodeCommand};
    let (mut project, layer) = self::project();
    let mut undo = UndoRedoManager::new();
    let target = GraphTarget::Generator(layer.clone());
    let catalog = scene(&project).graph.clone().unwrap();
    let post = AddGraphNodeCommand::new(target.clone(), "node.blur".into(), None, catalog.clone());
    assert!(undo.execute(Box::new(post), &mut project));
    let with_post = scene(&project).graph.clone();
    let lamp = AddSceneItemCommand::new(layer.clone(), ItemKind::Light, None, 0, None, id("lamp"));
    assert!(undo.execute(Box::new(lamp), &mut project));
    assert!(undo.undo(&mut project));
    assert_eq!(scene(&project).graph, with_post, "undoing a scene edit keeps the post-processing");
    let sealed = manifold_core::scene_node::scene_node(scene(&project).graph.as_ref().unwrap()).unwrap().id;
    let before = state(&project);
    let inside = AddGraphNodeCommand::new(target.clone(), "node.blur".into(), None, catalog.clone()).with_scope(vec![sealed]);
    assert!(!undo.execute(Box::new(inside), &mut project), "an edit inside the Scene node is refused");
    assert!(!undo.execute(Box::new(RemoveGraphNodeCommand::new(target, sealed, catalog)), &mut project), "and so is its removal");
    assert_eq!(state(&project), before);

    // Undo puts back only what the edit changed: a knob turned since, outside
    // undo (a MIDI-mapped dial), keeps its value.
    let (mut project, layer) = self::project();
    let mut undo = UndoRedoManager::new();
    let lamp = AddSceneItemCommand::new(layer.clone(), ItemKind::Light, None, 0, None, id("lamp"));
    assert!(undo.execute(Box::new(lamp), &mut project));
    let knob = param_id(&id(CUBE), "value");
    assert!(project.timeline.layers[0].gen_params_mut().unwrap().set_base_param(&knob, 0.3));
    assert!(undo.undo(&mut project));
    assert!(scene(&project).scene.as_ref().unwrap().find(&id("lamp")).is_none(), "the light goes");
    assert_eq!(scene(&project).get_base_param(&knob), 0.3, "the knob turned since stays where it is");
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

/// A second Environment would light nothing, so Duplicate refuses it with
/// the reason and leaves the scene as it was; the one Environment still moves.
#[test]
fn a_second_environment_is_refused_with_its_reason() {
    let (mut project, layer) = project();
    let before = state(&project);
    let mut duplicate = AddSceneItemCommand::new(layer.clone(), ItemKind::Light, None, 0, Some(id("environment")), id("environment2"));
    duplicate.execute(&mut project);
    assert!(!duplicate.was_applied());
    assert_eq!(duplicate.rejection_reason(), Some(manifold_core::scene_doc::ONE_ENVIRONMENT));
    assert_eq!(state(&project), before, "a refused duplicate changes nothing");
    let mut undo = UndoRedoManager::new();
    assert!(undo.execute(Box::new(ReparentSceneItemCommand::new(layer, id("environment"), None, 3)), &mut project), "the one Environment still moves");
}

/// A Water takes one Fill: a Fill added, duplicated or set into a filled
/// Water is refused with the reason; an Inflow still joins.
#[test]
fn a_second_fill_is_refused_with_its_reason() {
    use manifold_core::scene_doc::{ONE_FILL, SourceRole, WaterSourceFields};
    let (mut project, layer) = project();
    let fill = |role| ItemFields::WaterSource(WaterSourceFields { role, ..WaterSourceFields::default() });
    let mut undo = UndoRedoManager::new();
    assert!(undo.execute(Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Water, None, 0, None, id("water"))), &mut project));
    let add = |source: &str, role| -> Box<dyn Command> {
        Box::new(CompositeCommand::new(
            vec![
                Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::WaterSource, Some(id("water")), 0, None, id(source))),
                Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id(source), fill(role))),
            ],
            "Add Source".into(),
        ))
    };
    assert!(undo.execute(add("fill", SourceRole::Fill), &mut project), "the first Fill joins");
    let before = state(&project);

    let mut second = add("fill2", SourceRole::Fill);
    second.execute(&mut project);
    assert!(!second.was_applied());
    assert_eq!(second.rejection_reason(), Some(ONE_FILL));
    assert_eq!(state(&project), before, "a refused Add changes nothing");

    let mut duplicate = AddSceneItemCommand::new(layer.clone(), ItemKind::WaterSource, Some(id("water")), 1, Some(id("fill")), id("fill3"));
    duplicate.execute(&mut project);
    assert_eq!(duplicate.rejection_reason(), Some(ONE_FILL));
    assert_eq!(state(&project), before, "a refused duplicate changes nothing");

    assert!(undo.execute(add("inflow", SourceRole::Inflow), &mut project), "an Inflow still joins");
    let mut set = SetSceneItemFieldsCommand::new(layer, id("inflow"), fill(SourceRole::Fill));
    set.execute(&mut project);
    assert_eq!(set.rejection_reason(), Some(ONE_FILL), "an Inflow cannot become a second Fill");
}

/// Model facts only arrive: an undo back past their arrival keeps them, so
/// a model never falls back to a plain mesh.
#[test]
fn undo_keeps_model_facts_read_since_the_edit() {
    let (mut project, layer) = project();
    let mut undo = UndoRedoManager::new();
    assert!(undo.execute(Box::new(RenameSceneItemCommand::new(layer, id(CUBE), "Box")), &mut project));
    let inst = project.timeline.layers[0].gen_params_mut().unwrap();
    let mut doc = inst.scene.clone().unwrap();
    doc.models.insert("a.glb".into(), manifold_core::scene_doc::ModelFacts::new(()));
    inst.scene = Some(doc);
    assert!(undo.undo(&mut project));
    assert!(scene(&project).scene.as_ref().unwrap().models.contains("a.glb"), "the undo dropped the facts");
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
        clips: Vec::new(),
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
