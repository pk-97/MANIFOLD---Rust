use std::collections::{BTreeMap, BTreeSet, HashSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
};
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneStageScope};

use super::SceneModifierExpandError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneModifierNodeCopy {
    pub object: Option<SceneNodeRef>,
    pub node_id: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneModifierNodeRoute {
    pub modifier_id: NodeId,
    pub local: SceneNodeRef,
    pub copies: Vec<SceneModifierNodeCopy>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedSceneModifierGraph {
    pub def: EffectGraphDef,
    pub routes: Vec<SceneModifierNodeRoute>,
    pub event_routes: Vec<super::SceneModifierEventRoute>,
    /// One entry per expanded numeric binding, before runtime target resolution.
    pub binding_sources: Vec<Option<super::bindings::SceneModifierBindingSource>>,
}

/// Build the authored-to-generated aliases used by value-only updates after
/// structural expansion. This performs no graph mutation or runtime writes.
pub(super) fn build_routes(
    owner: &EffectGraphDef,
    expanded: &EffectGraphDef,
    leaf_maps: &BTreeMap<String, BTreeMap<String, Vec<NodeId>>>,
    target_maps: &BTreeMap<String, Vec<SceneNodeRef>>,
) -> Result<Vec<SceneModifierNodeRoute>, SceneModifierExpandError> {
    let expanded_ids: HashSet<&str> = expanded
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect();
    let mut routes = Vec::new();
    let mut modifier_ids = BTreeSet::new();

    for instance in &owner.scene_modifiers {
        if !modifier_ids.insert(instance.id.as_str()) {
            return Err(duplicate(
                &instance.id,
                "modifier instance id is duplicated",
            ));
        }
        let local_maps = leaf_maps
            .get(instance.id.as_str())
            .ok_or_else(|| missing(&instance.id, "modifier has no generated leaf map"))?;
        let targets = target_maps
            .get(instance.id.as_str())
            .ok_or_else(|| missing(&instance.id, "modifier has no stable target list"))?;
        let stage_scopes = stage_scopes(&instance.graph);
        let mut leaves = BTreeMap::new();
        let mut seen_ids = HashSet::new();
        collect_leaves(
            &instance.graph.nodes,
            &[],
            None,
            &stage_scopes,
            &mut seen_ids,
            &mut leaves,
            &instance.id,
        )?;

        for key in local_maps.keys() {
            if !seen_ids.contains(key.as_str()) {
                return Err(invalid(
                    instance.id.to_string(),
                    format!("generated leaf map contains unknown local id '{key}'"),
                ));
            }
        }

        let mut generated_ids = HashSet::new();
        for (local_id, (local, scope)) in leaves {
            let copies = local_maps.get(&local_id).ok_or_else(|| {
                missing(
                    &local.node,
                    format!("generated copies for '{local_id}' are missing"),
                )
            })?;
            let expected = if scope == SceneStageScope::EachObject {
                targets.len()
            } else {
                1
            };
            if copies.len() != expected {
                return Err(invalid(
                    local.node.to_string(),
                    format!(
                        "expected {expected} generated copies for '{local_id}', got {}",
                        copies.len()
                    ),
                ));
            }
            let mut route_copies = Vec::with_capacity(copies.len());
            for (index, generated) in copies.iter().enumerate() {
                if !expanded_ids.contains(generated.as_str()) {
                    return Err(missing(
                        generated,
                        "generated leaf is absent from the expanded graph",
                    ));
                }
                if !generated_ids.insert(generated.as_str().to_string()) {
                    return Err(duplicate(
                        generated,
                        "generated node is aliased by more than one local leaf",
                    ));
                }
                route_copies.push(SceneModifierNodeCopy {
                    object: (scope == SceneStageScope::EachObject).then(|| targets[index].clone()),
                    node_id: generated.clone(),
                });
            }
            routes.push(SceneModifierNodeRoute {
                modifier_id: instance.id.clone(),
                local,
                copies: route_copies,
            });
        }
    }

    if leaf_maps
        .keys()
        .any(|id| !modifier_ids.contains(id.as_str()))
        || target_maps
            .keys()
            .any(|id| !modifier_ids.contains(id.as_str()))
    {
        return Err(invalid(
            "sceneModifiers",
            "route input contains an unknown modifier instance",
        ));
    }
    Ok(routes)
}

fn stage_scopes(graph: &EffectGraphDef) -> BTreeMap<String, SceneStageScope> {
    graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .map(|recipe| {
            recipe
                .stages
                .iter()
                .map(|stage| (stage.group.as_str().to_string(), stage.scope))
                .collect()
        })
        .unwrap_or_default()
}

fn collect_leaves(
    nodes: &[EffectGraphNode],
    scope: &[NodeId],
    root_scope: Option<SceneStageScope>,
    stage_scopes: &BTreeMap<String, SceneStageScope>,
    seen_ids: &mut HashSet<String>,
    leaves: &mut BTreeMap<String, (SceneNodeRef, SceneStageScope)>,
    path: &NodeId,
) -> Result<(), SceneModifierExpandError> {
    if scope.len() > 64 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: path.to_string(),
            detail: "modifier group nesting exceeds depth 64".into(),
        });
    }
    for node in nodes {
        if !node.node_id.is_empty() && !seen_ids.insert(node.node_id.as_str().to_string()) {
            return Err(duplicate(
                &node.node_id,
                "local stable node id is duplicated in the modifier recipe",
            ));
        }
        let boundary = node.type_id == GROUP_INPUT_TYPE_ID || node.type_id == GROUP_OUTPUT_TYPE_ID;
        if let Some(group) = node.group.as_deref() {
            let next_root = if scope.is_empty() {
                stage_scopes
                    .get(node.node_id.as_str())
                    .copied()
                    .or(root_scope)
            } else {
                root_scope
            };
            let mut child_scope = scope.to_vec();
            if !node.node_id.is_empty() {
                child_scope.push(node.node_id.clone());
            }
            collect_leaves(
                &group.nodes,
                &child_scope,
                next_root,
                stage_scopes,
                seen_ids,
                leaves,
                path,
            )?;
        } else if !boundary && !node.node_id.is_empty() {
            let reference = SceneNodeRef {
                scope: scope.to_vec(),
                node: node.node_id.clone(),
            };
            let scope_kind = root_scope.unwrap_or(SceneStageScope::Scene);
            if leaves
                .insert(node.node_id.as_str().to_string(), (reference, scope_kind))
                .is_some()
            {
                return Err(duplicate(
                    &node.node_id,
                    "local primitive leaf is duplicated in the modifier recipe",
                ));
            }
        }
    }
    Ok(())
}

fn missing(id: &NodeId, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: id.to_string(),
        detail: detail.into(),
    }
}

fn duplicate(id: &NodeId, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::DuplicateIdentity {
        path: id.to_string(),
        detail: detail.into(),
    }
}

fn invalid(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: path.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn owner_json() -> Value {
        json!({
            "version": 3,
            "sceneModifiers": [{
                "id": "modifier",
                "scene": {"node": "scene"},
                "targets": {"explicit": {"objects": [
                    {"scope": ["object"], "node": "obj-a"},
                    {"scope": ["object"], "node": "obj-b"}
                ]}},
                "graph": {
                    "version": 3,
                    "presetMetadata": {
                        "id": "recipe",
                        "displayName": "Recipe",
                        "category": "Geometry",
                        "oscPrefix": "recipe",
                        "params": [],
                        "bindings": [],
                        "sceneModifier": {
                            "schemaVersion": 1,
                            "singleton": false,
                            "enabledParam": "enabled",
                            "stages": [
                                {"group": "shared-stage", "scope": "scene"},
                                {"group": "object-stage", "scope": "eachObject"}
                            ]
                        }
                    },
                    "nodes": [
                        {"id": 1, "nodeId": "shared-stage", "typeId": "group", "group": {
                            "interface": {"inputs": [], "outputs": [], "params": []},
                            "nodes": [
                                {"id": 1, "nodeId": "shared-input", "typeId": "system.group_input"},
                                {"id": 2, "nodeId": "shared-leaf", "typeId": "node.value"},
                                {"id": 3, "nodeId": "shared-output", "typeId": "system.group_output"}
                            ], "wires": []
                        }},
                        {"id": 2, "nodeId": "object-stage", "typeId": "group", "group": {
                            "interface": {"inputs": [], "outputs": [], "params": []},
                            "nodes": [
                                {"id": 1, "nodeId": "object-input", "typeId": "system.group_input"},
                                {"id": 2, "nodeId": "nested-stage", "typeId": "group", "group": {
                                    "interface": {"inputs": [], "outputs": [], "params": []},
                                    "nodes": [
                                        {"id": 1, "nodeId": "nested-input", "typeId": "system.group_input"},
                                        {"id": 2, "nodeId": "nested-leaf", "typeId": "node.value"},
                                        {"id": 3, "nodeId": "nested-output", "typeId": "system.group_output"}
                                    ], "wires": []
                                }},
                                {"id": 3, "nodeId": "object-output", "typeId": "system.group_output"}
                            ], "wires": []
                        }}
                    ],
                    "wires": []
                }
            }],
            "nodes": [],
            "wires": []
        })
    }

    type LeafMaps = BTreeMap<String, BTreeMap<String, Vec<NodeId>>>;
    type TargetMaps = BTreeMap<String, Vec<SceneNodeRef>>;

    fn fixture() -> (EffectGraphDef, EffectGraphDef, LeafMaps, TargetMaps) {
        let owner: EffectGraphDef = serde_json::from_value(owner_json()).expect("owner parses");
        let expanded: EffectGraphDef = serde_json::from_value(json!({
            "version": 3,
            "nodes": [
                {"id": 1, "nodeId": "generated-shared", "typeId": "node.value"},
                {"id": 2, "nodeId": "generated-a", "typeId": "node.value"},
                {"id": 3, "nodeId": "generated-b", "typeId": "node.value"}
            ], "wires": []
        }))
        .expect("expanded parses");
        let leaf_maps = BTreeMap::from([(
            "modifier".to_string(),
            BTreeMap::from([
                ("shared-stage".to_string(), Vec::new()),
                ("shared-input".to_string(), Vec::new()),
                (
                    "shared-leaf".to_string(),
                    vec![NodeId::new("generated-shared")],
                ),
                ("shared-output".to_string(), Vec::new()),
                ("object-stage".to_string(), Vec::new()),
                ("object-input".to_string(), Vec::new()),
                ("nested-stage".to_string(), Vec::new()),
                ("nested-input".to_string(), Vec::new()),
                (
                    "nested-leaf".to_string(),
                    vec![NodeId::new("generated-a"), NodeId::new("generated-b")],
                ),
                ("nested-output".to_string(), Vec::new()),
                ("object-output".to_string(), Vec::new()),
            ]),
        )]);
        let target_maps = BTreeMap::from([(
            "modifier".to_string(),
            vec![
                SceneNodeRef {
                    scope: vec![NodeId::new("object")],
                    node: NodeId::new("obj-a"),
                },
                SceneNodeRef {
                    scope: vec![NodeId::new("object")],
                    node: NodeId::new("obj-b"),
                },
            ],
        )]);
        (owner, expanded, leaf_maps, target_maps)
    }

    #[test]
    fn scene_modifier_expand_routes_shared_and_each_object_copies() {
        let (owner, expanded, leaf_maps, target_maps) = fixture();
        let routes =
            build_routes(&owner, &expanded, &leaf_maps, &target_maps).expect("routes build");
        let shared = routes
            .iter()
            .find(|route| route.local.node == "shared-leaf")
            .unwrap();
        assert_eq!(shared.local.scope, vec![NodeId::new("shared-stage")]);
        assert_eq!(
            shared.copies,
            vec![SceneModifierNodeCopy {
                object: None,
                node_id: NodeId::new("generated-shared")
            }]
        );
        let each = routes
            .iter()
            .find(|route| route.local.node == "nested-leaf")
            .unwrap();
        assert_eq!(
            each.local.scope,
            vec![NodeId::new("object-stage"), NodeId::new("nested-stage")]
        );
        assert_eq!(each.copies.len(), 2);
        assert_eq!(
            each.copies[0].object,
            Some(target_maps["modifier"][0].clone())
        );
        assert_eq!(
            each.copies[1].object,
            Some(target_maps["modifier"][1].clone())
        );
    }

    #[test]
    fn scene_modifier_expand_routes_reject_wrong_cardinality_or_unknown_generated_id() {
        let (owner, expanded, mut leaf_maps, target_maps) = fixture();
        leaf_maps
            .get_mut("modifier")
            .unwrap()
            .get_mut("nested-leaf")
            .unwrap()
            .pop();
        assert!(matches!(
            build_routes(&owner, &expanded, &leaf_maps, &target_maps),
            Err(SceneModifierExpandError::InvalidRecipe { .. })
        ));
        let (owner, expanded, mut leaf_maps, target_maps) = fixture();
        leaf_maps
            .get_mut("modifier")
            .unwrap()
            .insert("shared-leaf".into(), vec![NodeId::new("missing")]);
        assert!(matches!(
            build_routes(&owner, &expanded, &leaf_maps, &target_maps),
            Err(SceneModifierExpandError::MissingTarget { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_routes_reject_duplicate_local_ids_and_extra_maps() {
        let (mut owner, expanded, leaf_maps, target_maps) = fixture();
        let graph = owner.scene_modifiers[0].graph.as_mut();
        let group = graph.nodes[0].group.as_mut().unwrap();
        let duplicate = group.nodes[1].clone();
        group.nodes.push(duplicate);
        assert!(matches!(
            build_routes(&owner, &expanded, &leaf_maps, &target_maps),
            Err(SceneModifierExpandError::DuplicateIdentity { .. })
        ));
        let (owner, expanded, mut leaf_maps, target_maps) = fixture();
        leaf_maps
            .get_mut("modifier")
            .unwrap()
            .insert("unknown".into(), vec![NodeId::new("generated-shared")]);
        assert!(matches!(
            build_routes(&owner, &expanded, &leaf_maps, &target_maps),
            Err(SceneModifierExpandError::InvalidRecipe { .. })
        ));
        let (owner, expanded, leaf_maps, mut target_maps) = fixture();
        target_maps.insert("unknown-modifier".into(), Vec::new());
        assert!(matches!(
            build_routes(&owner, &expanded, &leaf_maps, &target_maps),
            Err(SceneModifierExpandError::InvalidRecipe { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_routes_skip_groups_and_boundaries_and_preserve_inputs() {
        let (owner, expanded, leaf_maps, target_maps) = fixture();
        let canonical_owner = owner.clone();
        let canonical_expanded = expanded.clone();
        let routes =
            build_routes(&owner, &expanded, &leaf_maps, &target_maps).expect("routes build");
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|route| {
            !matches!(
                route.local.node.as_str(),
                "shared-stage" | "shared-input" | "shared-output" | "nested-stage"
            )
        }));
        assert_eq!(owner, canonical_owner);
        assert_eq!(expanded, canonical_expanded);
    }
}
