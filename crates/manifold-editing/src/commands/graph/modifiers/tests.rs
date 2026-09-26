use super::super::modifiers::MESH_MODIFIER_TYPE_IDS;
use super::super::test_support::*;
use super::super::*;
use crate::command::Command;
use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
use manifold_core::effect_graph_def::{
    GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef,
};

fn plain_node(id: u32, handle: &str, type_id: &str) -> EffectGraphNode {
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

/// A one-object scene: `render_scene` (id 0, `objects=1`) wired to a
/// named group (id 1) containing a mesh source (id 10) → the given
/// modifier chain (ids 11, 12, … in wire order) → `node.scene_object`
/// (id 90) → `system.group_output` (id 99, re-exporting `object` only)
/// — the real D12 `AddSceneObjectCommand`/importer shape (see
/// `AddSceneObjectCommand::execute`), close enough to exercise the
/// splice commands against a realistic nested-group body. BUG-218: this
/// fixture used to construct the pre-D12 shape (group_output's own
/// `vertices` port re-exported directly, no scene_object at all) — that
/// shape never reproduced the bug the commands actually hit against
/// real objects, so it's replaced wholesale rather than kept alongside.
fn object_group_scene(modifier_type_ids: &[&str]) -> EffectGraphDef {
    let mesh = plain_node(10, "mesh", "node.cube_mesh");
    let mut body_nodes = vec![mesh];
    let mut body_wires = Vec::new();
    let mut prev = (10u32, "vertices".to_string());
    for (i, type_id) in modifier_type_ids.iter().enumerate() {
        let id = 11 + i as u32;
        body_nodes.push(plain_node(id, &format!("mod{i}"), type_id));
        body_wires.push(scene_build_wire(prev.0, &prev.1, id, "in"));
        prev = (id, "out".to_string());
    }
    let scene_object_id = 90;
    let scene_object = plain_node(scene_object_id, "Hero", "node.scene_object");
    body_wires.push(scene_build_wire(
        prev.0,
        &prev.1,
        scene_object_id,
        "vertices",
    ));
    body_nodes.push(scene_object);

    let mut out_node = plain_node(99, "out", GROUP_OUTPUT_TYPE_ID);
    out_node.handle = None;
    body_wires.push(scene_build_wire(scene_object_id, "object", 99, "object"));
    body_nodes.push(out_node);

    let mut group_node = plain_node(1, "Hero", GROUP_TYPE_ID);
    group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef {
                name: "object".to_string(),
                port_type: "Object".to_string(),
            }],
            params: Vec::new(),
        },
        nodes: body_nodes,
        wires: body_wires,
        tint: None,
    }));

    let mut render = plain_node(0, "render", "node.render_scene");
    render.params.insert(
        "objects".to_string(),
        SerializedParamValue::Float { value: 1.0 },
    );

    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![group_node, render],
        wires: vec![scene_build_wire(1, "object", 0, "object_0")],
    }
}

/// A one-object scene in the OTHER legitimate D12-era shape —
/// `migrate_scene_object_wires`'s output (e.g. the bundled
/// `SceneStarter.json`): the mesh-producer group (id 1) contains ONLY
/// mesh (id 10) → modifiers (ids 11, 12, …) → `system.group_output` (id
/// 99, re-exporting `vertices` DIRECTLY — the pre-D12 shape). The minted
/// `node.scene_object` (id 90) stays a ROOT-LEVEL SIBLING of the group,
/// wired `group.vertices -> scene_object.vertices` (the migration's
/// "same-scope re-point" — see `scene_object_migration.rs`'s
/// `migrate_scope`, which only ever retargets the wire's `to_node`/
/// `to_port`, never touches the group's own body). BUG-218/escape: the
/// group being edited here has NO scene_object inside it and NO
/// `object` port at all — `walk_mesh_modifier_chain` must fall through
/// to walking the group output's own `vertices` port, matching the
/// pre-D12 behavior this shape still relies on.
fn migrated_object_group_scene(modifier_type_ids: &[&str]) -> EffectGraphDef {
    let mesh = plain_node(10, "mesh", "node.cube_mesh");
    let mut body_nodes = vec![mesh];
    let mut body_wires = Vec::new();
    let mut prev = (10u32, "vertices".to_string());
    for (i, type_id) in modifier_type_ids.iter().enumerate() {
        let id = 11 + i as u32;
        body_nodes.push(plain_node(id, &format!("mod{i}"), type_id));
        body_wires.push(scene_build_wire(prev.0, &prev.1, id, "in"));
        prev = (id, "out".to_string());
    }
    let mut out_node = plain_node(99, "out", GROUP_OUTPUT_TYPE_ID);
    out_node.handle = None;
    body_wires.push(scene_build_wire(prev.0, &prev.1, 99, "vertices"));
    body_nodes.push(out_node);

    let mut group_node = plain_node(1, "Hero", GROUP_TYPE_ID);
    group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef {
                name: "vertices".to_string(),
                port_type: "Array(Vertex)".to_string(),
            }],
            params: Vec::new(),
        },
        nodes: body_nodes,
        wires: body_wires,
        tint: None,
    }));

    let scene_object = plain_node(90, "Hero", "node.scene_object");
    let mut render = plain_node(0, "render", "node.render_scene");
    render.params.insert(
        "objects".to_string(),
        SerializedParamValue::Float { value: 1.0 },
    );

    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![group_node, scene_object, render],
        wires: vec![
            scene_build_wire(1, "vertices", 90, "vertices"),
            scene_build_wire(90, "object", 0, "object_0"),
        ],
    }
}

/// A legacy bare object: the scene object owns the mesh chain directly in the
/// current scope, with no object group wrapper. This is still a valid scene
/// card shape and is addressed by passing the scene object's node id as the
/// command owner.
fn bare_object_scene(modifier_type_ids: &[&str]) -> EffectGraphDef {
    let mesh = plain_node(10, "mesh", "node.cube_mesh");
    let scene_object_id = 90;
    let scene_object = plain_node(scene_object_id, "Hero", "node.scene_object");
    let mut nodes = vec![mesh];
    let mut wires = Vec::new();
    let mut prev = (10u32, "vertices".to_string());
    for (i, type_id) in modifier_type_ids.iter().enumerate() {
        let id = 11 + i as u32;
        nodes.push(plain_node(id, &format!("mod{i}"), type_id));
        wires.push(scene_build_wire(prev.0, &prev.1, id, "in"));
        prev = (id, "out".to_string());
    }
    wires.push(scene_build_wire(prev.0, &prev.1, scene_object_id, "vertices"));
    nodes.push(scene_object);

    let mut render = plain_node(0, "render", "node.render_scene");
    render.params.insert(
        "objects".to_string(),
        SerializedParamValue::Float { value: 1.0 },
    );
    nodes.push(render);
    wires.push(scene_build_wire(scene_object_id, "object", 0, "object_0"));

    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes,
        wires,
    }
}

fn grouped_and_bare_object_scene() -> EffectGraphDef {
    let mut def = object_group_scene(&[]);
    let mesh_id = 110;
    let scene_object_id = 190;
    def.nodes.push(plain_node(mesh_id, "mesh2", "node.cube_mesh"));
    def.nodes.push(plain_node(
        scene_object_id,
        "Bare",
        "node.scene_object",
    ));
    def.wires
        .push(scene_build_wire(mesh_id, "vertices", scene_object_id, "vertices"));
    def.wires
        .push(scene_build_wire(scene_object_id, "object", 0, "object_1"));
    if let Some(render) = def.nodes.iter_mut().find(|node| node.id == 0) {
        render.params.insert(
            "objects".to_string(),
            SerializedParamValue::Float { value: 2.0 },
        );
    }
    def
}

/// Wrap `def`'s whole top level inside a fresh outer group at `outer_id`
/// — the "nested-group placement" gate: the object's group (id 1) now
/// lives at `scope_path = [outer_id]` instead of root, so
/// `full_modifier_scope` must compose two hops, not one.
fn wrap_in_outer_group(def: EffectGraphDef, outer_id: u32) -> EffectGraphDef {
    let mut outer = plain_node(outer_id, "Outer", GROUP_TYPE_ID);
    outer.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Vec::new(),
        },
        nodes: def.nodes,
        wires: def.wires,
        tint: None,
    }));
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![outer],
        wires: Vec::new(),
    }
}

/// Read the modifier stack's node ids back off `def`, in wire order —
/// the "Vm chain-trace tests: stack order matches wire order" gate,
/// re-derived independently of `scene_vm.rs` (this crate can't depend on
/// it) by walking the SAME chain shape production code walks. BUG-218/
/// escape: mirrors `walk_mesh_modifier_chain`'s dual-shape resolution —
/// if the group output's `object` port resolves to a scene_object,
/// anchor on ITS `vertices` input (import shape); otherwise anchor on
/// the group output's own `vertices` port directly (migrated/starter
/// shape, `scene_vm.rs:617-618`).
fn modifier_ids_in_wire_order(def: &EffectGraphDef, scope: &[u32]) -> Vec<u32> {
    let mut nodes: &[EffectGraphNode] = &def.nodes;
    let mut wires: &[EffectGraphWire] = &def.wires;
    for gid in scope {
        let group = nodes.iter().find(|n| n.id == *gid).unwrap();
        let body = group.group.as_deref().unwrap();
        nodes = &body.nodes;
        wires = &body.wires;
    }
    let out_id = nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
        .unwrap()
        .id;
    let scene_object_id = wires
        .iter()
        .find(|w| w.to_node == out_id && w.to_port == "object")
        .map(|w| w.from_node);
    let anchor = scene_object_id.unwrap_or(out_id);
    let mut chain = Vec::new();
    let mut cursor = wires
        .iter()
        .find(|w| w.to_node == anchor && w.to_port == "vertices")
        .map(|w| (w.from_node, w.from_port.clone()));
    while let Some((node_id, _)) = cursor {
        let node = nodes.iter().find(|n| n.id == node_id).unwrap();
        if !MESH_MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
            break;
        }
        chain.push(node_id);
        cursor = wires
            .iter()
            .find(|w| w.to_node == node_id && w.to_port == "in")
            .map(|w| (w.from_node, w.from_port.clone()));
    }
    chain.reverse();
    chain
}

#[test]
fn insert_modifier_appends_to_empty_stack_and_undo_restores() {
    let (mut project, fx) = project_with_graph(object_group_scene(&[]));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(ids.len(), 1);
    let inserted = def
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .unwrap()
        .group
        .as_deref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == ids[0])
        .unwrap();
    assert_eq!(inserted.type_id, "node.bend_mesh");

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-insert graph exactly (inverse-pair)"
    );
}

/// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `InsertMeshModifierCommand`
/// stamps the caller-supplied modifier metadata into the def's TOP-LEVEL
/// `preset_metadata`, targeting the inserted node's bare `NodeId`,
/// section named `"{object group name} — {modifier label}"` (mirrors the
/// glTF importer's modifier section convention, duplicated in
/// `modifier_section_label` since this crate has no renderer dep). Undo
/// restores `preset_metadata` verbatim; execute→undo→redo is
/// structurally stable, including the inserted node's stable identity.
#[test]
fn insert_modifier_stamps_exposures_and_undo_redo_are_stable() {
    use manifold_core::effect_graph_def::BindingTarget;

    let (mut project, fx) = project_with_graph(object_group_scene(&[]));

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        vec![scene_param_meta("amount", "Amount")],
        mirror_catalog_default(),
    );

    let assert_stamped = |project: &Project| {
        let def = graph_of(project, &fx);
        let ids = modifier_ids_in_wire_order(def, &[1]);
        assert_eq!(ids.len(), 1);
        let inserted = def
            .nodes
            .iter()
            .find(|n| n.id == 1)
            .unwrap()
            .group
            .as_deref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.id == ids[0])
            .unwrap();
        assert_eq!(inserted.type_id, "node.bend_mesh");

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("P1 stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(
            meta.params[0].section.as_deref(),
            Some("Hero — Bend_mesh"),
            "section = '{{object group name}} — {{modifier label}}' (the fixture's group is named 'Hero')"
        );
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, param } if *node_id == inserted.node_id && param == "amount"
            )),
            "modifier exposure targets the inserted node's bare NodeId"
        );
    };

    cmd.execute(&mut project);
    assert_stamped(&project);
    let after_first_execute = graph_of(&project, &fx).clone();

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert!(
        def.preset_metadata.is_none(),
        "undo restores the pre-insert (empty) preset_metadata verbatim"
    );

    cmd.execute(&mut project); // redo
    assert_stamped(&project);
    assert_eq!(graph_of(&project, &fx), &after_first_execute);
}

#[test]
fn insert_undo_restores_an_original_none_graph() {
    let catalog = object_group_scene(&[]);
    let (mut project, fx) = project_with_graph(catalog.clone());
    project.graph_target_owner_mut(&GraphTarget::Effect(fx.clone())).unwrap().graph = None;
    let target = GraphTarget::Effect(fx.clone());
    let mut cmd = InsertMeshModifierCommand::new(
        target.clone(),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        catalog,
    );
    cmd.execute(&mut project);
    assert!(project.graph_target_owner(&target).unwrap().graph.is_some());
    cmd.undo(&mut project);
    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
}

/// BUG-218 escape: the migrated/starter shape (`migrate_scene_object_wires`
/// — the scene_object lives OUTSIDE this group, the group only exports
/// `vertices`) must still splice via the group output's `vertices` port,
/// not the import shape's scene_object-input anchor. Regression gate for
/// the escape found landing this fix: the earlier version of this fix
/// only handled the import shape and silently broke this one.
#[test]
fn insert_modifier_on_migrated_shape_splices_at_group_output_and_undo_restores() {
    let (mut project, fx) = project_with_graph(migrated_object_group_scene(&[]));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(
        ids.len(),
        1,
        "the migrated shape's group output still gains the modifier"
    );
    let inserted = def
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .unwrap()
        .group
        .as_deref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == ids[0])
        .unwrap();
    assert_eq!(inserted.type_id, "node.bend_mesh");
    // The root-level scene_object (id 90) is untouched — its own
    // `vertices` wire still comes straight from the group's boundary.
    assert!(
        def.wires.iter().any(|w| w.from_node == 1
            && w.from_port == "vertices"
            && w.to_node == 90
            && w.to_port == "vertices"),
        "scene_object still wired directly from the group's vertices boundary"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-insert graph exactly (inverse-pair), migrated shape"
    );
}

/// Companion to the insert gate above: Remove/Move on the migrated shape
/// also splice at the group output, not a (nonexistent, in this shape)
/// scene_object input.
#[test]
fn remove_and_move_modifier_on_migrated_shape_splice_at_group_output_and_undo_restores() {
    let (mut project, fx) = project_with_graph(migrated_object_group_scene(&[
        "node.bend_mesh",
        "node.twist_mesh",
        "node.taper_mesh",
    ]));
    let before = graph_of(&project, &fx).clone();
    let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist, taper]

    let mut remove_cmd = RemoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[1], // remove the middle (twist)
        mirror_catalog_default(),
    );
    remove_cmd.execute(&mut project);
    let after_remove = graph_of(&project, &fx);
    assert_eq!(
        modifier_ids_in_wire_order(after_remove, &[1]),
        vec![ids0[0], ids0[2]]
    );
    remove_cmd.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &before,
        "undo restores after removing the middle, migrated shape"
    );

    let mut move_cmd = MoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[2], // move taper (last) to the front
        0,
        mirror_catalog_default(),
    );
    move_cmd.execute(&mut project);
    let after_move = graph_of(&project, &fx);
    assert_eq!(
        modifier_ids_in_wire_order(after_move, &[1]),
        vec![ids0[2], ids0[0], ids0[1]],
        "taper moved to the front, migrated shape"
    );
    move_cmd.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &before,
        "undo restores after the move, migrated shape"
    );
}

#[test]
fn insert_modifier_at_position_zero_lands_just_after_mesh_source() {
    let (mut project, fx) = project_with_graph(object_group_scene(&["node.twist_mesh"]));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        Some(0),
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(ids.len(), 2, "one existing + one inserted");
    let group = def
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .unwrap()
        .group
        .as_deref()
        .unwrap();
    let first = group.nodes.iter().find(|n| n.id == ids[0]).unwrap();
    let second = group.nodes.iter().find(|n| n.id == ids[1]).unwrap();
    assert_eq!(
        first.type_id, "node.bend_mesh",
        "position 0 = just after the mesh source"
    );
    assert_eq!(
        second.type_id, "node.twist_mesh",
        "the pre-existing modifier now sits second"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-insert graph exactly (inverse-pair)"
    );
}

#[test]
fn insert_modifier_default_position_appends_at_the_end() {
    let (mut project, fx) =
        project_with_graph(object_group_scene(&["node.twist_mesh", "node.taper_mesh"]));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(ids.len(), 3);
    let group = def
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .unwrap()
        .group
        .as_deref()
        .unwrap();
    let last = group.nodes.iter().find(|n| n.id == ids[2]).unwrap();
    assert_eq!(
        last.type_id, "node.bend_mesh",
        "no position = end of stack, just before the group output"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-insert graph exactly (inverse-pair)"
    );
}

#[test]
fn insert_modifier_in_nested_group_composes_scope_path() {
    // The object's own group (id 1) now lives at scope_path [50] instead
    // of root — proves `full_modifier_scope` composes two hops.
    let (mut project, fx) =
        project_with_graph(wrap_in_outer_group(object_group_scene(&[]), 50));
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![50],
        1,
        "node.rotate_3d".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[50, 1]);
    assert_eq!(ids.len(), 1);

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-insert graph exactly (inverse-pair), nested case"
    );
}

#[test]
fn insert_modifier_refuses_on_unparseable_chain() {
    // A group whose `vertices` boundary is unwired entirely — the
    // command must refuse (no partial/corrupt mutation), matching the
    // Vm's own `modifier_chain_parseable = false` case.
    let mut def = object_group_scene(&[]);
    {
        let group = def.nodes.iter_mut().find(|n| n.id == 1).unwrap();
        group.group.as_mut().unwrap().wires.clear(); // vertices now unwired
    }
    let (mut project, fx) = project_with_graph(def);
    let before = graph_of(&project, &fx).clone();

    let mut cmd = InsertMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "an unparseable chain is refused — no node pushed, no wires touched"
    );
    assert!(!cmd.was_applied());
    assert!(cmd.rejection_reason().is_some());
}

#[test]
fn bare_object_modifier_commands_use_scene_object_vertices_and_restore() {
    let (mut project, fx) = project_with_graph(bare_object_scene(&[
        "node.bend_mesh",
        "node.twist_mesh",
    ]));
    let target = GraphTarget::Effect(fx.clone());
    let before = graph_of(&project, &fx).clone();
    let mut insert = InsertMeshModifierCommand::new(
        target.clone(),
        vec![],
        90,
        "node.taper_mesh".to_string(),
        Some(1),
        Vec::new(),
        mirror_catalog_default(),
    );
    let mut admitted = Vec::new();
    insert.graph_admission_targets(&mut admitted);
    assert_eq!(admitted, vec![target.clone()]);
    insert.execute(&mut project);
    assert!(insert.was_applied());
    let def = graph_of(&project, &fx);
    let scene_object = def.nodes.iter().find(|node| node.id == 90).unwrap();
    let inserted_id = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.taper_mesh")
        .unwrap()
        .id;
    assert_eq!(scene_object.type_id, "node.scene_object");
    assert!(def.wires.iter().any(|wire| {
        wire.to_node == inserted_id && wire.to_port == "in"
    }));
    assert!(def.wires.iter().any(|wire| {
        wire.from_node == inserted_id && wire.from_port == "out"
    }));

    let mut remove = RemoveMeshModifierCommand::new(
        target.clone(),
        vec![],
        90,
        inserted_id,
        mirror_catalog_default(),
    );
    remove.execute(&mut project);
    assert!(remove.was_applied());
    assert!(!graph_of(&project, &fx).nodes.iter().any(|node| node.id == inserted_id));
    remove.undo(&mut project);
    assert!(graph_of(&project, &fx).nodes.iter().any(|node| node.id == inserted_id));

    insert.undo(&mut project);
    assert_eq!(graph_of(&project, &fx), &before);
}

#[test]
fn inserted_modifier_ids_are_unique_across_grouped_and_bare_objects() {
    let (mut project, fx) = project_with_graph(grouped_and_bare_object_scene());
    let target = GraphTarget::Effect(fx.clone());
    let mut grouped = InsertMeshModifierCommand::new(
        target.clone(),
        vec![],
        1,
        "node.bend_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    grouped.execute(&mut project);
    let grouped_id = graph_of(&project, &fx)
        .nodes
        .iter()
        .find(|node| node.type_id == GROUP_TYPE_ID)
        .unwrap()
        .group
        .as_deref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.type_id == "node.bend_mesh")
        .unwrap()
        .id;

    let mut bare = InsertMeshModifierCommand::new(
        target,
        vec![],
        190,
        "node.twist_mesh".to_string(),
        None,
        Vec::new(),
        mirror_catalog_default(),
    );
    bare.execute(&mut project);
    let def = graph_of(&project, &fx);
    let bare_id = def
        .nodes
        .iter()
        .find(|node| node.type_id == "node.twist_mesh")
        .unwrap()
        .id;
    assert_ne!(grouped_id, bare_id);
    assert!(bare_id > grouped_id);
}

#[test]
fn remove_modifier_prunes_exposure_side_wire_and_automation_atomically() {
    let (mut project, fx) = project_with_graph(bare_object_scene(&[]));
    let target = GraphTarget::Effect(fx.clone());
    let mut insert = InsertMeshModifierCommand::new(
        target.clone(),
        vec![],
        90,
        "node.bend_mesh".to_string(),
        None,
        vec![scene_param_meta("amount", "Amount")],
        mirror_catalog_default(),
    );
    insert.execute(&mut project);
    let inserted = graph_of(&project, &fx)
        .nodes
        .iter()
        .find(|node| node.type_id == "node.bend_mesh")
        .unwrap()
        .clone();
    let param_id = format!("{}_amount", inserted.id);

    {
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.params = manifold_core::params::ParamManifest::from_params(vec![
            slot(&param_id, 0.25, true),
        ]);
        owner.automation_lanes = Some(vec![manifold_core::effects::AutomationLane {
            param_id: param_id.clone().into(),
            enabled: true,
            points: Vec::new(),
        }]);
        let graph = owner.graph.as_mut().unwrap();
        graph.nodes.push(plain_node(300, "side", "node.value"));
        graph.wires.push(scene_build_wire(300, "value", inserted.id, "aux"));
    }
    let before_remove = graph_of(&project, &fx).clone();
    let before_params = project.graph_target_owner(&target).unwrap().params.clone();

    let mut remove = RemoveMeshModifierCommand::new(
        target.clone(),
        vec![],
        90,
        inserted.id,
        mirror_catalog_default(),
    );
    remove.execute(&mut project);
    let owner = project.graph_target_owner(&target).unwrap();
    let graph = owner.graph.as_ref().unwrap();
    assert!(!graph.nodes.iter().any(|node| node.id == inserted.id));
    assert!(graph
        .wires
        .iter()
        .all(|wire| wire.from_node != inserted.id && wire.to_node != inserted.id));
    assert!(graph
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .all(|binding| !matches!(
            &binding.target,
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. }
                if node_id == &inserted.node_id
        )));
    assert!(!owner.params.contains(&param_id));
    assert!(owner.automation_lanes.is_none());

    remove.undo(&mut project);
    let owner = project.graph_target_owner(&target).unwrap();
    assert_eq!(owner.graph.as_ref().unwrap(), &before_remove);
    assert_eq!(owner.params, before_params);
    let lanes = owner.automation_lanes.as_ref().unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].param_id.as_ref(), param_id);
}

#[test]
fn remove_modifier_middle_of_stack_rejoins_wire_and_undo_restores() {
    let (mut project, fx) = project_with_graph(object_group_scene(&[
        "node.bend_mesh",
        "node.twist_mesh",
        "node.taper_mesh",
    ]));
    let before = graph_of(&project, &fx).clone();
    let middle_id = modifier_ids_in_wire_order(&before, &[1])[1];

    let mut cmd = RemoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        middle_id,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(ids.len(), 2, "the middle modifier is gone");
    assert!(!ids.contains(&middle_id));
    let group = def
        .nodes
        .iter()
        .find(|n| n.id == 1)
        .unwrap()
        .group
        .as_deref()
        .unwrap();
    assert!(
        !group.nodes.iter().any(|n| n.id == middle_id),
        "the node itself is deleted"
    );
    assert_eq!(
        group.nodes.iter().find(|n| n.id == ids[0]).unwrap().type_id,
        "node.bend_mesh"
    );
    assert_eq!(
        group.nodes.iter().find(|n| n.id == ids[1]).unwrap().type_id,
        "node.taper_mesh",
        "bend now feeds taper directly — the wire rejoined around the removed node"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-remove graph exactly (inverse-pair)"
    );
}

#[test]
fn remove_modifier_at_first_and_last_positions_and_undo_restores() {
    let (mut project, fx) =
        project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh"]));
    let before = graph_of(&project, &fx).clone();
    let ids0 = modifier_ids_in_wire_order(&before, &[1]);

    // Remove the FIRST modifier — mesh source must now feed the second directly.
    let mut cmd_first = RemoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[0],
        mirror_catalog_default(),
    );
    cmd_first.execute(&mut project);
    let after_first = graph_of(&project, &fx);
    assert_eq!(modifier_ids_in_wire_order(after_first, &[1]), vec![ids0[1]]);
    cmd_first.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &before,
        "undo restores after removing the first"
    );

    // Remove the LAST modifier — its predecessor must now feed group_output directly.
    let mut cmd_last = RemoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[1],
        mirror_catalog_default(),
    );
    cmd_last.execute(&mut project);
    let after_last = graph_of(&project, &fx);
    assert_eq!(modifier_ids_in_wire_order(after_last, &[1]), vec![ids0[0]]);
    cmd_last.undo(&mut project);
    assert_eq!(
        graph_of(&project, &fx),
        &before,
        "undo restores after removing the last"
    );
}

#[test]
fn move_modifier_reorders_stack_and_undo_restores() {
    let (mut project, fx) = project_with_graph(object_group_scene(&[
        "node.bend_mesh",
        "node.twist_mesh",
        "node.taper_mesh",
    ]));
    let before = graph_of(&project, &fx).clone();
    let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist, taper]

    // Move taper (currently last) to position 0 — just after the mesh source.
    let mut cmd = MoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[2],
        0,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    let ids1 = modifier_ids_in_wire_order(def, &[1]);
    assert_eq!(
        ids1,
        vec![ids0[2], ids0[0], ids0[1]],
        "taper moved to the front, bend/twist shift down"
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-move graph exactly (inverse-pair)"
    );
}

#[test]
fn move_modifier_to_the_end_and_undo_restores() {
    let (mut project, fx) =
        project_with_graph(object_group_scene(&["node.bend_mesh", "node.twist_mesh"]));
    let before = graph_of(&project, &fx).clone();
    let ids0 = modifier_ids_in_wire_order(&before, &[1]); // [bend, twist]

    // Move bend (currently first) to the end.
    let mut cmd = MoveMeshModifierCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        1,
        ids0[0],
        1, // one slot remains (twist) once bend is detached — "end" is position 1
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = graph_of(&project, &fx);
    assert_eq!(
        modifier_ids_in_wire_order(def, &[1]),
        vec![ids0[1], ids0[0]]
    );

    cmd.undo(&mut project);
    let def = graph_of(&project, &fx);
    assert_eq!(
        def, &before,
        "undo restores the pre-move graph exactly (inverse-pair)"
    );
}
