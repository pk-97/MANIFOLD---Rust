use std::collections::{BTreeMap, BTreeSet, HashSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GroupParamDef, SerializedParamValue,
};
use manifold_core::flatten::flatten_groups;
use manifold_core::scene_modifier_preset::SceneNodeRef;

use super::SceneModifierExpandError;

#[derive(Debug, Clone)]
pub struct SceneModifierValueSourcePlan {
    entries: Vec<SceneModifierValueSource>,
}

#[derive(Debug, Clone)]
pub struct SceneModifierValueSource {
    pub local: SceneNodeRef,
    pub param: String,
    leaf: SourcePath,
    ancestors: Vec<InterfaceSource>,
}

#[derive(Debug, Clone)]
struct SourcePath {
    nodes: Vec<PathNode>,
}

#[derive(Debug, Clone)]
struct PathNode {
    index: usize,
    node_id: NodeId,
}

#[derive(Debug, Clone)]
struct InterfaceSource {
    group: SourcePath,
    interface_index: usize,
    group_id: NodeId,
    param_name: String,
    target_handle: String,
    target_param: String,
}

#[derive(Debug, Clone)]
struct AncestorGroup {
    path: SourcePath,
    group_id: NodeId,
    handles: Vec<String>,
    params: Vec<GroupParamDef>,
}

impl SceneModifierValueSourcePlan {
    pub fn prepare(graph: &EffectGraphDef) -> Result<Self, SceneModifierExpandError> {
        Self::prepare_with_leaf_params(graph, &BTreeMap::new())
    }

    pub fn prepare_with_leaf_params(
        graph: &EffectGraphDef,
        leaf_params: &BTreeMap<SceneNodeRef, Vec<String>>,
    ) -> Result<Self, SceneModifierExpandError> {
        validate_with_core_flatten(graph)?;
        let mut entries = Vec::new();
        let mut group_ids = HashSet::new();
        walk_nodes(
            &graph.nodes,
            &[],
            &[],
            &[],
            &mut group_ids,
            leaf_params,
            &mut entries,
        )?;
        entries.sort_by(|a, b| a.local.cmp(&b.local).then_with(|| a.param.cmp(&b.param)));
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[SceneModifierValueSource] {
        &self.entries
    }
}

impl SceneModifierValueSource {
    pub fn value<'a>(
        &self,
        graph: &'a EffectGraphDef,
    ) -> Result<Option<&'a SerializedParamValue>, SceneModifierExpandError> {
        // Validate every structural route before looking at any value. An
        // outer override must not hide a stale inner path from the cache.
        for source in &self.ancestors {
            let group = resolve_path(graph, &source.group)?;
            let Some(definition) = group.group.as_deref() else {
                return Err(structural(
                    &self.local,
                    "ancestor source no longer names a group",
                ));
            };
            let Some(param) = definition.interface.params.get(source.interface_index) else {
                return Err(structural(
                    &self.local,
                    "ancestor interface parameter index changed",
                ));
            };
            if group.node_id != source.group_id
                || param.name != source.param_name
                || param.target_handle != source.target_handle
                || param.target_param != source.target_param
            {
                return Err(structural(&self.local, "ancestor group interface changed"));
            }
        }

        let leaf = resolve_path(graph, &self.leaf)?;
        if leaf.group.is_some() || leaf.node_id != self.local.node {
            return Err(structural(&self.local, "prepared leaf path changed"));
        }
        for source in self.ancestors.iter().rev() {
            let group = resolve_path(graph, &source.group)?;
            let definition = group.group.as_deref().expect("validated above");
            let param = &definition.interface.params[source.interface_index];
            if let Some(value) = group.params.get(&source.param_name) {
                return Ok(Some(value));
            }
            if let Some(value) = param.default.as_ref() {
                return Ok(Some(value));
            }
        }
        Ok(leaf.params.get(&self.param))
    }
}

fn validate_with_core_flatten(graph: &EffectGraphDef) -> Result<(), SceneModifierExpandError> {
    let mut scratch = graph.clone();
    scratch.scene_modifiers.clear();
    if let Some(metadata) = scratch.preset_metadata.as_mut() {
        metadata.scene_modifier = None;
        metadata
            .bindings
            .retain(|binding| !matches!(&binding.target, BindingTarget::SceneModifier { .. }));
        metadata
            .string_bindings
            .retain(|binding| !matches!(&binding.target, BindingTarget::SceneModifier { .. }));
    }
    flatten_groups(&scratch)
        .map(|_| ())
        .map_err(|error| structural_root(error.to_string()))
}

fn walk_nodes(
    nodes: &[EffectGraphNode],
    scope: &[NodeId],
    handles: &[String],
    ancestors: &[AncestorGroup],
    group_ids: &mut HashSet<NodeId>,
    leaf_params: &BTreeMap<SceneNodeRef, Vec<String>>,
    entries: &mut Vec<SceneModifierValueSource>,
) -> Result<(), SceneModifierExpandError> {
    if handles.len() > 64 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: "graph".into(),
            detail: "group nesting exceeds depth 64".into(),
        });
    }
    for (index, node) in nodes.iter().enumerate() {
        let mut path = ancestors
            .last()
            .map(|ancestor| ancestor.path.nodes.clone())
            .unwrap_or_default();
        path.push(PathNode {
            index,
            node_id: node.node_id.clone(),
        });
        if let Some(group) = node.group.as_deref() {
            if node.node_id.is_empty() || !group_ids.insert(node.node_id.clone()) {
                return Err(structural(
                    &SceneNodeRef {
                        scope: scope.to_vec(),
                        node: node.node_id.clone(),
                    },
                    "group stable id is empty or duplicated",
                ));
            }
            let group_handle = node.handle.clone().ok_or_else(|| {
                structural(
                    &SceneNodeRef {
                        scope: scope.to_vec(),
                        node: node.node_id.clone(),
                    },
                    "group has no handle",
                )
            })?;
            let mut group_path = handles.to_vec();
            group_path.push(group_handle);
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            let group_source = AncestorGroup {
                path: SourcePath { nodes: path },
                group_id: node.node_id.clone(),
                handles: group_path.clone(),
                params: group.interface.params.clone(),
            };
            let mut child_ancestors = ancestors.to_vec();
            child_ancestors.push(group_source);
            walk_nodes(
                &group.nodes,
                &child_scope,
                &group_path,
                &child_ancestors,
                group_ids,
                leaf_params,
                entries,
            )?;
        } else if node.type_id != GROUP_INPUT_TYPE_ID && node.type_id != GROUP_OUTPUT_TYPE_ID {
            if node.node_id.is_empty() {
                return Err(structural(
                    &SceneNodeRef {
                        scope: scope.to_vec(),
                        node: node.node_id.clone(),
                    },
                    "primitive leaf has an empty stable id",
                ));
            }
            add_leaf_entries(
                node,
                scope,
                handles,
                SourcePath { nodes: path },
                ancestors,
                leaf_params,
                entries,
            )?;
        }
    }
    Ok(())
}

fn add_leaf_entries(
    node: &EffectGraphNode,
    scope: &[NodeId],
    handles: &[String],
    leaf: SourcePath,
    ancestors: &[AncestorGroup],
    leaf_params: &BTreeMap<SceneNodeRef, Vec<String>>,
    entries: &mut Vec<SceneModifierValueSource>,
) -> Result<(), SceneModifierExpandError> {
    let mut names: BTreeSet<String> = node.params.keys().cloned().collect();
    let flat_handle = node.handle.as_ref().map(|handle| {
        handles
            .iter()
            .chain(std::iter::once(handle))
            .cloned()
            .collect::<Vec<_>>()
            .join("/")
    });
    let mut matching = Vec::new();
    for ancestor in ancestors.iter().rev() {
        for (interface_index, param) in ancestor.params.iter().enumerate() {
            let target = format!("{}/{}", ancestor.handles.join("/"), param.target_handle);
            if flat_handle.as_deref() == Some(target.as_str()) {
                names.insert(param.target_param.clone());
                matching.push(InterfaceSource {
                    group: ancestor.path.clone(),
                    interface_index,
                    group_id: ancestor.group_id.clone(),
                    param_name: param.name.clone(),
                    target_handle: param.target_handle.clone(),
                    target_param: param.target_param.clone(),
                });
            }
        }
    }
    let local = SceneNodeRef {
        scope: scope.to_vec(),
        node: node.node_id.clone(),
    };
    if let Some(supplied) = leaf_params.get(&local) {
        names.extend(supplied.iter().cloned());
    }
    for name in names {
        entries.push(SceneModifierValueSource {
            local: local.clone(),
            param: name.clone(),
            leaf: leaf.clone(),
            ancestors: matching
                .iter()
                .filter(|source| source.target_param == name)
                .cloned()
                .collect(),
        });
    }
    Ok(())
}

fn resolve_path<'a>(
    graph: &'a EffectGraphDef,
    path: &SourcePath,
) -> Result<&'a EffectGraphNode, SceneModifierExpandError> {
    let mut nodes = graph.nodes.as_slice();
    let mut current = None;
    for (position, part) in path.nodes.iter().enumerate() {
        let Some(node) = nodes.get(part.index) else {
            return Err(structural_root("prepared source path index changed"));
        };
        if node.node_id != part.node_id {
            return Err(structural_root("prepared source stable id changed"));
        }
        current = Some(node);
        if position + 1 < path.nodes.len() {
            let Some(group) = node.group.as_deref() else {
                return Err(structural_root("prepared source group path changed"));
            };
            nodes = &group.nodes;
        }
    }
    current.ok_or_else(|| structural_root("prepared source path is empty"))
}

fn structural(reference: &SceneNodeRef, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: format!("{:?}", reference),
        detail: detail.into(),
    }
}

fn structural_root(detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: "valueSources".into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{
        EffectGraphNode, GroupDef, GroupInterface, InterfacePortDef,
    };

    fn leaf() -> EffectGraphNode {
        let mut node = EffectGraphNode {
            id: 1,
            node_id: NodeId::new("leaf"),
            type_id: "node.value".into(),
            handle: Some("leaf".into()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        node.params
            .insert("amount".into(), SerializedParamValue::Float { value: 1.0 });
        node
    }

    fn param(name: &str, target_param: &str, default: Option<f32>) -> GroupParamDef {
        GroupParamDef {
            name: name.into(),
            target_handle: "inner/leaf".into(),
            target_param: target_param.into(),
            default: default.map(|value| SerializedParamValue::Float { value }),
        }
    }

    fn graph() -> EffectGraphDef {
        let mut inner = EffectGraphNode {
            id: 2,
            node_id: NodeId::new("inner"),
            type_id: "group".into(),
            handle: Some("inner".into()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: vec![],
                    outputs: vec![],
                    params: vec![GroupParamDef {
                        name: "inner_amount".into(),
                        target_handle: "leaf".into(),
                        target_param: "amount".into(),
                        default: Some(SerializedParamValue::Float { value: 2.0 }),
                    }],
                },
                nodes: vec![leaf()],
                wires: vec![],
                tint: None,
            })),
        };
        inner.params.insert(
            "inner_amount".into(),
            SerializedParamValue::Float { value: 5.0 },
        );
        let mut outer = EffectGraphNode {
            id: 3,
            node_id: NodeId::new("outer"),
            type_id: "group".into(),
            handle: Some("outer".into()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: vec![],
                    outputs: vec![InterfacePortDef {
                        name: "out".into(),
                        port_type: "Scalar(F32)".into(),
                    }],
                    params: vec![
                        param("outer_amount", "amount", Some(3.0)),
                        GroupParamDef {
                            name: "outer_new".into(),
                            target_handle: "inner/leaf".into(),
                            target_param: "new_param".into(),
                            default: None,
                        },
                    ],
                },
                nodes: vec![inner],
                wires: vec![],
                tint: None,
            })),
        };
        outer.params.insert(
            "outer_amount".into(),
            SerializedParamValue::Float { value: 4.0 },
        );
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: vec![],
            nodes: vec![outer],
            wires: vec![],
        }
    }

    fn entry<'a>(
        plan: &'a SceneModifierValueSourcePlan,
        name: &str,
    ) -> &'a SceneModifierValueSource {
        plan.entries()
            .iter()
            .find(|entry| entry.param == name)
            .expect("prepared value source")
    }

    #[test]
    fn scene_modifier_value_sources_outer_override_wins_and_matches_flatten() {
        let graph = graph();
        let plan = SceneModifierValueSourcePlan::prepare(&graph).unwrap();
        let entry = entry(&plan, "amount");
        assert_eq!(entry.local.scope.len(), 2);
        assert_eq!(
            entry.value(&graph).unwrap(),
            Some(&SerializedParamValue::Float { value: 4.0 })
        );
        let flat = flatten_groups(&graph).unwrap();
        assert_eq!(
            flat.nodes[0].params.get("amount"),
            Some(&SerializedParamValue::Float { value: 4.0 })
        );
    }

    #[test]
    fn scene_modifier_value_sources_fall_through_defaults_and_leaf() {
        let mut graph = graph();
        let plan = SceneModifierValueSourcePlan::prepare(&graph).unwrap();
        let entry = entry(&plan, "amount");
        graph.nodes[0].params.remove("outer_amount");
        assert_eq!(
            entry.value(&graph).unwrap(),
            Some(&SerializedParamValue::Float { value: 3.0 })
        );
        graph.nodes[0].group.as_mut().unwrap().interface.params[0].default = None;
        assert_eq!(
            entry.value(&graph).unwrap(),
            Some(&SerializedParamValue::Float { value: 5.0 })
        );
        graph.nodes[0].group.as_mut().unwrap().nodes[0]
            .params
            .remove("inner_amount");
        assert_eq!(
            entry.value(&graph).unwrap(),
            Some(&SerializedParamValue::Float { value: 2.0 })
        );
        graph.nodes[0].group.as_mut().unwrap().interface.params[0].default = None;
        graph.nodes[0].group.as_mut().unwrap().nodes[0]
            .group
            .as_mut()
            .unwrap()
            .interface
            .params[0]
            .default = None;
        assert_eq!(
            entry.value(&graph).unwrap(),
            Some(&SerializedParamValue::Float { value: 1.0 })
        );
    }

    #[test]
    fn scene_modifier_value_sources_include_empty_declared_params_and_reject_path_mutation() {
        let graph = graph();
        let plan = SceneModifierValueSourcePlan::prepare(&graph).unwrap();
        assert!(
            plan.entries()
                .iter()
                .any(|entry| entry.param == "new_param")
        );
        assert_eq!(entry(&plan, "new_param").value(&graph).unwrap(), None);

        let mut changed = graph;
        changed.nodes[0].group.as_mut().unwrap().nodes[0].node_id = NodeId::new("moved");
        assert!(entry(&plan, "amount").value(&changed).is_err());
    }

    #[test]
    fn scene_modifier_value_sources_include_supplied_default_only_params() {
        let graph = graph();
        let local = SceneNodeRef {
            scope: vec![NodeId::new("outer"), NodeId::new("inner")],
            node: NodeId::new("leaf"),
        };
        let mut supplied = BTreeMap::new();
        supplied.insert(local, vec!["late".to_string()]);
        let plan =
            SceneModifierValueSourcePlan::prepare_with_leaf_params(&graph, &supplied).unwrap();
        assert!(plan.entries().iter().any(|entry| entry.param == "late"));

        let mut updated = graph;
        updated.nodes[0].group.as_mut().unwrap().nodes[0]
            .group
            .as_mut()
            .unwrap()
            .nodes[0]
            .params
            .insert("late".into(), SerializedParamValue::Float { value: 8.0 });
        assert_eq!(
            entry(&plan, "late").value(&updated).unwrap(),
            Some(&SerializedParamValue::Float { value: 8.0 })
        );
    }
}
