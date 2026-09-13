use super::super::test_support::*;
use super::super::*;
use crate::command::Command;
use manifold_core::LayerId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
use manifold_core::effect_graph_def::{
    BindingDef, GROUP_TYPE_ID, ParamSpecDef, PresetMetadata, StringBindingDef,
};
use manifold_core::layer::Layer;
use manifold_core::types::LayerType;

/// A single `node.render_scene` node (id 0) with `objects`/`lights` set to
/// the given counts — the fixture `AddSceneObjectCommand`/
/// `AddSceneLightCommand` operate against.
fn render_scene_graph(objects: u32, lights: u32) -> EffectGraphDef {
    let mut render = EffectGraphNode {
        id: 0,
        node_id: manifold_core::NodeId::new("render"),
        type_id: "node.render_scene".to_string(),
        handle: Some("render".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    };
    render.params.insert(
        "objects".to_string(),
        SerializedParamValue::Float {
            value: objects as f32,
        },
    );
    render.params.insert(
        "lights".to_string(),
        SerializedParamValue::Float {
            value: lights as f32,
        },
    );
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![render],
        wires: vec![],
    }
}

/// A generator-hosted twin of [`project_with_graph`] (BUG-295 regression
/// coverage): production scene commands always target
/// `GraphTarget::Generator` — `is_generator()` gates
/// `gather_known_params`'s full-`meta.params`-authority branch, which is
/// what actually lets a freshly stamped exposure (whose binding carries
/// `user_added: false`, `scene_exposure.rs`) surface into the live
/// manifest. An `Effect`-target fixture like `project_with_graph` would
/// silently take the OTHER `gather_known_params` branch (registry
/// `param_defs` + `user_added`-flagged bindings only) and never see the
/// stamped param at all — not a proof of the live-refresh fix.
fn project_with_generator_graph(def: EffectGraphDef) -> (Project, LayerId) {
    let mut project = Project::default();
    let mut layer = Layer::new("Test Layer".to_string(), LayerType::Generator, 0);
    let lid = layer.layer_id.clone();
    layer.gen_params_or_init().graph = Some(def);
    project.timeline.layers.push(layer);
    (project, lid)
}

#[test]
fn add_scene_object_command_bumps_count_builds_group_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = AddSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        2, // next_index — matches the fixture's current `objects` (2)
        (100.0, 200.0),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("objects"),
        Some(&SerializedParamValue::Float { value: 3.0 }),
        "objects bumped by one"
    );

    let group = def
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("Object 3"))
        .expect("named group created");
    assert_eq!(group.editor_pos, Some((100.0, 200.0)));
    let body = group.group.as_deref().expect("is a group node");
    assert_eq!(
        body.nodes.len(),
        5,
        "cube + material + transform + scene_object bind + group_output boundary"
    );
    assert!(body.nodes.iter().any(|n| n.type_id == "node.cube_mesh"));
    assert!(
        body.nodes
            .iter()
            .any(|n| n.type_id == "node.phong_material")
    );
    assert!(body.nodes.iter().any(|n| n.type_id == "node.transform_3d"));
    assert!(body.nodes.iter().any(|n| n.type_id == "node.scene_object"));
    assert_eq!(
        body.wires.len(),
        4,
        "mesh/material/transform wired to scene_object, scene_object wired to the group_output"
    );
    assert_eq!(body.interface.outputs.len(), 1, "a single Object output");
    assert_eq!(body.interface.outputs[0].name, "object");
    assert_eq!(body.interface.outputs[0].port_type, "Object");

    // SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D3/D4: the group's single
    // `object` output wired to render_scene's new object_2 slot.
    assert!(def.wires.iter().any(|w| w.from_node == group.id
        && w.from_port == "object"
        && w.to_node == 0
        && w.to_port == "object_2"));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-add graph exactly (inverse-pair)"
    );
}

/// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneObjectCommand`
/// stamps the material/transform/scene_object metadata the caller hands
/// it into the def's TOP-LEVEL `preset_metadata`, targeting each new
/// node's bare `NodeId`, with the section named per the convention
/// (`"{handle} — Material"` / `"{handle} — Transform"` / `handle`).
/// Undo restores `preset_metadata` verbatim; execute→undo→redo is stable.
#[test]
fn add_scene_object_command_stamps_exposures_and_undo_redo_are_stable() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

    let mut cmd = AddSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        vec![scene_param_meta("ambient", "Ambient")],
        vec![scene_param_meta("pos_x", "X")],
        vec![scene_param_meta("visible", "Visible")],
        mirror_catalog_default(),
    );

    // Asserted after both the first execute and the redo: `execute`
    // mints a fresh random NodeId every call (`scene_build_node` ->
    // `manifold_core::short_id()`, pre-existing behavior, not a P1
    // change), so graph IDENTITY isn't byte-stable across redo — only
    // the STRUCTURE the stamping produces is. "Stable" here means the
    // exposures always target whichever node currently sits in that
    // role, not a frozen id.
    let assert_stamped = |project: &Project| {
        let def = graph_of(project, &fx);
        let group = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("Object 1"))
            .unwrap();
        let body = group.group.as_deref().unwrap();
        let mat_node = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.phong_material")
            .unwrap();
        let transform_node = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.transform_3d")
            .unwrap();
        let scene_object_node = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .unwrap();

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("P1 stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 3, "one ParamSpecDef per exposed param");
        assert_eq!(meta.bindings.len(), 3);

        let has_binding = |node_id: &NodeId, param: &str, section: &str| {
            meta.bindings.iter().any(|b| {
                matches!(&b.target, BindingTarget::Node { node_id: nid, param: p } if nid == node_id && p == param)
            }) && meta.params.iter().any(|p| p.section.as_deref() == Some(section))
        };
        assert!(
            has_binding(&mat_node.node_id, "ambient", "Object 1 — Material"),
            "material exposure targets the grouped node's bare NodeId, section 'Object 1 — Material'"
        );
        assert!(
            has_binding(&transform_node.node_id, "pos_x", "Object 1 — Transform"),
            "transform exposure targets the grouped node's bare NodeId, section 'Object 1 — Transform'"
        );
        assert!(
            has_binding(&scene_object_node.node_id, "visible", "Object 1"),
            "scene_object exposure targets the grouped node's bare NodeId, section 'Object 1'"
        );
    };

    cmd.execute(&mut project);
    assert_stamped(&project);

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert!(
        def.preset_metadata.is_none(),
        "undo restores the pre-add (empty) preset_metadata verbatim"
    );

    cmd.execute(&mut project); // redo
    assert_stamped(&project);
}

#[test]
fn add_scene_light_command_bumps_count_wires_bare_light_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = AddSceneLightCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        1, // next_index — matches the fixture's current `lights` (1)
        (-260.0, 50.0),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("lights"),
        Some(&SerializedParamValue::Float { value: 2.0 }),
        "lights bumped by one"
    );

    let light = def
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("light_1"))
        .expect("bare light node created");
    assert!(light.group.is_none(), "D7a: no group around the light");
    assert_eq!(light.type_id, "node.light");
    assert_eq!(light.editor_pos, Some((-260.0, 50.0)));

    // D7a defaults, transcribed.
    assert_eq!(
        light.params.get("mode"),
        Some(&SerializedParamValue::Enum { value: 0 })
    );
    assert_eq!(
        light.params.get("color_r"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        light.params.get("color_g"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        light.params.get("color_b"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        light.params.get("intensity"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        light.params.get("cast_shadows"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );

    // Auto-wired into the new light_1 slot — "add means added," never a
    // bumped count with a dead port.
    assert!(def.wires.iter().any(|w| w.from_node == light.id
        && w.from_port == "out"
        && w.to_node == 0
        && w.to_port == "light_1"));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-add graph exactly (inverse-pair)"
    );
}

/// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneLightCommand`
/// stamps the caller-supplied light metadata into the def's TOP-LEVEL
/// `preset_metadata`, targeting the new light's bare `NodeId`, section
/// "Light N" (1-based display convention, independent of the node's own
/// internal `light_{k}` handle). Undo restores `preset_metadata`
/// verbatim; execute→undo→redo is structurally stable (see the
/// AddSceneObjectCommand sibling test for why redo isn't byte-identical:
/// `execute` mints a fresh random NodeId every call).
#[test]
fn add_scene_light_command_stamps_exposures_and_undo_redo_are_stable() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

    let mut cmd = AddSceneLightCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (-260.0, 50.0),
        vec![scene_param_meta("intensity", "Intensity")],
        mirror_catalog_default(),
    );

    let assert_stamped = |project: &Project| {
        let def = graph_of(project, &fx);
        let light = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.light")
            .unwrap();

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("P1 stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(meta.params[0].section.as_deref(), Some("Light 1"));
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, param } if *node_id == light.node_id && param == "intensity"
            )),
            "light exposure targets the light's bare NodeId"
        );
    };

    cmd.execute(&mut project);
    assert_stamped(&project);

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert!(
        def.preset_metadata.is_none(),
        "undo restores the pre-add (empty) preset_metadata verbatim"
    );

    cmd.execute(&mut project); // redo
    assert_stamped(&project);
}

/// A fixture with 3 objects wired as `AddSceneObjectCommand` builds them
/// (group + mesh_k/material_k/transform_k wires), so removal tests can
/// exercise the middle-object renumbering case (BUG-193's core claim).
/// Builds `count` bare `node.scene_object` producers wired directly to
/// `render_scene`'s `object_k` ports (the D3/D4 shape) — hand-built
/// rather than via `AddSceneObjectCommand`, whose `catalog_default` still
/// emits the pre-migration legacy-port shape (P3's job to retarget, see
/// docs/BUG_BACKLOG.md). Returns the def and each producer's node id.
fn render_scene_with_objects(count: u32) -> (EffectGraphDef, Vec<u32>) {
    let mut def = render_scene_graph(0, 0);
    def.nodes
        .iter_mut()
        .find(|n| n.id == 0)
        .unwrap()
        .params
        .insert(
            "objects".to_string(),
            SerializedParamValue::Float {
                value: count as f32,
            },
        );
    let mut object_ids = Vec::new();
    for k in 0..count {
        let id = 100 + k;
        def.nodes.push(EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(format!("obj{k}")),
            type_id: "node.scene_object".to_string(),
            handle: Some(format!("Object {}", k + 1)),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        def.wires.push(EffectGraphWire {
            from_node: id,
            from_port: "object".to_string(),
            to_node: 0,
            to_port: format!("object_{k}"),
        });
        object_ids.push(id);
    }
    (def, object_ids)
}

#[test]
fn remove_scene_object_middle_deletes_group_and_renumbers_survivors() {
    let (fixture, object_ids) = render_scene_with_objects(3);
    let (mut project, fx) = project_with_graph(fixture);
    let before = graph_of(&project, &fx).clone();

    let mut cmd = RemoveSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        1, // remove the MIDDLE object (index 1 of 0,1,2)
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("objects"),
        Some(&SerializedParamValue::Float { value: 2.0 }),
        "objects decremented by one"
    );
    assert!(
        !def.nodes.iter().any(|n| n.id == object_ids[1]),
        "the removed object's scene_object node is gone"
    );
    assert!(
        def.nodes.iter().any(|n| n.id == object_ids[0]),
        "object 0 survives untouched"
    );
    assert!(
        def.nodes.iter().any(|n| n.id == object_ids[2]),
        "object 2 survives (renumbered)"
    );
    // Object 0 stays at slot 0.
    assert!(def.wires.iter().any(|w| w.from_node == object_ids[0]
        && w.from_port == "object"
        && w.to_node == 0
        && w.to_port == "object_0"));
    // Object 2 (formerly slot 2) is renumbered down to slot 1.
    assert!(def.wires.iter().any(|w| w.from_node == object_ids[2]
        && w.from_port == "object"
        && w.to_node == 0
        && w.to_port == "object_1"));
    // No dangling slot-2 wires left behind.
    assert!(
        !def.wires
            .iter()
            .any(|w| w.to_node == 0 && w.to_port == "object_2")
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-remove graph exactly (inverse-pair)"
    );
}

#[test]
fn scene_modifier_explicit_object_delete_rejects_without_history_or_mutation() {
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let (mut project, local_target, _) = modifier_draft_fixture();
    let owner_target = local_target.host_target().unwrap().clone();
    let (scene, ids) = render_scene_with_objects(1);
    let node = scene
        .nodes
        .iter()
        .find(|node| node.id == ids[0])
        .unwrap()
        .node_id
        .clone();
    let host = project.graph_target_owner_mut(&owner_target).unwrap();
    let graph = host.graph.as_mut().unwrap();
    graph.nodes = scene.nodes;
    graph.wires = scene.wires;
    graph.scene_modifiers[0].targets = SceneTargetSelection::Explicit {
        objects: vec![SceneNodeRef {
            scope: vec![],
            node,
        }],
    };
    let before = graph.clone();
    let version = host.graph_structure_version;
    let command =
        RemoveSceneObjectCommand::new(owner_target.clone(), vec![], 0, 0, before.clone());
    let mut undo = crate::undo::UndoRedoManager::new();
    assert!(!undo.execute(Box::new(command), &mut project));
    let host = project.graph_target_owner(&owner_target).unwrap();
    assert_eq!(host.graph.as_ref(), Some(&before));
    assert_eq!(host.graph_structure_version, version);
    let host = project.graph_target_owner_mut(&owner_target).unwrap();
    let graph = host.graph.as_mut().unwrap();
    graph.scene_modifiers[0].targets = SceneTargetSelection::AllObjects;
    let all_objects = graph.clone();
    let command =
        RemoveSceneObjectCommand::new(owner_target.clone(), vec![], 0, 0, all_objects.clone());
    assert!(undo.execute(Box::new(command), &mut project));
    assert!(undo.undo(&mut project));
    assert_eq!(
        project
            .graph_target_owner(&owner_target)
            .unwrap()
            .graph
            .as_ref(),
        Some(&all_objects)
    );
}

#[test]
fn remove_scene_light_only_light_removes_node_and_zeroes_count() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 1));
    // Wire the fixture's declared single light exactly like
    // AddSceneLightCommand would (bare node, no group).
    {
        let mut cmd = AddSceneLightCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            0,
            0,
            (-260.0, 50.0),
            Vec::new(),
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);
    }
    let before = graph_of(&project, &fx).clone();
    let light_id = before
        .nodes
        .iter()
        .find(|n| n.type_id == "node.light")
        .expect("light node present")
        .id;

    let mut cmd = RemoveSceneLightCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("lights"),
        Some(&SerializedParamValue::Float { value: 0.0 }),
        "lights decremented to zero"
    );
    assert!(
        !def.nodes.iter().any(|n| n.id == light_id),
        "light node removed"
    );
    assert!(
        !def.wires
            .iter()
            .any(|w| w.to_node == 0 && w.to_port == "light_0"),
        "wire removed"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-remove graph exactly (inverse-pair)"
    );
}

/// Every stable [`NodeId`] and doc `id` anywhere in `nodes`, recursively
/// through nested groups — test helper mirroring `collect_node_ids` +
/// `max_node_id_over`, used to prove a duplicate mints fresh identity
/// throughout its whole cloned subtree, not just the top node.
fn collect_ids(nodes: &[EffectGraphNode], doc_ids: &mut Vec<u32>, node_ids: &mut Vec<NodeId>) {
    for n in nodes {
        doc_ids.push(n.id);
        if !n.node_id.is_empty() {
            node_ids.push(n.node_id.clone());
        }
        if let Some(body) = n.group.as_deref() {
            collect_ids(&body.nodes, doc_ids, node_ids);
        }
    }
}

#[test]
fn duplicate_scene_object_command_clones_grouped_object_with_fresh_ids_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    AddSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        mirror_catalog_default(),
    )
    .execute(&mut project);
    let before = graph_of(&project, &fx).clone();
    let (mut orig_doc_ids, mut orig_node_ids) = (Vec::new(), Vec::new());
    collect_ids(&before.nodes, &mut orig_doc_ids, &mut orig_node_ids);

    let mut cmd = DuplicateSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0, // duplicate object 0 (the only object)
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("objects"),
        Some(&SerializedParamValue::Float { value: 2.0 }),
        "objects bumped by one"
    );
    let clone = def
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("Object 1 2"))
        .expect("clone named with the D11 ' 2' suffix");
    assert!(
        def.wires.iter().any(|w| w.from_node == clone.id
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_1"),
        "clone wired to the next free object slot"
    );

    // D11: every id in the clone's subtree is fresh — no overlap with
    // the original's doc ids or stable NodeIds anywhere.
    let (mut all_doc_ids, mut all_node_ids) = (Vec::new(), Vec::new());
    collect_ids(&def.nodes, &mut all_doc_ids, &mut all_node_ids);
    let mut clone_doc_ids = Vec::new();
    let mut clone_node_ids = Vec::new();
    collect_ids(
        std::slice::from_ref(clone),
        &mut clone_doc_ids,
        &mut clone_node_ids,
    );
    for id in &clone_doc_ids {
        assert!(
            !orig_doc_ids.contains(id),
            "clone doc id {id} must not reuse an original doc id"
        );
    }
    for nid in &clone_node_ids {
        assert!(
            !orig_node_ids.contains(nid),
            "clone NodeId {nid:?} must not reuse an original NodeId"
        );
    }
    // No duplicate doc ids anywhere in the whole def (fresh minting is
    // globally unique, not just locally).
    let mut sorted = all_doc_ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        all_doc_ids.len(),
        "no doc id collisions anywhere in the def"
    );

    // No duplicate handles among SIBLINGS at any one scope — the real
    // constraint the flattener's group-name-prefixing composite naming
    // needs (`Graph::add_node_named` builds on the flattened, prefixed
    // names; two DIFFERENT groups' identically-named inner leaves don't
    // collide because the group name prefixes them, but two nodes in
    // the SAME scope sharing a handle do). The clone's own group got a
    // distinct top handle ("Object 1 2" vs the source's "Object 1"), so
    // this must hold recursively through both subtrees.
    fn assert_no_sibling_handle_collisions(nodes: &[EffectGraphNode]) {
        let mut seen = std::collections::HashSet::new();
        for n in nodes {
            if let Some(h) = &n.handle {
                assert!(
                    seen.insert(h.clone()),
                    "sibling handle collision at this scope: {h:?}"
                );
            }
            if let Some(body) = n.group.as_deref() {
                assert_no_sibling_handle_collisions(&body.nodes);
            }
        }
    }
    assert_no_sibling_handle_collisions(&def.nodes);

    // D6: the clone's inner scene_object handle stays in sync with the
    // group's handle.
    let clone_body = clone.group.as_deref().expect("clone is a group");
    let inner_object = clone_body
        .nodes
        .iter()
        .find(|n| n.type_id == "node.scene_object")
        .unwrap();
    assert_eq!(inner_object.handle.as_deref(), Some("Object 1 2"));

    // D11: transform_3d.pos_x offset by +0.5 on the clone.
    let clone_transform = clone_body
        .nodes
        .iter()
        .find(|n| n.type_id == "node.transform_3d")
        .unwrap();
    assert_eq!(
        clone_transform.params.get("pos_x"),
        Some(&SerializedParamValue::Float { value: 0.5 })
    );

    // D11: card exposes are not cloned.
    assert!(clone_body.nodes.iter().all(|n| n.exposed_params.is_empty()));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-duplicate graph exactly (inverse-pair)"
    );
}

/// BUG-212: `string_bindings` (the importer's "Model File" path
/// plumbing — one `StringBindingDef` per file-dependent node, fanned
/// out under a shared outer id) must follow a duplicated object's
/// cloned nodes, re-targeted at the clone's fresh `NodeId`, same
/// `id`/`label`/`default_value` — the same mechanism as D5's rename
/// sweep, exercised here for `DuplicateSceneObjectCommand`.
#[test]
fn duplicate_scene_object_command_clones_string_bindings_onto_fresh_node_id_and_undo_restores()
{
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    AddSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        mirror_catalog_default(),
    )
    .execute(&mut project);

    // Simulate the importer's "Model File" binding: one string_bindings
    // entry targeting the object's mesh node by its stable NodeId.
    let mesh_node_id = {
        let def = graph_of(&project, &fx);
        let group = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("Object 1"))
            .unwrap();
        let mesh = group
            .group
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.type_id == "node.cube_mesh")
            .unwrap();
        mesh.node_id.clone()
    };
    {
        let effect = project.find_effect_by_id_mut(&fx).unwrap();
        let def = effect.graph.as_mut().unwrap();
        def.preset_metadata = Some(PresetMetadata {
            id: PresetTypeId::new("test.scene"),
            display_name: "Test Scene".into(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            scene_modifier: None,
            scene_bounds: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: vec![StringBindingDef {
                id: "model_file".into(),
                label: "Model File".into(),
                default_value: "assets/hero.glb".into(),
                target: BindingTarget::Node {
                    node_id: mesh_node_id.clone(),
                    param: "path".into(),
                },
            }],
        });
    }
    let before_meta = graph_of(&project, &fx).preset_metadata.clone().unwrap();

    let mut cmd = DuplicateSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0, // duplicate object 0 (the only object)
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let clone = def
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("Object 1 2"))
        .unwrap();
    let clone_mesh = clone
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.cube_mesh")
        .unwrap();

    let meta = def.preset_metadata.as_ref().unwrap();
    assert_eq!(
        meta.string_bindings.len(),
        2,
        "the clone's mesh node gets its own string_bindings entry"
    );
    let clone_binding = meta
        .string_bindings
        .iter()
        .find(|b| matches!(&b.target, BindingTarget::Node { node_id, .. } if *node_id == clone_mesh.node_id))
        .expect("a string_bindings entry targets the clone's fresh NodeId");
    assert_eq!(clone_binding.id, "model_file");
    assert_eq!(
        clone_binding.default_value, "assets/hero.glb",
        "same default_value as the source entry"
    );
    // The original entry (still targeting the SOURCE mesh's NodeId) is untouched.
    assert!(meta.string_bindings.iter().any(
        |b| matches!(&b.target, BindingTarget::Node { node_id, .. } if *node_id == mesh_node_id)
    ));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def.preset_metadata.as_ref().unwrap(),
        &before_meta,
        "undo restores string_bindings exactly (inverse-pair)"
    );
}

#[test]
fn rename_scene_object_command_renames_group_and_sweeps_section_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    AddSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        mirror_catalog_default(),
    )
    .execute(&mut project);
    let def = graph_of(&project, &fx).clone();
    let group = def
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("Object 1"))
        .unwrap();
    let group_id = group.id;
    let mat_node = group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.phong_material")
        .unwrap();
    let (mat_node_id, mat_u32_id) = (mat_node.node_id.clone(), mat_node.id);

    ToggleNodeParamExposeCommand::new(
        GraphTarget::Effect(fx.clone()),
        mat_node_id,
        mat_u32_id,
        "mat_0".to_string(),
        "ambient".to_string(),
        true,
        mirror_catalog_default(),
        "Ambient".to_string(),
        0.0,
        1.0,
        0.0,
        manifold_core::effects::ParamConvert::Float,
        false,
        Vec::new(),
    )
    .with_scope(vec![group_id])
    .execute(&mut project);
    let ub_id = project
        .find_effect_by_id(&fx)
        .unwrap()
        .user_param_bindings()[0]
        .id
        .clone();
    assert_eq!(
        project
            .find_effect_by_id(&fx)
            .unwrap()
            .params
            .get(&ub_id)
            .unwrap()
            .spec
            .section
            .as_deref(),
        Some("Object 1"),
        "setup: expose seeded the section from the group name"
    );

    let before = graph_of(&project, &fx).clone();
    let mut cmd = RenameSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        group_id,
        "Hero".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let group = def.nodes.iter().find(|n| n.id == group_id).unwrap();
    assert_eq!(
        group.handle.as_deref(),
        Some("Hero"),
        "group handle renamed"
    );
    let inner_object = group
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.type_id == "node.scene_object")
        .unwrap();
    assert_eq!(
        inner_object.handle.as_deref(),
        Some("Hero"),
        "scene_object handle kept in sync (D6)"
    );
    assert_eq!(
        project
            .find_effect_by_id(&fx)
            .unwrap()
            .params
            .get(&ub_id)
            .unwrap()
            .spec
            .section
            .as_deref(),
        Some("Hero"),
        "D5: card section follows the rename"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-rename graph exactly (inverse-pair)"
    );
    assert_eq!(
        project
            .find_effect_by_id(&fx)
            .unwrap()
            .params
            .get(&ub_id)
            .unwrap()
            .spec
            .section
            .as_deref(),
        Some("Object 1"),
        "undo restores the pre-rename section"
    );
}

#[test]
fn rename_scene_object_command_ungrouped_renames_bare_node_and_undo_restores() {
    let (fixture, object_ids) = render_scene_with_objects(2);
    let (mut project, fx) = project_with_graph(fixture);
    let before = graph_of(&project, &fx).clone();

    let mut cmd = RenameSceneObjectCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        object_ids[0],
        "Renamed".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let node = def.nodes.iter().find(|n| n.id == object_ids[0]).unwrap();
    assert_eq!(node.handle.as_deref(), Some("Renamed"));
    assert!(
        node.group.is_none(),
        "ungrouped node stays bare, no group is fabricated"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-rename graph exactly (inverse-pair)"
    );
}

#[test]
fn set_node_handle_command_renames_light_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    AddSceneLightCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        Vec::new(),
        mirror_catalog_default(),
    )
    .execute(&mut project);
    let before = graph_of(&project, &fx).clone();
    let light_id = before
        .nodes
        .iter()
        .find(|n| n.type_id == "node.light")
        .unwrap()
        .id;

    let mut cmd = SetNodeHandleCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        light_id,
        "Key Light".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    assert_eq!(
        def.nodes
            .iter()
            .find(|n| n.id == light_id)
            .unwrap()
            .handle
            .as_deref(),
        Some("Key Light")
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-rename graph exactly (inverse-pair)"
    );
}

#[test]
fn add_scene_environment_command_spawns_bake_environment_and_wires_envmap() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = AddSceneEnvironmentCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (10.0, 20.0),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let env = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.bake_environment")
        .expect("environment node created");
    assert_eq!(env.editor_pos, Some((10.0, 20.0)));
    assert_eq!(
        env.params.get("intensity"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert!(def.wires.iter().any(|w| w.from_node == env.id
        && w.from_port == "envmap"
        && w.to_node == 0
        && w.to_port == "envmap"));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-add graph exactly (inverse-pair)"
    );
}

#[test]
fn add_scene_fog_command_spawns_atmosphere_and_wires_atmosphere_port() {
    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = AddSceneFogCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (30.0, 40.0),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let fog = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.atmosphere")
        .expect("fog node created");
    assert_eq!(fog.editor_pos, Some((30.0, 40.0)));
    assert!(def.wires.iter().any(|w| w.from_node == fog.id
        && w.from_port == "atmosphere"
        && w.to_node == 0
        && w.to_port == "atmosphere"));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-add graph exactly (inverse-pair)"
    );
}

/// R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneEnvironmentCommand`
/// stamps the caller-supplied environment metadata into the def's
/// TOP-LEVEL `preset_metadata`, targeting the new environment node's bare
/// `NodeId`, section "Environment" — same P1 stamp shape
/// `AddSceneLightCommand` performs for its own node. Regression coverage
/// for the R1 bug: a freshly-added environment was structurally invisible
/// in the scene panel because `world_sections` (`state_sync.rs`'s
/// `sections_for_doc_ids`) came back empty with nothing stamped. Undo
/// restores `preset_metadata` verbatim; execute→undo→redo is stable.
#[test]
fn add_scene_environment_command_stamps_exposures_and_undo_redo_are_stable() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

    let mut cmd = AddSceneEnvironmentCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (10.0, 20.0),
        vec![scene_param_meta("intensity", "Intensity")],
        mirror_catalog_default(),
    );

    let assert_stamped = |project: &Project| {
        let def = graph_of(project, &fx);
        let env = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.bake_environment")
            .unwrap();

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("R1 stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(meta.params[0].section.as_deref(), Some("Environment"));
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, param } if *node_id == env.node_id && param == "intensity"
            )),
            "environment exposure targets the environment node's bare NodeId"
        );
    };

    cmd.execute(&mut project);
    assert_stamped(&project);

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert!(
        def.preset_metadata.is_none(),
        "undo restores the pre-add (empty) preset_metadata verbatim"
    );

    cmd.execute(&mut project); // redo
    assert_stamped(&project);
}

/// R1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `AddSceneFogCommand`
/// stamps the caller-supplied fog metadata into the def's TOP-LEVEL
/// `preset_metadata`, targeting the new fog node's bare `NodeId`, section
/// "Atmosphere" — same P1 stamp shape `AddSceneLightCommand` performs for
/// its own node. Regression coverage for the R1 bug this lane fixes: a
/// freshly-added fog node was structurally invisible in the scene panel
/// (not even the fallback row rendered) because `world_sections`
/// (`state_sync.rs`'s `sections_for_doc_ids`) came back empty with
/// nothing stamped, and `build_filtered_properties` iterates an empty
/// section list. Undo restores `preset_metadata` verbatim; execute→undo→
/// redo is stable.
#[test]
fn add_scene_fog_command_stamps_exposures_and_undo_redo_are_stable() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));

    let mut cmd = AddSceneFogCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (30.0, 40.0),
        vec![scene_param_meta("density", "Density")],
        mirror_catalog_default(),
    );

    let assert_stamped = |project: &Project| {
        let def = graph_of(project, &fx);
        let fog = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.atmosphere")
            .unwrap();

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("R1 stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(meta.params[0].section.as_deref(), Some("Atmosphere"));
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, param } if *node_id == fog.node_id && param == "density"
            )),
            "fog exposure targets the fog node's bare NodeId"
        );
    };

    cmd.execute(&mut project);
    assert_stamped(&project);

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert!(
        def.preset_metadata.is_none(),
        "undo restores the pre-add (empty) preset_metadata verbatim"
    );

    cmd.execute(&mut project); // redo
    assert_stamped(&project);
}

/// BUG-295: `AddSceneFogCommand` stamps the fog exposure into
/// `def.preset_metadata.params` (proven above), but until
/// `refresh_manifest_from_graph` is ALSO wired to run post-stamp, that
/// stamp is invisible to the LIVE `PresetInstance.params` the panel
/// actually reads — the bug's own root-cause finding (`reconcile_manifest`
/// only fires from a load-time `pending_wire` stash, never from a runtime
/// graph edit). Regression coverage for the live-manifest half of the fix:
/// execute → the fog row is in `inst.params`, not just `preset_metadata`;
/// undo → the row is gone from `inst.params`; redo → it's back. Targets
/// `GraphTarget::Generator` (see `project_with_generator_graph`) so
/// `gather_known_params`'s generator branch actually picks up the
/// stamped `meta.params` entry regardless of the binding's `user_added`
/// flag (scene exposures are always `user_added: false`).
#[test]
fn add_scene_fog_command_refreshes_live_manifest_and_undo_redo_restore_it() {
    let (mut project, lid) = project_with_generator_graph(render_scene_graph(0, 0));

    let mut cmd = AddSceneFogCommand::new(
        GraphTarget::Generator(lid.clone()),
        vec![],
        0,
        (30.0, 40.0),
        vec![scene_param_meta("density", "Density")],
        mirror_catalog_default(),
    );

    let has_fog_row = |project: &Project| {
        project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .params
            .iter()
            .any(|p| p.spec.section.as_deref() == Some("Atmosphere"))
    };

    cmd.execute(&mut project);
    assert!(
        has_fog_row(&project),
        "BUG-295: freshly-stamped fog param must land in the live inst.params after execute"
    );

    cmd.undo(&mut project);
    assert!(
        !has_fog_row(&project),
        "undo must remove the fog row from the live manifest, not just def.preset_metadata"
    );

    cmd.execute(&mut project); // redo
    assert!(has_fog_row(&project), "redo must restore the live fog row");
}

/// BUG-p6x7: fog density slider range must scale with scene radius when
/// scene_bounds are present. A scene whose bbox diagonal is 20 units
/// (radius 10) gets density range (0, 0.2) — the slider covers the
/// useful per-world-unit range instead of the generic 0..1 band.
#[test]
fn add_scene_fog_scales_density_with_scene_bounds() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph({
        let mut def = render_scene_graph(0, 0);
        def.preset_metadata = Some(manifold_core::effect_graph_def::PresetMetadata {
            id: manifold_core::PresetTypeId::from_string("TestScene".to_string()),
            display_name: "Test".to_string(),
            category: "Geometry".to_string(),
            osc_prefix: "test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            // bbox diagonal = sqrt(20^2+20^2+20^2) = 34.64 -> radius = 17.32
            scene_modifier: None,
            scene_bounds: Some(([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0])),
        });
        def
    });

    let mut cmd = AddSceneFogCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (30.0, 40.0),
        vec![scene_param_meta("fog_density", "Density")],
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let meta = def.preset_metadata.as_ref().unwrap();
    let fog = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.atmosphere")
        .unwrap();

    let density_spec = meta.params.iter().find(|p| {
        meta.bindings.iter().any(|b| {
            b.id == p.id
                && matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == fog.node_id && param == "fog_density"
                )
        })
    }).expect("fog density exposure must be stamped");

    let (expected_min, expected_max) =
        super::super::fog_density_range(Some(([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0])))
            .unwrap();
    assert_eq!(density_spec.min, expected_min, "density min must be 0.0");
    // The widen rule expands the band to contain the stamped default (0.5
    // from scene_param_meta) when it falls outside the derived range.
    assert!(
        density_spec.max >= expected_max,
        "density max must be >= 2/radius ({}), got {}",
        expected_max,
        density_spec.max
    );
    assert!(
        density_spec.default_value <= density_spec.max,
        "density max must contain the stamped default"
    );
}

/// BUG-p6x7: without scene_bounds (procedural scene), fog density keeps
/// the generic 0..1 band.
#[test]
fn add_scene_fog_keeps_generic_density_band_without_scene_bounds() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(render_scene_graph(0, 0));
    let mut cmd = AddSceneFogCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        (30.0, 40.0),
        vec![scene_param_meta("fog_density", "Density")],
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let meta = def.preset_metadata.as_ref().unwrap();
    let fog = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.atmosphere")
        .unwrap();

    let density_spec = meta.params.iter().find(|p| {
        meta.bindings.iter().any(|b| {
            b.id == p.id
                && matches!(
                    &b.target,
                    BindingTarget::Node { node_id, param } if *node_id == fog.node_id && param == "fog_density"
                )
        })
    }).expect("fog density exposure must be stamped");

    // scene_param_meta defaults: min=0.0, max=1.0 — generic band untouched.
    assert_eq!(
        density_spec.min, 0.0,
        "density min must stay 0.0 without scene_bounds"
    );
    assert_eq!(
        density_spec.max, 1.0,
        "density max must stay 1.0 without scene_bounds"
    );
}

/// Unit test for the fog_density_range helper.
#[test]
fn fog_density_range_unit() {
    assert_eq!(super::super::fog_density_range(None), None);
    let (min, max) =
        super::super::fog_density_range(Some(([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]))).unwrap();
    assert_eq!(min, 0.0);
    // radius = max(0, 0.01) = 0.01 → max = 2/0.01 = 200.0
    assert!((max - 200.0).abs() < 1e-6);
    let (_, max) =
        super::super::fog_density_range(Some(([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0])))
            .unwrap();
    // bbox diagonal = sqrt(20^2+20^2+20^2) = sqrt(1200) = 20*sqrt(3)
    // radius = diagonal/2 = 10*sqrt(3) ≈ 17.32 -> max = 2/(10*sqrt(3)) ≈ 0.11547
    let expected_radius = 10.0 * (3.0_f32).sqrt();
    assert!((max - 2.0 / expected_radius).abs() < 1e-4);
}

/// BUG-295 value-preservation proof: `refresh_manifest_from_graph`
/// round-trips the CURRENT manifest through the same wire encoding the
/// file serializer uses before overlaying the graph's descriptors, so a
/// pre-existing param's live (possibly non-default) value must survive a
/// LATER structural edit's refresh — not just the freshly-stamped one's
/// own default. Sets a light's Intensity to a hand-picked non-default
/// value, then executes `AddSceneFogCommand` (a second, unrelated
/// structural edit) and asserts Intensity kept its value rather than
/// resetting to the spec default a naive `build_param_manifest(..., None)`
/// resync would have produced.
#[test]
fn add_scene_fog_command_refresh_preserves_existing_param_values() {
    let (mut project, lid) = project_with_generator_graph(render_scene_graph(0, 0));

    let mut add_light = AddSceneLightCommand::new(
        GraphTarget::Generator(lid.clone()),
        vec![],
        0,
        0,
        (0.0, 0.0),
        vec![scene_param_meta("intensity", "Intensity")],
        mirror_catalog_default(),
    );
    add_light.execute(&mut project);

    let intensity_id = project
        .timeline
        .find_layer_by_id(&lid)
        .unwrap()
        .1
        .gen_params()
        .unwrap()
        .params
        .iter()
        .find(|p| p.spec.name == "Intensity")
        .expect("add-light's own refresh surfaced the stamped Intensity param live")
        .id()
        .to_string();

    project
        .timeline
        .find_layer_by_id_mut(&lid)
        .unwrap()
        .1
        .gen_params_or_init()
        .params
        .get_mut(&intensity_id)
        .expect("intensity param resolves by its synthesized id")
        .value = 0.42;

    let mut add_fog = AddSceneFogCommand::new(
        GraphTarget::Generator(lid.clone()),
        vec![],
        0,
        (30.0, 40.0),
        vec![scene_param_meta("density", "Density")],
        mirror_catalog_default(),
    );
    add_fog.execute(&mut project);

    let intensity_value = project
        .timeline
        .find_layer_by_id(&lid)
        .unwrap()
        .1
        .gen_params()
        .unwrap()
        .params
        .get(&intensity_id)
        .expect("intensity param survives the fog add's refresh")
        .value;
    assert_eq!(
        intensity_value, 0.42,
        "BUG-295 refresh must preserve a pre-existing param's live value, not reset it to spec default"
    );
}

fn scene_object_graph() -> EffectGraphDef {
    let render = EffectGraphNode {
        id: 0,
        node_id: manifold_core::NodeId::new("render"),
        type_id: "node.render_scene".to_string(),
        handle: Some("render".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    };
    let object = EffectGraphNode {
        id: 1,
        node_id: manifold_core::NodeId::new("obj"),
        type_id: "node.scene_object".to_string(),
        handle: Some("Statue".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    };
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![render, object],
        wires: vec![EffectGraphWire {
            from_node: 1,
            from_port: "object".to_string(),
            to_node: 0,
            to_port: "object_0".to_string(),
        }],
    }
}

#[test]
fn add_object_transform_command_spawns_transform_3d_and_wires_it_into_scene_object() {
    let (mut project, fx) = project_with_graph(scene_object_graph());
    let before = graph_of(&project, &fx).clone();

    let mut cmd = AddObjectTransformCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        (5.0, 6.0),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);
    let xf_id = cmd
        .created_node_id()
        .expect("command should resolve and create a node");

    let def = graph_of(&project, &fx);
    let xf = def
        .nodes
        .iter()
        .find(|n| n.id == xf_id)
        .expect("transform node exists");
    assert_eq!(xf.type_id, "node.transform_3d");
    assert_eq!(xf.editor_pos, Some((5.0, 6.0)));
    assert!(def.wires.iter().any(|w| w.from_node == xf_id
        && w.from_port == "transform"
        && w.to_node == 1
        && w.to_port == "transform"));

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-add graph exactly (inverse-pair)"
    );
}

#[test]
fn add_object_transform_then_gizmo_param_drag_round_trips_undo_redo() {
    let (mut project, fx) = project_with_graph(scene_object_graph());
    let before = graph_of(&project, &fx).clone();

    let mut add_cmd = AddObjectTransformCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        (0.0, 0.0),
        mirror_catalog_default(),
    );
    add_cmd.execute(&mut project);
    let xf_id = add_cmd.created_node_id().unwrap();
    let after_create = graph_of(&project, &fx).clone();

    // The gizmo's first move-axis drag: write pos_x on the freshly
    // created transform atom (D8's drag-writes-the-transform-atom path).
    let mut set_cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(fx.clone()),
        xf_id,
        "pos_x".to_string(),
        SerializedParamValue::Float { value: 3.5 },
        mirror_catalog_default(),
    );
    set_cmd.execute(&mut project);
    let def = graph_of(&project, &fx);
    let xf = def.nodes.iter().find(|n| n.id == xf_id).unwrap();
    assert_eq!(
        xf.params.get("pos_x"),
        Some(&SerializedParamValue::Float { value: 3.5 })
    );

    // Undo the drag: pos_x reverts (the transform atom itself, and its
    // wire, stay — same as any other param undo).
    set_cmd.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &after_create,
        "undo of the drag restores pre-drag graph"
    );

    // Redo the drag.
    set_cmd.execute(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def.nodes
            .iter()
            .find(|n| n.id == xf_id)
            .unwrap()
            .params
            .get("pos_x"),
        Some(&SerializedParamValue::Float { value: 3.5 })
    );

    // Undo the drag AND the atom creation: back to the original,
    // transform-less graph — the full round trip P6's gate names.
    set_cmd.undo(&mut project);
    add_cmd.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &before,
        "full undo restores the original graph"
    );
}

/// A plain, un-grouped merged object node (mesh source + material +
/// transform, no group wrapper) — this test exercises the COMMAND, not
/// the assembler, so a minimal top-level node stands in for the
/// (grouped) shape `merge_import_into_graph` would actually produce.
fn plain_merge_node(id: u32, handle: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(handle),
        type_id: type_id.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

#[test]
fn import_model_into_scene_command_bumps_objects_adds_nodes_wires_and_undo_restores() {
    let (mut project, fx) = project_with_graph(render_scene_graph(2, 1));
    let before = graph_of(&project, &fx).clone();

    let new_node = plain_merge_node(100, "MergedObject", GROUP_TYPE_ID);
    let new_wire = EffectGraphWire {
        from_node: 100,
        from_port: "vertices".to_string(),
        to_node: 0,
        to_port: "mesh_2".to_string(),
    };

    let mut cmd = ImportModelIntoSceneCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        vec![new_node],
        vec![new_wire],
        3,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
    assert_eq!(
        render.params.get("objects"),
        Some(&SerializedParamValue::Float { value: 3.0 }),
        "objects bumped to existing(2) + incoming(1)"
    );
    assert!(
        def.nodes
            .iter()
            .any(|n| n.id == 100 && n.handle.as_deref() == Some("MergedObject")),
        "the merged node must be present"
    );
    assert!(
        def.wires
            .iter()
            .any(|w| w.from_node == 100 && w.to_node == 0 && w.to_port == "mesh_2"),
        "the merged wire must be present"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-merge graph exactly (inverse-pair)"
    );
}

#[test]
fn import_model_into_scene_command_extends_card_metadata_and_undo_restores() {
    let mut base = render_scene_graph(1, 0);
    base.preset_metadata = Some(PresetMetadata {
        id: manifold_core::PresetTypeId::from_string("Existing".to_string()),
        display_name: "Existing".to_string(),
        category: "Geometry".to_string(),
        osc_prefix: "existing".to_string(),
        legacy_discriminant: None,
        scene_modifier: None,
        scene_bounds: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: vec![],
        bindings: vec![],
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
    });
    let (mut project, fx) = project_with_graph(base);
    let before = graph_of(&project, &fx).clone();

    let new_param = ParamSpecDef {
        id: "opacity_1".to_string(),
        name: "Opacity".to_string(),
        min: 0.0,
        max: 1.0,
        default_value: 1.0,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: Some("MergedGlass".to_string()),
        card_visible: true,
    };
    let new_binding = BindingDef {
        id: "opacity_1".to_string(),
        label: "Opacity".to_string(),
        default_value: 1.0,
        target: manifold_core::effect_graph_def::BindingTarget::Node {
            node_id: manifold_core::NodeId::new("mat_1"),
            param: "color_a".to_string(),
        },
        convert: manifold_core::effects::ParamConvert::Float,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
        default_mirrors_node_param: false,
    };

    let mut cmd = ImportModelIntoSceneCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        0,
        vec![plain_merge_node(50, "MergedGlass", GROUP_TYPE_ID)],
        vec![EffectGraphWire {
            from_node: 50,
            from_port: "vertices".to_string(),
            to_node: 0,
            to_port: "mesh_1".to_string(),
        }],
        2,
        vec![new_param],
        vec![new_binding],
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let meta = def
        .preset_metadata
        .as_ref()
        .expect("metadata still present");
    assert!(
        meta.params.iter().any(|p| p.id == "opacity_1"),
        "new card param appended"
    );
    assert!(
        meta.bindings.iter().any(|b| b.id == "opacity_1"),
        "new card binding appended"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-merge graph AND metadata exactly"
    );
}

fn local_modifier_scene_project(mut local: EffectGraphDef) -> (Project, GraphTarget) {
    let (mut project, target, _) = modifier_draft_fixture();
    let owner_target = target.host_target().unwrap().clone();
    let host = project.graph_target_owner_mut(&owner_target).unwrap();
    let owner_graph = host.graph.as_mut().unwrap();
    local.preset_metadata = target
        .graph_in(owner_graph)
        .unwrap()
        .preset_metadata
        .clone();
    *target.graph_in_mut(owner_graph).unwrap() = local;
    (project, target)
}

#[test]
fn scene_modifier_structural_refresh_updates_host_manifest_and_roundtrips() {
    let (mut project, target) = local_modifier_scene_project(render_scene_graph(0, 0));
    let before_params = project.graph_target_owner(&target).unwrap().params.clone();
    let mut command = AddSceneObjectCommand::new(
        target.clone(),
        vec![],
        0,
        0,
        (0.0, 0.0),
        vec![scene_param_meta("ambient", "Ambient")],
        vec![scene_param_meta("pos_x", "X")],
        vec![scene_param_meta("visible", "Visible")],
        render_scene_graph(0, 0),
    );

    command.execute(&mut project);
    let host = project.graph_target_owner(&target).unwrap();
    let local = target.graph_in(host.graph.as_ref().unwrap()).unwrap();
    assert_eq!(local.preset_metadata.as_ref().unwrap().params.len(), 3);
    assert_eq!(
        host.params.len(),
        3,
        "local metadata refreshes the owning generator manifest"
    );

    command.undo(&mut project);
    let host = project.graph_target_owner(&target).unwrap();
    assert!(
        target
            .graph_in(host.graph.as_ref().unwrap())
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .is_empty()
    );
    assert_eq!(
        host.params, before_params,
        "undo restores host live values exactly"
    );

    command.execute(&mut project);
    let host = project.graph_target_owner(&target).unwrap();
    assert_eq!(
        host.params.len(),
        3,
        "redo refreshes the host manifest again"
    );
}

#[test]
fn scene_modifier_duplicate_copies_local_string_binding_and_preserves_sibling() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, target) = local_modifier_scene_project(render_scene_graph(0, 0));
    AddSceneObjectCommand::new(
        target.clone(),
        vec![],
        0,
        0,
        (0.0, 0.0),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        render_scene_graph(0, 0),
    )
    .execute(&mut project);
    let source_mesh = {
        let host = project.graph_target_owner(&target).unwrap();
        let local = target.graph_in(host.graph.as_ref().unwrap()).unwrap();
        local
            .nodes
            .iter()
            .find(|node| node.handle.as_deref() == Some("Object 1"))
            .and_then(|group| group.group.as_ref())
            .and_then(|group| {
                group
                    .nodes
                    .iter()
                    .find(|node| node.type_id == "node.cube_mesh")
            })
            .unwrap()
            .node_id
            .clone()
    };
    {
        let owner_target = target.host_target().unwrap().clone();
        let host = project.graph_target_owner_mut(&owner_target).unwrap();
        let local = target.graph_in_mut(host.graph.as_mut().unwrap()).unwrap();
        local
            .preset_metadata
            .as_mut()
            .unwrap()
            .string_bindings
            .push(StringBindingDef {
                id: "model_file".into(),
                label: "Model File".into(),
                default_value: "assets/hero.glb".into(),
                target: BindingTarget::Node {
                    node_id: source_mesh.clone(),
                    param: "path".into(),
                },
            });
    }
    let mut command = DuplicateSceneObjectCommand::new(
        target.clone(),
        vec![],
        0,
        0,
        render_scene_graph(0, 0),
    );
    command.execute(&mut project);
    let host = project.graph_target_owner(&target).unwrap();
    let local = target.graph_in(host.graph.as_ref().unwrap()).unwrap();
    let clone = local
        .nodes
        .iter()
        .find(|node| node.handle.as_deref() == Some("Object 1 2"))
        .unwrap();
    let clone_mesh = clone
        .group
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.type_id == "node.cube_mesh")
        .unwrap();
    let strings = &local.preset_metadata.as_ref().unwrap().string_bindings;
    assert_eq!(strings.len(), 2);
    assert!(strings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if *node_id == source_mesh)));
    assert!(strings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if *node_id == clone_mesh.node_id)));

    let sibling = GraphTarget::SceneModifier {
        owner: Box::new(target.host_target().unwrap().clone()),
        modifier_id: NodeId::new("b"),
    };
    let sibling_host = project.graph_target_owner(&sibling).unwrap();
    assert!(
        sibling
            .graph_in(sibling_host.graph.as_ref().unwrap())
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .string_bindings
            .is_empty()
    );

    command.undo(&mut project);
    let host = project.graph_target_owner(&target).unwrap();
    let local = target.graph_in(host.graph.as_ref().unwrap()).unwrap();
    let strings = &local.preset_metadata.as_ref().unwrap().string_bindings;
    assert_eq!(strings.len(), 1);
    assert!(
        matches!(&strings[0].target, BindingTarget::Node { node_id, .. } if *node_id == source_mesh)
    );
}
