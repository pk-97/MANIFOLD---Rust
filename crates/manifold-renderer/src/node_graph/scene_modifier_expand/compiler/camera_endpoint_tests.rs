use super::*;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};

fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.into(),
        handle: None,
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.into(),
        to_node,
        to_port: to_port.into(),
    }
}

fn owner(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
    EffectGraphDef {
        version: 3,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes,
        wires,
    }
}

fn instance(scene: &str) -> SceneModifierInstanceDef {
    SceneModifierInstanceDef {
        id: NodeId::new("camera_modifier"),
        scene: SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new(scene),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        graph: Box::new(owner(Vec::new(), Vec::new())),
    }
}

fn builder<'a>(
    graph: &EffectGraphDef,
    index: &'a FlatSceneIndex,
    registry: &'a PrimitiveRegistry,
) -> Builder<'a> {
    Builder {
        derived: graph.clone(),
        index,
        registry,
        next_id: 100,
        current: BTreeMap::new(),
        reference: BTreeMap::new(),
        written: BTreeSet::new(),
        camera_anchors: BTreeMap::new(),
        contexts: BTreeMap::new(),
        event_routes: Vec::new(),
        math_view: None,
        math_seeded: false,
        math_targets: None,
        math_captures: BTreeMap::new(),
        math_samples: BTreeMap::new(),
    }
}

#[test]
fn camera_chain_anchors_before_lens_and_preserves_downstream_fanout() {
    let graph = owner(
        vec![
            node(1, "source", "node.orbit_camera"),
            node(2, "lens", "node.camera_lens"),
            node(3, "scene", "node.render_scene"),
            node(4, "lens_side_consumer", "node.camera_lens"),
        ],
        vec![
            wire(1, "out", 2, "camera"),
            wire(2, "out", 3, "camera"),
            wire(2, "out", 4, "camera"),
        ],
    );
    let index = FlatSceneIndex::build(&graph).unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let mut builder = builder(&graph, &index, &registry);
    let key = builder
        .attachment_key(&instance("scene"), None, SceneEndpoint::Camera)
        .unwrap();

    assert_eq!(key.0.node, NodeId::new("lens"));
    assert_eq!(key.1, "camera");
    assert!(graph.wires.contains(&wire(2, "out", 3, "camera")));
    assert!(graph.wires.contains(&wire(2, "out", 4, "camera")));
}

#[test]
fn camera_stages_reuse_one_anchor_and_reference_original_source() {
    let graph = owner(
        vec![
            node(1, "source", "node.orbit_camera"),
            node(2, "lens", "node.camera_lens"),
            node(3, "scene", "node.render_scene"),
        ],
        vec![wire(1, "out", 2, "camera"), wire(2, "out", 3, "camera")],
    );
    let index = FlatSceneIndex::build(&graph).unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let mut builder = builder(&graph, &index, &registry);
    let modifier = instance("scene");
    let first = builder
        .attachment_key(&modifier, None, SceneEndpoint::Camera)
        .unwrap();
    builder
        .current
        .insert(first.clone(), Some((90, "out".into())));
    let second = builder
        .attachment_key(&modifier, None, SceneEndpoint::Camera)
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(builder.current[&first], Some((90, "out".into())));
    assert_eq!(builder.reference[&first], Some((1, "out".into())));
}

#[test]
fn direct_camera_source_keeps_render_scene_attachment() {
    let graph = owner(
        vec![
            node(1, "source", "node.orbit_camera"),
            node(2, "scene", "node.render_scene"),
        ],
        vec![wire(1, "out", 2, "camera")],
    );
    let index = FlatSceneIndex::build(&graph).unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let mut builder = builder(&graph, &index, &registry);
    let key = builder
        .attachment_key(&instance("scene"), None, SceneEndpoint::Camera)
        .unwrap();

    assert_eq!(key.0.node, NodeId::new("scene"));
    assert_eq!(key.1, "camera");
}

#[test]
fn multi_input_camera_switch_stays_at_render_scene_without_branch_guessing() {
    let graph = owner(
        vec![
            node(1, "source_a", "node.orbit_camera"),
            node(2, "source_b", "node.orbit_camera"),
            node(3, "switch", "node.camera_switch"),
            node(4, "scene", "node.render_scene"),
        ],
        vec![
            wire(1, "out", 3, "a"),
            wire(2, "out", 3, "b"),
            wire(3, "out", 4, "camera"),
        ],
    );
    let index = FlatSceneIndex::build(&graph).unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let mut builder = builder(&graph, &index, &registry);
    let key = builder
        .attachment_key(&instance("scene"), None, SceneEndpoint::Camera)
        .unwrap();

    assert_eq!(key.0.node, NodeId::new("scene"));
    assert_eq!(key.1, "camera");
}

#[test]
fn camera_processing_cycle_is_a_typed_invalid_recipe_error() {
    let graph = owner(
        vec![
            node(1, "lens_a", "node.camera_lens"),
            node(2, "lens_b", "node.camera_lens"),
            node(3, "scene", "node.render_scene"),
        ],
        vec![
            wire(1, "out", 2, "camera"),
            wire(2, "out", 1, "camera"),
            wire(1, "out", 3, "camera"),
        ],
    );
    let index = FlatSceneIndex::build(&graph).unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let mut builder = builder(&graph, &index, &registry);
    let error = builder
        .attachment_key(&instance("scene"), None, SceneEndpoint::Camera)
        .unwrap_err();

    assert!(matches!(
        error,
        SceneModifierExpandError::InvalidRecipe { .. }
    ));
    assert!(
        error
            .to_string()
            .contains("camera processing chain contains a cycle")
    );
}
