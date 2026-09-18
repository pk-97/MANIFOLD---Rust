use super::super::test_support::*;
use super::super::*;
use crate::command::Command;
use manifold_core::EffectId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef,
};
use manifold_core::effects::PresetInstance;

/// A batch layout sets every listed node's `editor_pos` in one command,
/// and undo restores them all — including the never-positioned `None`.
#[test]
fn layout_graph_nodes_sets_and_undoes_positions() {
    let (mut project, fx) = project_with_graph(abc_graph());
    let pos_of = |project: &Project, id: u32| {
        graph_of(project, &fx)
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap()
            .editor_pos
    };
    assert_eq!(pos_of(&project, 0), None);
    assert_eq!(pos_of(&project, 2), None);

    let mut cmd = LayoutGraphNodesCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![(0, (10.0, 20.0)), (2, (30.0, 40.0))],
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);
    assert_eq!(pos_of(&project, 0), Some((10.0, 20.0)));
    assert_eq!(pos_of(&project, 2), Some((30.0, 40.0)));

    cmd.undo(&mut project);
    assert_eq!(pos_of(&project, 0), None, "undo restored node 0");
    assert_eq!(pos_of(&project, 2), None, "undo restored node 2");
}

/// A scoped Add drops the new node into the group body, not the root, and
/// undo removes it from the body.
#[test]
fn scoped_add_node_lands_in_group_body() {
    let (mut project, fx) = project_with_graph(abc_graph());
    let mut group = GroupNodesCommand::new(
        GraphTarget::Effect(fx.clone()),
        vec![],
        vec![1],
        "g".to_string(),
        (0.0, 0.0),
        mirror_catalog_default(),
    );
    group.execute(&mut project);
    let g_id = graph_of(&project, &fx)
        .nodes
        .iter()
        .find(|n| n.handle.as_deref() == Some("g"))
        .unwrap()
        .id;

    let mut add = AddGraphNodeCommand::new(
        GraphTarget::Effect(fx.clone()),
        "node.transform".to_string(),
        Some((1.0, 2.0)),
        mirror_catalog_default(),
    )
    .with_scope(vec![g_id]);
    add.execute(&mut project);
    let new_id = add.new_node_id().expect("node added");

    let def = graph_of(&project, &fx);
    let body = def
        .nodes
        .iter()
        .find(|n| n.id == g_id)
        .unwrap()
        .group
        .as_deref()
        .unwrap();
    assert!(
        body.nodes.iter().any(|n| n.id == new_id),
        "new node added to the group body"
    );
    assert!(
        !def.nodes.iter().any(|n| n.id == new_id),
        "new node not added at root"
    );

    add.undo(&mut project);
    let def = graph_of(&project, &fx);
    let body = def
        .nodes
        .iter()
        .find(|n| n.id == g_id)
        .unwrap()
        .group
        .as_deref()
        .unwrap();
    assert!(
        !body.nodes.iter().any(|n| n.id == new_id),
        "undo removed the node from the body"
    );
}

/// Project with one master Mirror effect, graph: None.
fn project_with_one_master_effect() -> (Project, EffectId) {
    let mut project = Project::default();
    let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
    let id = fx.id.clone();
    project.settings.master_effects.push(fx);
    (project, id)
}

#[test]
fn add_graph_node_lifts_from_none_and_appends_node() {
    let (mut project, id) = project_with_one_master_effect();
    let mut cmd = AddGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        "node.blur".to_string(),
        Some((50.0, 60.0)),
        mirror_catalog_default(),
    );

    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(fx.graph.is_some(), "lift should populate graph");
    let def = fx.graph.as_ref().unwrap();
    // Catalog default (4 nodes) + the new Blur = 5.
    assert_eq!(def.nodes.len(), 5);
    let new_id = cmd.new_node_id().expect("id minted");
    let new_node = def.nodes.iter().find(|n| n.id == new_id).unwrap();
    assert_eq!(new_node.type_id, "node.blur");
    assert_eq!(new_node.editor_pos, Some((50.0, 60.0)));
    assert_eq!(fx.graph_version, 1);
}

#[test]
fn add_graph_node_undo_removes_node() {
    let (mut project, id) = project_with_one_master_effect();
    let mut cmd = AddGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        "node.blur".to_string(),
        None,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);
    cmd.undo(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    // Graph is still Some after undo (no un-lift), but with
    // catalog-default contents.
    let def = fx.graph.as_ref().unwrap();
    assert_eq!(def.nodes.len(), 4);
    assert_eq!(fx.graph_version, 2); // bumped twice (execute + undo)
}

#[test]
fn remove_graph_node_also_removes_incident_wires() {
    let (mut project, id) = project_with_one_master_effect();
    // Pre-populate graph with the catalog default.
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = RemoveGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    assert_eq!(def.nodes.len(), 3);
    // Wires touching node 1 (src→uv, uv→mix.b) are gone.
    assert!(def.wires.iter().all(|w| w.from_node != 1 && w.to_node != 1));
    // The src→mix.a wire is intact.
    assert!(
        def.wires
            .iter()
            .any(|w| w.from_node == 0 && w.to_port == "a")
    );
}

#[test]
fn remove_graph_node_undo_restores_node_and_wires() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = RemoveGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);
    cmd.undo(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    assert_eq!(def.nodes.len(), 4);
    assert_eq!(def.wires.len(), 4);
}

#[test]
fn remove_graph_node_prunes_bound_card_slider_and_undo_restores() {
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
    };

    let (mut project, id) = project_with_one_master_effect();
    // Diverged graph carrying a card slider bound to node 1 (uv_transform).
    let mut def = mirror_catalog_default();
    def.preset_metadata = Some(PresetMetadata {
        id: PresetTypeId::new("Mirror"),
        display_name: "Mirror".into(),
        category: String::new(),
        osc_prefix: String::new(),
        legacy_discriminant: None,
        scene_modifier: None,
        scene_bounds: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: vec![ParamSpecDef {
            id: "amount".into(),
            name: "Amount".into(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        }],
        bindings: vec![BindingDef {
            id: "amount".into(),
            label: "Amount".into(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: NodeId::new("uv_transform"),
                param: "scale".into(),
            },
            convert: Default::default(),
            user_added: true,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        }],
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
    });
    {
        let fx = project.find_effect_by_id_mut(&id).unwrap();
        fx.graph = Some(def);
        fx.params =
            manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
    }

    let mut cmd = RemoveGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(
        fx.graph.as_ref().unwrap().nodes.iter().all(|n| n.id != 1),
        "node deleted"
    );
    let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
    assert!(meta.bindings.is_empty(), "bound slider's binding pruned");
    assert!(meta.params.is_empty(), "bound slider's param spec pruned");
    assert!(fx.params.is_empty(), "its value slot pruned");

    cmd.undo(&mut project);
    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(
        fx.graph.as_ref().unwrap().nodes.iter().any(|n| n.id == 1),
        "node restored"
    );
    let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
    assert_eq!(meta.bindings.len(), 1, "binding restored");
    assert_eq!(meta.params.len(), 1, "param spec restored");
    assert_eq!(fx.params.len(), 1, "value slot restored");
    let restored = fx.params.get("amount").unwrap();
    assert_eq!(restored.value, 0.5);
    assert_eq!(restored.base, 0.5);
    assert!(restored.exposed);
}

/// BUG-154: deleting a GROUP node that contains a node bound to a card
/// slider used to leave the slider dangling — `remove_exposures_for_node`
/// only ever matched the group container's own id, never the id of a
/// node NESTED inside the removed group's subgraph. `subtree_node_ids`
/// closes that: it walks the removed node's group tree and prunes an
/// exposure bound to a node at ANY depth inside it.
#[test]
fn remove_group_node_prunes_card_slider_bound_to_a_nested_node() {
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
    };

    let (mut project, id) = project_with_one_master_effect();
    let mut def = mirror_catalog_default();
    // Rewrap node 1 ("uv_transform") as a group whose sole child carries
    // the SAME node_id — the slider stays bound to the nested node, not
    // the group container.
    let inner = def.nodes[1].clone();
    let group_node = EffectGraphNode {
        id: 1,
        node_id: NodeId::new("the_group"),
        type_id: GROUP_TYPE_ID.to_string(),
        handle: Some("the_group".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![InterfacePortDef {
                    name: "source".to_string(),
                    port_type: String::new(),
                }],
                outputs: vec![InterfacePortDef {
                    name: "out".to_string(),
                    port_type: String::new(),
                }],
                params: vec![],
            },
            nodes: vec![inner],
            wires: vec![],
            tint: None,
        })),
    };
    def.nodes[1] = group_node;
    def.preset_metadata = Some(PresetMetadata {
        id: PresetTypeId::new("Mirror"),
        display_name: "Mirror".into(),
        category: String::new(),
        osc_prefix: String::new(),
        legacy_discriminant: None,
        scene_modifier: None,
        scene_bounds: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: vec![ParamSpecDef {
            id: "amount".into(),
            name: "Amount".into(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        }],
        // Bound to the NESTED node's id ("uv_transform"), not the group
        // container's id ("the_group") — this is the exact configuration
        // BUG-154's cleanup used to miss.
        bindings: vec![BindingDef {
            id: "amount".into(),
            label: "Amount".into(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: NodeId::new("uv_transform"),
                param: "scale".into(),
            },
            convert: Default::default(),
            user_added: true,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        }],
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
    });
    {
        let fx = project.find_effect_by_id_mut(&id).unwrap();
        fx.graph = Some(def);
        fx.params =
            manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
    }

    let mut cmd = RemoveGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        1, // the group container
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(
        fx.graph.as_ref().unwrap().nodes.iter().all(|n| n.id != 1),
        "group node deleted"
    );
    let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
    assert!(
        meta.bindings.is_empty(),
        "slider bound to a node NESTED inside the removed group must be pruned"
    );
    assert!(meta.params.is_empty(), "its param spec pruned");
    assert!(fx.params.is_empty(), "its value slot pruned");

    cmd.undo(&mut project);
    let fx = project.find_effect_by_id(&id).unwrap();
    let meta = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap();
    assert_eq!(meta.bindings.len(), 1, "binding restored on undo");
    assert_eq!(fx.params.len(), 1, "value slot restored on undo");
}

#[test]
fn revert_prunes_orphaned_automation_and_undo_restores() {
    let (mut project, id) = project_with_one_master_effect();
    {
        let fx = project.find_effect_by_id_mut(&id).unwrap();
        // A user-added binding (lives in the graph) with a driver hung on it.
        fx.append_user_binding(manifold_core::effects::UserParamBinding {
            id: "user.a.b.1".into(),
            label: "B".into(),
            node_id: manifold_core::NodeId::new("a"),
            legacy_node_handle: None,
            inner_param: "b".into(),
            min: 0.0,
            max: 1.0,
            default_value: 0.25,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        });
        fx.create_driver(manifold_core::effects::ParamId::from("user.a.b.1"));
        assert!(fx.find_driver("user.a.b.1").is_some());
    }

    let mut cmd = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(fx.graph.is_none(), "graph reverted to catalog default");
    assert!(
        fx.find_driver("user.a.b.1").is_none(),
        "driver orphaned by the revert is pruned"
    );

    cmd.undo(&mut project);
    let fx = project.find_effect_by_id(&id).unwrap();
    assert!(fx.graph.is_some(), "graph restored");
    assert!(
        fx.find_driver("user.a.b.1").is_some(),
        "the orphaned driver is re-attached on undo"
    );
}

#[test]
fn connect_ports_displaces_existing_wire_and_undo_restores() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    // Rewire mix.b from uv_transform → directly from source.
    let mut cmd = ConnectPortsCommand::new(
        GraphTarget::Effect(id.clone()),
        0,
        "out".to_string(),
        2,
        "b".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    // mix.b is now fed from node 0 (source), not node 1 (uv).
    let mix_b = def
        .wires
        .iter()
        .find(|w| w.to_node == 2 && w.to_port == "b")
        .unwrap();
    assert_eq!(mix_b.from_node, 0);

    cmd.undo(&mut project);
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let mix_b = def
        .wires
        .iter()
        .find(|w| w.to_node == 2 && w.to_port == "b")
        .unwrap();
    assert_eq!(mix_b.from_node, 1, "undo restores original uv→mix.b wire");
}

#[test]
fn disconnect_ports_removes_wire_and_undo_restores() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = DisconnectPortsCommand::new(
        GraphTarget::Effect(id.clone()),
        2,
        "a".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    assert!(
        def.wires
            .iter()
            .all(|w| !(w.to_node == 2 && w.to_port == "a"))
    );

    cmd.undo(&mut project);
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    assert!(def.wires.iter().any(|w| w.to_node == 2 && w.to_port == "a"));
}

#[test]
fn move_graph_node_updates_editor_pos_and_undo_restores() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = MoveGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        (100.0, 200.0),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(node.editor_pos, Some((100.0, 200.0)));

    cmd.undo(&mut project);
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(node.editor_pos, None);
}

#[test]
fn set_graph_node_param_inserts_and_undo_restores_absence() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        "mode".to_string(),
        SerializedParamValue::Enum { value: 7 },
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(
        node.params.get("mode"),
        Some(&SerializedParamValue::Enum { value: 7 })
    );

    cmd.undo(&mut project);
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert!(!node.params.contains_key("mode"), "undo removes the key");
}

#[test]
fn calibrated_source_param_rejects_atomically_but_ordinary_node_writes_remain_live() {
    use manifold_core::NodeId;
    use manifold_core::scene_modifier_preset::{
        SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let mut def = mirror_catalog_default();
    def.nodes[1].node_id = NodeId::new("calibrated-source");
    def.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("modifier"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scene"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: vec![SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("object"),
            },
            source: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("calibrated-source"),
            },
            source_definition_hash: "hash".into(),
            source_offset: [0.0; 3],
            scene_radius: 1.0,
        }],
        legacy_math_view_carrier: None,
        graph: Box::new(mirror_catalog_default()),
    });
    let (mut project, id) = project_with_graph(def.clone());
    let before = serde_json::to_value(&project).unwrap();
    let mut service = crate::service::EditingService::new();
    service.execute(
        Box::new(SetGraphNodeParamCommand::new(
            GraphTarget::Effect(id.clone()),
            1,
            "rotation".into(),
            SerializedParamValue::Float { value: 0.25 },
            def.clone(),
        )),
        &mut project,
    );
    let rejection = service
        .take_rejection()
        .expect("calibrated source is locked");
    assert!(rejection.contains("calibrated"));
    assert_eq!(service.data_version(), 0);
    assert!(!service.can_undo());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);

    service.execute(
        Box::new(SetGraphNodeParamCommand::new(
            GraphTarget::Effect(id),
            2,
            "rotation".into(),
            SerializedParamValue::Float { value: 0.25 },
            def,
        )),
        &mut project,
    );
    assert!(service.take_rejection().is_none());
    assert_eq!(service.data_version(), 1);
    assert!(service.can_undo());
}

#[test]
fn vertices_modifier_allows_host_rt_enabled_with_undo_redo() {
    use manifold_core::NodeId;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let mut def = mirror_catalog_default();
    def.nodes.push(EffectGraphNode {
        id: 99,
        node_id: NodeId::new("scene"),
        type_id: "node.render_scene".into(),
        handle: Some("scene".into()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    let recipe: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 3,
        "presetMetadata": {
            "id": "vertices", "displayName": "Vertices", "category": "Geometry",
            "oscPrefix": "vertices", "params": [], "bindings": [],
            "sceneModifier": {
                "schemaVersion": 1, "singleton": false, "enabledParam": "enabled",
                "stages": [{"group": "deform", "scope": "scene", "outputs":
                    [{"port": "vertices", "endpoint": "vertices"}]}]
            }
        },
        "nodes": [], "wires": []
    }))
    .expect("vertices recipe fixture");
    def.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("modifier"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scene"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: vec![],
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    });
    let (mut project, id) = project_with_graph(def.clone());
    let before = serde_json::to_value(&project).unwrap();
    let mut service = crate::service::EditingService::new();
    service.execute(
        Box::new(SetGraphNodeParamCommand::new(
            GraphTarget::Effect(id.clone()),
            99,
            "rt_enabled".into(),
            SerializedParamValue::Bool { value: true },
            def,
        )),
        &mut project,
    );
    assert!(service.take_rejection().is_none());
    assert_eq!(service.data_version(), 1);
    assert!(service.can_undo());
    let graph = project.find_effect_by_id(&id).unwrap().graph.as_ref().unwrap();
    let scene = graph.nodes.iter().find(|node| node.id == 99).unwrap();
    assert_eq!(scene.params.get("rt_enabled"), Some(&SerializedParamValue::Bool { value: true }));
    let after = serde_json::to_value(&project).unwrap();
    assert!(service.undo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert!(service.redo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), after);
}

#[test]
fn set_graph_node_param_undo_restores_previous_value() {
    let (mut project, id) = project_with_one_master_effect();
    let mut def = mirror_catalog_default();
    // Pre-seed node 1 with mode=3 so undo has something to restore.
    def.nodes
        .iter_mut()
        .find(|n| n.id == 1)
        .unwrap()
        .params
        .insert("mode".to_string(), SerializedParamValue::Enum { value: 3 });
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        "mode".to_string(),
        SerializedParamValue::Enum { value: 7 },
        def,
    );
    cmd.execute(&mut project);
    cmd.undo(&mut project);

    let after = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(
        node.params.get("mode"),
        Some(&SerializedParamValue::Enum { value: 3 }),
    );
}

#[test]
fn set_wgsl_source_sets_and_undo_restores_absence() {
    let (mut project, id) = project_with_one_master_effect();
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(mirror_catalog_default());

    let mut cmd = SetWgslSourceCommand::new(
        GraphTarget::Effect(id.clone()),
        1,
        "fn main() {}".to_string(),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(node.wgsl_source.as_deref(), Some("fn main() {}"));

    cmd.undo(&mut project);
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    assert!(
        node.wgsl_source.is_none(),
        "undo restores the absent source"
    );
}

#[test]
fn set_wgsl_source_empty_clears_back_to_builtin() {
    let (mut project, id) = project_with_one_master_effect();
    let mut def = mirror_catalog_default();
    // Pre-seed node 1 with a custom kernel so the clear has something to drop.
    def.nodes
        .iter_mut()
        .find(|n| n.id == 1)
        .unwrap()
        .wgsl_source = Some("// custom".to_string());
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

    // An all-whitespace buffer clears the override rather than compiling empty.
    let mut cmd =
        SetWgslSourceCommand::new(GraphTarget::Effect(id.clone()), 1, "   ".to_string(), def);
    cmd.execute(&mut project);

    let after = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
    assert!(
        node.wgsl_source.is_none(),
        "blank source clears the override"
    );

    cmd.undo(&mut project);
    let after = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    let node = after.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(node.wgsl_source.as_deref(), Some("// custom"));
}

#[test]
fn revert_clears_graph_and_undo_restores_it() {
    let (mut project, id) = project_with_one_master_effect();
    // Diverge by adding a Blur — graph now Some(...).
    let mut add = AddGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        "node.blur".to_string(),
        None,
        mirror_catalog_default(),
    );
    add.execute(&mut project);
    assert!(project.find_effect_by_id(&id).unwrap().graph.is_some());

    let mut revert = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
    revert.execute(&mut project);
    assert!(
        project.find_effect_by_id(&id).unwrap().graph.is_none(),
        "revert clears the per-card override"
    );

    revert.undo(&mut project);
    assert!(
        project.find_effect_by_id(&id).unwrap().graph.is_some(),
        "undo restores the per-card override"
    );
    let def = project
        .find_effect_by_id(&id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap();
    assert!(def.nodes.iter().any(|n| n.type_id == "node.blur"));
}

#[test]
fn revert_on_already_default_is_a_no_op() {
    // graph: None to start. Revert should be silent (no panic, no
    // change), and undo should also be silent.
    let (mut project, id) = project_with_one_master_effect();
    assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());

    let mut revert = RevertEffectGraphCommand::new(GraphTarget::Effect(id.clone()));
    revert.execute(&mut project);
    assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());

    revert.undo(&mut project);
    assert!(project.find_effect_by_id(&id).unwrap().graph.is_none());
}

/// End-to-end: lift via AddGraphNode, save to JSON, reload, verify
/// the per-card graph survived. Phase 3's load-bearing test for
/// "persistent edits across restart".
#[test]
fn graph_edits_survive_json_round_trip() {
    let (mut project, id) = project_with_one_master_effect();
    let mut cmd = AddGraphNodeCommand::new(
        GraphTarget::Effect(id.clone()),
        "node.blur".to_string(),
        Some((10.0, 20.0)),
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    // Serialize just the PresetInstance — what the project save
    // path emits per effect.
    let fx = project.find_effect_by_id(&id).unwrap();
    let json = serde_json::to_string(fx).unwrap();
    let back: manifold_core::effects::PresetInstance = serde_json::from_str(&json).unwrap();

    assert!(back.graph.is_some(), "graph field survived round-trip");
    let def = back.graph.as_ref().unwrap();
    assert_eq!(def.nodes.len(), 5, "appended Blur survived");
    assert!(def.nodes.iter().any(|n| n.type_id == "node.blur"));
}

/// RAYTRACING_DESIGN.md P1: the scene-level `rt_enabled` toggle wires
/// onto `node.render_scene`'s existing W0 `ParamDef` (RT W0 commit
/// f76253f5) through this SAME generic path every other node param
/// already uses — no bespoke command, no bespoke serialization.
/// `SetGraphNodeParamCommand` writes into `EffectGraphNode::params:
/// BTreeMap<String, SerializedParamValue>`, a plain serde map with no
/// per-param field to add; this proves both halves of the P1
/// deliverable ("through the existing scene def + EditingService
/// path... serialized — round-trip gate applies") against a
/// `node.render_scene`-typed node, mirroring
/// `graph_edits_survive_json_round_trip`'s save/reload shape.
#[test]
fn rt_enabled_toggle_survives_editing_service_and_json_round_trip() {
    let (mut project, id) = project_with_one_master_effect();
    let mut def = mirror_catalog_default();
    // Registry-less at this layer (per `SetGraphNodeParamCommand`'s
    // doc) — a plain fixture node with `node.render_scene`'s type_id
    // is enough to prove the generic param-set + serialize path; the
    // real `ParamDef`/default (W0) lives in `manifold-renderer`, which
    // `manifold-editing` doesn't depend on.
    def.nodes.push(EffectGraphNode {
        id: 99,
        node_id: manifold_core::NodeId::new("scene"),
        type_id: "node.render_scene".to_string(),
        handle: Some("scene".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(id.clone()),
        99,
        "rt_enabled".to_string(),
        SerializedParamValue::Bool { value: true },
        def,
    );
    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    let node = fx
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert_eq!(
        node.params.get("rt_enabled"),
        Some(&SerializedParamValue::Bool { value: true }),
        "EditingService write lands on the scene node's rt_enabled param"
    );

    // Save/reload — the P1 gate's round-trip probe.
    let json = serde_json::to_string(fx).unwrap();
    let back: manifold_core::effects::PresetInstance = serde_json::from_str(&json).unwrap();
    let node = back
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert_eq!(
        node.params.get("rt_enabled"),
        Some(&SerializedParamValue::Bool { value: true }),
        "rt_enabled survives save/reload"
    );

    // Undo removes it — same contract as every other graph-node param.
    cmd.undo(&mut project);
    let fx = project.find_effect_by_id(&id).unwrap();
    let node = fx
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert!(
        !node.params.contains_key("rt_enabled"),
        "undo restores the pre-toggle (absent = ParamDef default false) state"
    );
}

/// RAYTRACING_DESIGN.md section 9 RD9 / T4: the scene-level `rt_reflections`
/// toggle follows the EXACT same generic-param path that `rt_enabled`'s
/// P1 test proved — no bespoke command, no bespoke serialization.
/// Proves both the write and the save/reload round-trip.
#[test]
fn rt_reflections_toggle_survives_editing_service_and_json_round_trip() {
    let (mut project, id) = project_with_one_master_effect();
    let mut def = mirror_catalog_default();
    def.nodes.push(EffectGraphNode {
        id: 99,
        node_id: manifold_core::NodeId::new("scene"),
        type_id: "node.render_scene".to_string(),
        handle: Some("scene".to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    });
    project.find_effect_by_id_mut(&id).unwrap().graph = Some(def.clone());

    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(id.clone()),
        99,
        "rt_reflections".to_string(),
        SerializedParamValue::Bool { value: true },
        def,
    );
    cmd.execute(&mut project);

    let fx = project.find_effect_by_id(&id).unwrap();
    let node = fx
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert_eq!(
        node.params.get("rt_reflections"),
        Some(&SerializedParamValue::Bool { value: true }),
        "EditingService write lands on the scene node's rt_reflections param"
    );

    // Save/reload round-trip.
    let json = serde_json::to_string(fx).unwrap();
    let back: manifold_core::effects::PresetInstance = serde_json::from_str(&json).unwrap();
    let node = back
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert_eq!(
        node.params.get("rt_reflections"),
        Some(&SerializedParamValue::Bool { value: true }),
        "rt_reflections survives save/reload"
    );

    // Undo removes it.
    cmd.undo(&mut project);
    let fx = project.find_effect_by_id(&id).unwrap();
    let node = fx
        .graph
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|n| n.id == 99)
        .unwrap();
    assert!(
        !node.params.contains_key("rt_reflections"),
        "undo restores the pre-toggle state"
    );
}

#[test]
fn add_graph_node_against_generator_target_lifts_layer_generator_graph() {
    let (mut project, lid) = project_with_one_generator_layer();
    let mut cmd = AddGraphNodeCommand::new(
        GraphTarget::Generator(lid.clone()),
        "node.uv_field".to_string(),
        Some((40.0, 50.0)),
        mirror_catalog_default(),
    );

    cmd.execute(&mut project);

    let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
    assert!(
        layer.generator_graph().is_some(),
        "generator_graph must lift from None on first edit",
    );
    let def = layer.generator_graph().unwrap();
    assert_eq!(def.nodes.len(), 5, "catalog 4 + new node = 5");
    assert!(def.nodes.iter().any(|n| n.type_id == "node.uv_field"));
    assert_eq!(layer.generator_graph_version(), 1);
}

#[test]
fn revert_clears_generator_graph_and_undo_restores_it() {
    let (mut project, lid) = project_with_one_generator_layer();
    // Pre-populate with the catalog default (acts as an existing
    // user-edited override).
    project
        .timeline
        .find_layer_by_id_mut(&lid)
        .unwrap()
        .1
        .gen_params_or_init()
        .graph = Some(mirror_catalog_default());

    let mut revert = RevertEffectGraphCommand::new(GraphTarget::Generator(lid.clone()));
    revert.execute(&mut project);
    assert!(
        project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .generator_graph()
            .is_none(),
        "execute clears the override",
    );

    revert.undo(&mut project);
    assert!(
        project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .generator_graph()
            .is_some(),
        "undo restores the previous override",
    );
}

#[test]
fn set_graph_node_param_against_generator_target_routes_to_layer() {
    let (mut project, lid) = project_with_one_generator_layer();
    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Generator(lid.clone()),
        1, // uv_transform node id from mirror_catalog_default
        "rotation".to_string(),
        SerializedParamValue::Float { value: 45.0 },
        mirror_catalog_default(),
    );
    cmd.execute(&mut project);

    let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
    let def = layer.generator_graph().unwrap();
    let node = def.nodes.iter().find(|n| n.id == 1).unwrap();
    let v = node.params.get("rotation").unwrap();
    match v {
        SerializedParamValue::Float { value } => assert!((value - 45.0).abs() < 1e-6),
        _ => panic!("expected Float param value"),
    }
}

#[test]
fn graph_admission_distinguishes_live_param_from_structural_insert() {
    let target = GraphTarget::Effect(EffectId::new("admission-test"));
    let catalog = mirror_catalog_default();
    let live = SetGraphNodeParamCommand::new(
        target.clone(),
        1,
        "rotation".into(),
        SerializedParamValue::Float { value: 2.0 },
        catalog.clone(),
    );
    let mut live_targets = Vec::new();
    live.graph_admission_targets(&mut live_targets);
    assert!(live_targets.is_empty());

    let structural =
        AddGraphNodeCommand::new(target.clone(), "node.uv_field".into(), None, catalog);
    let mut structural_targets = Vec::new();
    structural.graph_admission_targets(&mut structural_targets);
    assert_eq!(structural_targets, vec![target]);
}
