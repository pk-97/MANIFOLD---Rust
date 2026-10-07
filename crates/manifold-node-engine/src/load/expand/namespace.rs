use std::collections::{BTreeMap, HashSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GroupDef,
};

use super::SceneModifierExpandError;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 65_536;
const MAX_WIRES: usize = 262_144;
const NAMESPACE_PREFIX: &str = "__scene_modifier_namespace_v1";

pub(super) fn namespace_node_id(parts: &[&str]) -> NodeId {
    let mut encoded = String::from(NAMESPACE_PREFIX);
    for part in parts {
        encoded.push_str(&format!("{:08x}", part.len()));
        for byte in part.as_bytes() {
            encoded.push_str(&format!("{:02x}", byte));
        }
    }
    NodeId::new(encoded)
}

pub(super) fn clone_template_node(
    template: &EffectGraphNode,
    namespace: &[&str],
    next_id: &mut u32,
) -> Result<(EffectGraphNode, BTreeMap<String, NodeId>), SceneModifierExpandError> {
    let mut state = Preflight::default();
    preflight_node(template, &[], 0, &mut state)?;

    let mut cursor = *next_id;
    let mut stable_map = BTreeMap::new();
    let cloned = clone_node(template, namespace, &[], true, &mut cursor, &mut stable_map)?;
    *next_id = cursor;
    Ok((cloned, stable_map))
}

#[derive(Default)]
struct Preflight {
    nodes: usize,
    wires: usize,
    stable_ids: HashSet<String>,
}

fn preflight_node(
    node: &EffectGraphNode,
    local_scope: &[String],
    depth: usize,
    state: &mut Preflight,
) -> Result<(), SceneModifierExpandError> {
    if depth > MAX_DEPTH {
        return Err(capacity(
            path_for(local_scope, node),
            "group nesting exceeds depth 64",
        ));
    }
    state.nodes = state.nodes.saturating_add(1);
    if state.nodes > MAX_NODES {
        return Err(capacity(
            path_for(local_scope, node),
            "authored node count exceeds 65536",
        ));
    }

    if node.node_id.is_empty()
        && node.type_id != GROUP_INPUT_TYPE_ID
        && node.type_id != GROUP_OUTPUT_TYPE_ID
    {
        return Err(invalid(
            path_for(local_scope, node),
            "empty stable ids are reserved for group boundary nodes",
        ));
    }
    if !node.node_id.is_empty() && !state.stable_ids.insert(node.node_id.as_str().to_string()) {
        return Err(duplicate(
            path_for(local_scope, node),
            "stable node id is duplicated throughout the template",
        ));
    }

    if let Some(group) = node.group.as_deref() {
        let mut doc_ids = HashSet::new();
        for child in &group.nodes {
            if !doc_ids.insert(child.id) {
                return Err(duplicate(
                    path_for(local_scope, node),
                    format!("document id {} is duplicated in one scope", child.id),
                ));
            }
        }
        state.wires = state.wires.saturating_add(group.wires.len());
        if state.wires > MAX_WIRES {
            return Err(capacity(
                path_for(local_scope, node),
                "authored wire count exceeds 262144",
            ));
        }
        let ids: HashSet<u32> = group.nodes.iter().map(|child| child.id).collect();
        for wire in &group.wires {
            if !ids.contains(&wire.from_node) || !ids.contains(&wire.to_node) {
                return Err(SceneModifierExpandError::MissingInput {
                    path: path_for(local_scope, node),
                    detail: format!(
                        "wire endpoint {} -> {} is missing from its local scope",
                        wire.from_node, wire.to_node
                    ),
                });
            }
        }

        let mut child_scope = local_scope.to_vec();
        if !node.node_id.is_empty() {
            child_scope.push(node.node_id.as_str().to_string());
        }
        for child in &group.nodes {
            preflight_node(child, &child_scope, depth + 1, state)?;
        }
    }
    Ok(())
}

fn clone_node(
    template: &EffectGraphNode,
    namespace: &[&str],
    local_scope: &[String],
    root: bool,
    next_id: &mut u32,
    stable_map: &mut BTreeMap<String, NodeId>,
) -> Result<EffectGraphNode, SceneModifierExpandError> {
    let new_id = allocate(next_id, path_for(local_scope, template))?;
    let generated_node_id = if template.node_id.is_empty() {
        boundary_node_id(namespace, local_scope, template)
    } else {
        generated_stable_id(namespace, local_scope, template.node_id.as_str())
    };
    if !template.node_id.is_empty() {
        stable_map.insert(
            template.node_id.as_str().to_string(),
            generated_node_id.clone(),
        );
    }

    // Clone the node's own data once. Cloning `template` here would also clone
    // its complete subtree at every nesting level before replacing it below.
    let mut cloned = EffectGraphNode {
        id: new_id,
        node_id: generated_node_id.clone(),
        type_id: template.type_id.clone(),
        handle: if root {
            Some(root_handle(&generated_node_id))
        } else {
            template.handle.clone()
        },
        params: template.params.clone(),
        exposed_params: template.exposed_params.clone(),
        editor_pos: template.editor_pos,
        wgsl_source: template.wgsl_source.clone(),
        title: template.title.clone(),
        output_formats: template.output_formats.clone(),
        output_canvas_scales: template.output_canvas_scales.clone(),
        group: None,
    };

    if let Some(group) = template.group.as_deref() {
        let mut numeric_map = BTreeMap::new();
        let mut child_scope = local_scope.to_vec();
        if !template.node_id.is_empty() {
            child_scope.push(template.node_id.as_str().to_string());
        }
        let mut cloned_nodes = Vec::with_capacity(group.nodes.len());
        for child in &group.nodes {
            let child_id = child.id;
            let cloned_child =
                clone_node(child, namespace, &child_scope, false, next_id, stable_map)?;
            numeric_map.insert(child_id, cloned_child.id);
            cloned_nodes.push(cloned_child);
        }
        let cloned_wires = group
            .wires
            .iter()
            .map(|wire| {
                let from_node = numeric_map.get(&wire.from_node).copied().ok_or_else(|| {
                    missing_input(&path_for(local_scope, template), wire.from_node)
                })?;
                let to_node = numeric_map
                    .get(&wire.to_node)
                    .copied()
                    .ok_or_else(|| missing_input(&path_for(local_scope, template), wire.to_node))?;
                Ok(EffectGraphWire {
                    from_node,
                    from_port: wire.from_port.clone(),
                    to_node,
                    to_port: wire.to_port.clone(),
                })
            })
            .collect::<Result<Vec<_>, SceneModifierExpandError>>()?;
        cloned.group = Some(Box::new(GroupDef {
            interface: group.interface.clone(),
            nodes: cloned_nodes,
            wires: cloned_wires,
            tint: group.tint,
        }));
    }
    Ok(cloned)
}

fn generated_stable_id(namespace: &[&str], local_scope: &[String], leaf: &str) -> NodeId {
    let namespace = namespace_node_id(namespace);
    let mut parts = vec!["node", namespace.as_str()];
    parts.extend(local_scope.iter().map(String::as_str));
    parts.push(leaf);
    namespace_node_id(&parts)
}

fn boundary_node_id(namespace: &[&str], local_scope: &[String], node: &EffectGraphNode) -> NodeId {
    let namespace = namespace_node_id(namespace);
    let mut parts = vec!["boundary", namespace.as_str()];
    parts.extend(local_scope.iter().map(String::as_str));
    parts.push(node.type_id.as_str());
    let document_id = node.id.to_string();
    parts.push(document_id.as_str());
    namespace_node_id(&parts)
}

fn root_handle(node_id: &NodeId) -> String {
    // namespace_node_id already uses only slash-free ASCII hex and a prefix.
    node_id.to_string()
}

fn allocate(next_id: &mut u32, path: String) -> Result<u32, SceneModifierExpandError> {
    let id = *next_id;
    *next_id = next_id
        .checked_add(1)
        .ok_or_else(|| capacity(path, "numeric node id allocation overflow"))?;
    Ok(id)
}

fn path_for(scope: &[String], node: &EffectGraphNode) -> String {
    scope
        .iter()
        .cloned()
        .chain(std::iter::once(if node.node_id.is_empty() {
            node.type_id.clone()
        } else {
            node.node_id.as_str().to_string()
        }))
        .collect::<Vec<_>>()
        .join("/")
}

fn invalid(path: String, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path,
        detail: detail.into(),
    }
}

fn duplicate(path: String, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::DuplicateIdentity {
        path,
        detail: detail.into(),
    }
}

fn capacity(path: String, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::CapacityExceeded {
        path,
        detail: detail.into(),
    }
}

fn missing_input(path: &str, endpoint: u32) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingInput {
        path: path.to_string(),
        detail: format!("wire endpoint {endpoint} is missing from its local scope"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use manifold_core::effect_graph_def::{
        EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface, GroupParamDef, InterfacePortDef,
    };

    use super::*;

    fn node(id: u32, stable: &str, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(stable),
            type_id: type_id.to_string(),
            handle: handle.map(str::to_string),
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

    fn group_template() -> EffectGraphNode {
        let mut root = node(10, "root", "group", Some("display label"));
        let mut body = node(11, "leaf", "node.value", Some("nested label"));
        body.params.insert(
            "value".to_string(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: 1.0 },
        );
        root.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![InterfacePortDef {
                    name: "out".to_string(),
                    port_type: "Scalar(F32)".to_string(),
                }],
                params: vec![GroupParamDef {
                    name: "amount".to_string(),
                    target_handle: "nested label".to_string(),
                    target_param: "value".to_string(),
                    default: None,
                }],
            },
            nodes: vec![body, node(12, "boundary", GROUP_OUTPUT_TYPE_ID, None)],
            wires: vec![EffectGraphWire {
                from_node: 11,
                from_port: "value".to_string(),
                to_node: 12,
                to_port: "out".to_string(),
            }],
            tint: None,
        }));
        root
    }

    #[test]
    fn scene_modifier_expand_namespace_tuple_encoding_is_unambiguous() {
        assert_ne!(
            generated_stable_id(&["a"], &["b".into()], "c"),
            generated_stable_id(&["a", "b"], &[], "c"),
            "the namespace/local-path tuple boundary must be retained"
        );
        assert_ne!(
            namespace_node_id(&["ab", "c"]),
            namespace_node_id(&["a", "bc"])
        );
        assert_ne!(
            namespace_node_id(&["a/b", "c"]),
            namespace_node_id(&["a", "b/c"])
        );
        assert_ne!(namespace_node_id(&["é"]), namespace_node_id(&["éé"]));
    }

    #[test]
    fn scene_modifier_expand_namespace_is_deterministic_and_disjoint() {
        let template = group_template();
        let canonical = template.clone();
        let mut next_a = 100;
        let (clone_a, map_a) =
            clone_template_node(&template, &["modifier", "target"], &mut next_a).unwrap();
        let mut next_b = 100;
        let (clone_b, map_b) =
            clone_template_node(&template, &["modifier", "target"], &mut next_b).unwrap();
        assert_eq!(clone_a, clone_b);
        assert_eq!(map_a, map_b);
        assert_eq!(template, canonical);

        let mut next_c = 100;
        let (_, map_c) =
            clone_template_node(&template, &["modifier", "other-target"], &mut next_c).unwrap();
        assert_ne!(map_a["root"], map_c["root"]);
        assert_ne!(map_a["leaf"], map_c["leaf"]);
    }

    #[test]
    fn scene_modifier_expand_namespace_rewrites_nested_wires_and_preserves_routes() {
        let template = group_template();
        let mut next = 0;
        let (cloned, map) = clone_template_node(&template, &["m", "t"], &mut next).unwrap();
        let group = cloned.group.as_deref().unwrap();
        assert_eq!(group.interface.params[0].target_handle, "nested label");
        assert_ne!(group.nodes[0].id, 11);
        assert_ne!(group.wires[0].from_node, 11);
        assert_ne!(group.wires[0].to_node, 12);
        assert_eq!(cloned.node_id, map["root"].clone());
    }

    #[test]
    fn scene_modifier_expand_namespace_rejects_invalid_templates_and_rolls_back_ids() {
        let mut duplicate = group_template();
        duplicate.group.as_mut().unwrap().nodes.push(node(
            11,
            "other",
            "node.value",
            Some("duplicate doc id"),
        ));
        let mut next = 7;
        assert!(matches!(
            clone_template_node(&duplicate, &["m"], &mut next),
            Err(SceneModifierExpandError::DuplicateIdentity { .. })
        ));
        assert_eq!(next, 7);

        let mut missing_wire = group_template();
        missing_wire.group.as_mut().unwrap().wires[0].from_node = 999;
        assert!(matches!(
            clone_template_node(&missing_wire, &["m"], &mut next),
            Err(SceneModifierExpandError::MissingInput { .. })
        ));
        assert_eq!(next, 7);

        let mut overflow_next = u32::MAX;
        assert!(matches!(
            clone_template_node(&group_template(), &["m"], &mut overflow_next),
            Err(SceneModifierExpandError::CapacityExceeded { .. })
        ));
        assert_eq!(overflow_next, u32::MAX);
    }
}
