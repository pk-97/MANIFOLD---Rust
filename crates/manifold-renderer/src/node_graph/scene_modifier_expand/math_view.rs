use manifold_core::NodeId;

/// Original embedded-view semantics, used only by migrated Scope macros.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyMathViewScope { ThisModifier, WithinChain }

/// Internal request passed through the canonical scene-modifier builder. The
/// requested modifier is the standalone Math View instance; the derived graph
/// evaluates every preceding modifier of the same scene on sampled real faces.
#[derive(Debug, Clone, Copy)]
pub(super) struct MathViewRequest<'a> {
    pub(super) modifier_id: &'a NodeId,
    pub(super) legacy_scope: Option<LegacyMathViewScope>,
}

/// Deterministic saved-frame fixture: no asynchronous asset loading is needed
/// to compare scene and diagram pixels. Fresh import capture is covered by the
/// existing frame tests; these frames exercise the saved-project path.
#[cfg(test)]
pub(crate) fn test_owner() -> manifold_core::effect_graph_def::EffectGraphDef {
    use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
    use manifold_core::scene_modifier_preset::{
        SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
    )))
    .unwrap();
    owner.version = 3;
    // Keep the fixture on the baked scene path used by the native parity
    // proof before source definition hashes are captured.
    for container in &mut owner.nodes {
        let Some(group) = &mut container.group else {
            continue;
        };
        if let Some(material) = group
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.pbr_material")
        {
            material.params.insert(
                "baked_look".into(),
                SerializedParamValue::Bool { value: true },
            );
        }
    }
    let recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/VortexFragments.json"
    )))
    .unwrap();
    let view_recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/MathView.json"
    )))
    .unwrap();
    let mut frames = Vec::new();
    for container in &owner.nodes {
        let Some(group) = &container.group else {
            continue;
        };
        let source = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.cube_mesh")
            .unwrap();
        let object = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.scene_object")
            .unwrap();
        let transform = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.transform_3d")
            .unwrap();
        let scope = vec![container.node_id.clone()];
        frames.push(SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: scope.clone(),
                node: object.node_id.clone(),
            },
            source: SceneNodeRef {
                scope,
                node: source.node_id.clone(),
            },
            source_definition_hash:
                manifold_core::scene_source_identity::scene_source_definition_hash(&owner, source)
                    .unwrap(),
            source_offset: ["pos_x", "pos_y", "pos_z"].map(|param| {
                match transform.params.get(param) {
                    Some(SerializedParamValue::Float { value }) => f64::from(*value),
                    _ => 0.0,
                }
            }),
            scene_radius: 3.0,
        });
    }
    owner.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("vortex_a"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scan_render"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: frames.clone(),
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    });
    owner.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("math_view"),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scan_render"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: frames,
        legacy_math_view_carrier: None,
        graph: Box::new(view_recipe),
    });
    let owner = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &owner,
        &NodeId::new("vortex_a"),
    )
    .unwrap()
    .graph;
    manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &owner,
        &NodeId::new("math_view"),
    )
    .unwrap()
    .graph
}

/// The standalone fixture plus a SpatialEchoes instance (instances-only
/// endpoint writer) between the vertex modifier and the view. Compiler and
/// GPU proofs use it to show captures combine the vertices chain with the
/// instances chain from their own producers.
#[cfg(test)]
pub(crate) fn test_owner_with_instance_echoes() -> manifold_core::effect_graph_def::EffectGraphDef {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::scene_modifier_preset::SceneModifierInstanceDef;
    let owner = test_owner();
    let echo_recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/SpatialEchoes.json"
    )))
    .unwrap();
    let (view_scene, view_targets, view_frames) = {
        let view = owner
            .scene_modifiers
            .iter()
            .find(|instance| instance.id == NodeId::new("math_view"))
            .expect("fixture view");
        (
            view.scene.clone(),
            view.targets.clone(),
            view.mesh_frames.clone(),
        )
    };
    let mut owner = owner;
    owner.scene_modifiers.insert(
        1,
        SceneModifierInstanceDef {
            id: NodeId::new("spatial_echoes"),
            scene: view_scene,
            targets: view_targets,
            mesh_frames: view_frames,
            legacy_math_view_carrier: None,
            graph: Box::new(echo_recipe),
        },
    );
    let owner = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &owner,
        &NodeId::new("spatial_echoes"),
    )
    .unwrap()
    .graph;
    // Host bindings from the original fixture still target vortex_a and
    // math_view only; reconcile the view again so its control values settle
    // after the chain order change.
    manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &owner,
        &NodeId::new("math_view"),
    )
    .unwrap()
    .graph
}
