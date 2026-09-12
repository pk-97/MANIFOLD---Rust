use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, PresetMetadata,
    SerializedParamValue, StringBindingDef,
};
use manifold_core::effects::ParamConvert;

use super::SceneModifierExpandError;

pub(super) fn expand_bindings(
    owner: &EffectGraphDef,
    leaf_maps: &BTreeMap<String, BTreeMap<String, Vec<NodeId>>>,
) -> Result<Option<PresetMetadata>, SceneModifierExpandError> {
    let Some(metadata) = owner.preset_metadata.as_ref() else {
        return Ok(None);
    };
    let mut expanded = metadata.clone();
    let mut bindings = Vec::new();
    for binding in &metadata.bindings {
        let BindingTarget::SceneModifier {
            modifier_id,
            param_id,
        } = &binding.target
        else {
            bindings.push(binding.clone());
            continue;
        };
        let instance = find_instance(owner, modifier_id)?;
        let local_metadata = instance
            .graph
            .preset_metadata
            .as_ref()
            .ok_or_else(|| invalid_binding(&binding.id, "modifier instance has no metadata"))?;
        let local_bindings: Vec<&BindingDef> = local_metadata
            .bindings
            .iter()
            .filter(|local| local.id == *param_id)
            .collect();
        if local_bindings.is_empty() {
            return Err(invalid_binding(
                &binding.id,
                format!("modifier parameter '{param_id}' has no local numeric bindings"),
            ));
        }
        if binding.convert != ParamConvert::Float {
            return Err(invalid_binding(
                &binding.id,
                "scene-modifier macro bindings must use ParamConvert::Float",
            ));
        }
        let copies = leaf_maps
            .get(modifier_id.as_str())
            .ok_or_else(|| missing_target(modifier_id, "modifier has no generated leaf map"))?;
        for local in local_bindings {
            let target = local_node_target(local, &binding.id)?;
            let generated = copies
                .get(target.as_str())
                .ok_or_else(|| missing_target(target, "local binding leaf is not generated"))?;
            if generated.is_empty() {
                return Err(missing_target(
                    target,
                    "local binding has no generated copies",
                ));
            }
            let scale = binding.scale * local.scale;
            let offset = binding.offset * local.scale + local.offset;
            if !scale.is_finite() || !offset.is_finite() || !binding.default_value.is_finite() {
                return Err(invalid_binding(
                    &binding.id,
                    "composed binding scale, offset, and default must be finite",
                ));
            }
            for node_id in generated {
                bindings.push(BindingDef {
                    id: binding.id.clone(),
                    label: binding.label.clone(),
                    default_value: binding.default_value,
                    target: BindingTarget::Node {
                        node_id: node_id.clone(),
                        param: local_param(local)?.to_string(),
                    },
                    convert: local.convert,
                    user_added: binding.user_added,
                    scale,
                    offset,
                    default_mirrors_node_param: binding.default_mirrors_node_param,
                });
            }
        }
    }
    expanded.bindings = bindings;

    let mut string_bindings = Vec::new();
    for binding in &metadata.string_bindings {
        let BindingTarget::SceneModifier {
            modifier_id,
            param_id,
        } = &binding.target
        else {
            string_bindings.push(binding.clone());
            continue;
        };
        let instance = find_instance(owner, modifier_id)?;
        let local_metadata = instance
            .graph
            .preset_metadata
            .as_ref()
            .ok_or_else(|| invalid_binding(&binding.id, "modifier instance has no metadata"))?;
        let local_bindings: Vec<&StringBindingDef> = local_metadata
            .string_bindings
            .iter()
            .filter(|local| local.id == *param_id)
            .collect();
        if local_bindings.is_empty() {
            return Err(invalid_binding(
                &binding.id,
                format!("modifier parameter '{param_id}' has no local string bindings"),
            ));
        }
        let copies = leaf_maps
            .get(modifier_id.as_str())
            .ok_or_else(|| missing_target(modifier_id, "modifier has no generated leaf map"))?;
        for local in local_bindings {
            let target = local_node_target_string(local, &binding.id)?;
            let generated = copies.get(target.as_str()).ok_or_else(|| {
                missing_target(target, "local string binding leaf is not generated")
            })?;
            if generated.is_empty() {
                return Err(missing_target(
                    target,
                    "local string binding has no generated copies",
                ));
            }
            for node_id in generated {
                string_bindings.push(StringBindingDef {
                    id: binding.id.clone(),
                    label: binding.label.clone(),
                    default_value: binding.default_value.clone(),
                    target: BindingTarget::Node {
                        node_id: node_id.clone(),
                        param: local_param_string(local)?.to_string(),
                    },
                });
            }
        }
    }
    expanded.string_bindings = string_bindings;
    Ok(Some(expanded))
}

pub(super) fn seed_local_defaults(
    local: &EffectGraphDef,
) -> Result<EffectGraphDef, SceneModifierExpandError> {
    let mut seeded = local.clone();
    let Some(metadata) = local.preset_metadata.as_ref() else {
        return Ok(seeded);
    };

    for binding in &metadata.bindings {
        if binding.default_mirrors_node_param {
            continue;
        }
        let target = match &binding.target {
            BindingTarget::Node { node_id, param } => (node_id, param),
            BindingTarget::Composite { .. } if metadata.scene_modifier.is_none() => continue,
            BindingTarget::Composite { .. } | BindingTarget::SceneModifier { .. } => {
                return Err(invalid_binding(
                    &binding.id,
                    "local default seeding requires a direct node binding",
                ));
            }
        };
        let value = binding.default_value * binding.scale + binding.offset;
        if !value.is_finite() {
            return Err(invalid_binding(
                &binding.id,
                "default mapping must produce a finite value",
            ));
        }
        let node = unique_node_mut(&mut seeded.nodes, target.0, &binding.id)?;
        let converted =
            crate::node_graph::param_binding::convert_param_value(binding.convert, value);
        node.params
            .insert(target.1.clone(), SerializedParamValue::from(converted));
    }

    for binding in &metadata.string_bindings {
        let target = match &binding.target {
            BindingTarget::Node { node_id, param } => (node_id, param),
            BindingTarget::Composite { .. } if metadata.scene_modifier.is_none() => continue,
            BindingTarget::Composite { .. } | BindingTarget::SceneModifier { .. } => {
                return Err(invalid_binding(
                    &binding.id,
                    "local default seeding requires a direct string node binding",
                ));
            }
        };
        let default_value = metadata
            .string_params
            .iter()
            .find(|param| param.id == binding.id)
            .map(|param| param.default_value.clone())
            .unwrap_or_else(|| binding.default_value.clone());
        let node = unique_node_mut(&mut seeded.nodes, target.0, &binding.id)?;
        node.params.insert(
            target.1.clone(),
            SerializedParamValue::String {
                value: default_value,
            },
        );
    }

    Ok(seeded)
}

fn find_instance<'a>(
    owner: &'a EffectGraphDef,
    id: &NodeId,
) -> Result<&'a manifold_core::SceneModifierInstanceDef, SceneModifierExpandError> {
    let mut matches = owner
        .scene_modifiers
        .iter()
        .filter(|instance| instance.id == *id);
    let Some(instance) = matches.next() else {
        return Err(missing_target(id, "scene-modifier instance was not found"));
    };
    if matches.next().is_some() {
        return Err(SceneModifierExpandError::DuplicateIdentity {
            path: id.to_string(),
            detail: "scene-modifier instance id is ambiguous".into(),
        });
    }
    Ok(instance)
}

fn local_node_target<'a>(
    binding: &'a BindingDef,
    path: &str,
) -> Result<&'a NodeId, SceneModifierExpandError> {
    match &binding.target {
        BindingTarget::Node { node_id, .. } => Ok(node_id),
        BindingTarget::Composite { .. } => Err(invalid_binding(
            path,
            "composite local bindings cannot be expanded to generated leaves",
        )),
        BindingTarget::SceneModifier { .. } => Err(SceneModifierExpandError::RecursiveModifier {
            path: path.into(),
            detail: "nested scene-modifier binding is not supported".into(),
        }),
    }
}

fn local_param(binding: &BindingDef) -> Result<&str, SceneModifierExpandError> {
    match &binding.target {
        BindingTarget::Node { param, .. } => Ok(param),
        _ => unreachable!("local_node_target validates the target first"),
    }
}

fn local_node_target_string<'a>(
    binding: &'a StringBindingDef,
    path: &str,
) -> Result<&'a NodeId, SceneModifierExpandError> {
    match &binding.target {
        BindingTarget::Node { node_id, .. } => Ok(node_id),
        BindingTarget::Composite { .. } => Err(invalid_binding(
            path,
            "composite local string bindings cannot be expanded to generated leaves",
        )),
        BindingTarget::SceneModifier { .. } => Err(SceneModifierExpandError::RecursiveModifier {
            path: path.into(),
            detail: "nested scene-modifier string binding is not supported".into(),
        }),
    }
}

fn local_param_string(binding: &StringBindingDef) -> Result<&str, SceneModifierExpandError> {
    match &binding.target {
        BindingTarget::Node { param, .. } => Ok(param),
        _ => unreachable!("local_node_target_string validates the target first"),
    }
}

fn unique_node_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
    path: &str,
) -> Result<&'a mut EffectGraphNode, SceneModifierExpandError> {
    let count = count_nodes(nodes, node_id);
    if count == 0 {
        return Err(missing_target(node_id, "binding target node was not found"));
    }
    if count > 1 {
        return Err(SceneModifierExpandError::DuplicateIdentity {
            path: path.into(),
            detail: format!("binding target node '{node_id}' is ambiguous"),
        });
    }
    find_node_mut(nodes, node_id)
        .ok_or_else(|| missing_target(node_id, "binding target disappeared"))
}

fn count_nodes(nodes: &[EffectGraphNode], node_id: &NodeId) -> usize {
    nodes
        .iter()
        .map(|node| {
            usize::from(node.node_id == *node_id)
                + node
                    .group
                    .as_deref()
                    .map(|group| count_nodes(&group.nodes, node_id))
                    .unwrap_or(0)
        })
        .sum()
}

fn find_node_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
) -> Option<&'a mut EffectGraphNode> {
    for node in nodes {
        if node.node_id == *node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref_mut()
            && let Some(found) = find_node_mut(&mut group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}

fn missing_target(id: &NodeId, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: id.to_string(),
        detail: detail.into(),
    }
}

fn invalid_binding(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidBinding {
        path: path.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::effects::ParamConvert;
    use serde_json::{Value, json};

    fn local_json() -> Value {
        json!({
            "version": 3,
            "presetMetadata": {
                "id": "local",
                "displayName": "Local",
                "category": "Geometry",
                "oscPrefix": "local",
                "params": [],
                "bindings": [{
                    "id": "inner",
                    "label": "Inner",
                    "defaultValue": 0.25,
                    "target": {"kind": "node", "nodeId": "leaf", "param": "amount"},
                    "convert": {"type": "IntRound"},
                    "scale": 2.0,
                    "offset": 0.5
                }],
                "stringParams": [{"id": "label", "name": "Label", "defaultValue": "from-spec"}],
                "stringBindings": [{
                    "id": "label",
                    "label": "Label",
                    "defaultValue": "from-binding",
                    "target": {"kind": "node", "nodeId": "leaf", "param": "text"}
                }]
            },
            "nodes": [{
                "id": 1,
                "nodeId": "leaf",
                "typeId": "node.test",
                "params": {
                    "amount": {"type": "Float", "value": 0.0},
                    "text": {"type": "String", "value": "old"}
                }
            }],
            "wires": []
        })
    }

    fn owner_json() -> Value {
        json!({
            "version": 3,
            "presetMetadata": {
                "id": "owner",
                "displayName": "Owner",
                "category": "Geometry",
                "oscPrefix": "owner",
                "params": [],
                "bindings": [{
                    "id": "ordinary",
                    "label": "Ordinary",
                    "defaultValue": 0.0,
                    "target": {"kind": "node", "nodeId": "host", "param": "amount"}
                }, {
                    "id": "macro",
                    "label": "Macro",
                    "defaultValue": 0.25,
                    "target": {"kind": "sceneModifier", "modifierId": "mod", "paramId": "inner"},
                    "convert": {"type": "Float"},
                    "scale": 3.0,
                    "offset": 4.0
                }],
                "stringBindings": [{
                    "id": "textMacro",
                    "label": "Text Macro",
                    "defaultValue": "host-default",
                    "target": {"kind": "sceneModifier", "modifierId": "mod", "paramId": "label"}
                }]
            },
            "sceneModifiers": [{
                "id": "mod",
                "scene": {"node": "scene"},
                "targets": "allObjects",
                "graph": local_json()
            }],
            "nodes": [],
            "wires": []
        })
    }

    fn owner() -> EffectGraphDef {
        serde_json::from_value(owner_json()).expect("binding fixture parses")
    }

    fn leaf_maps() -> BTreeMap<String, BTreeMap<String, Vec<NodeId>>> {
        let mut inner = BTreeMap::new();
        inner.insert(
            "leaf".to_string(),
            vec![NodeId::new("generated-a"), NodeId::new("generated-b")],
        );
        let mut outer = BTreeMap::new();
        outer.insert("mod".to_string(), inner);
        outer
    }

    #[test]
    fn scene_modifier_expand_bindings_numeric_and_string_fanout() {
        let expanded = expand_bindings(&owner(), &leaf_maps())
            .expect("binding expansion succeeds")
            .expect("metadata present");
        assert_eq!(expanded.bindings.len(), 3);
        assert!(matches!(
            expanded.bindings[0].target,
            BindingTarget::Node { ref node_id, .. } if node_id == &NodeId::new("host")
        ));
        assert_eq!(expanded.bindings[1].id, "macro");
        assert_eq!(expanded.bindings[1].scale, 6.0);
        assert_eq!(expanded.bindings[1].offset, 8.5);
        assert_eq!(expanded.bindings[1].convert, ParamConvert::IntRound);
        assert_eq!(expanded.bindings[2].id, "macro");
        assert_eq!(expanded.string_bindings.len(), 2);
        assert_eq!(expanded.string_bindings[0].default_value, "host-default");
        assert_eq!(expanded.string_bindings[1].default_value, "host-default");

        let applied = crate::node_graph::param_binding::convert_param_value(
            expanded.bindings[1].convert,
            expanded.bindings[1].default_value * expanded.bindings[1].scale
                + expanded.bindings[1].offset,
        );
        assert_eq!(
            applied,
            crate::node_graph::parameters::ParamValue::Float(10.0)
        );
    }

    #[test]
    fn scene_modifier_expand_bindings_preserves_ordinary_and_rejects_bad_macro() {
        let mut raw = owner_json();
        raw["presetMetadata"]["bindings"][1]["convert"] = serde_json::json!({"type": "IntRound"});
        let bad: EffectGraphDef = serde_json::from_value(raw).expect("bad macro parses");
        assert!(matches!(
            expand_bindings(&bad, &leaf_maps()),
            Err(SceneModifierExpandError::InvalidBinding { .. })
        ));

        let mut maps = leaf_maps();
        maps.get_mut("mod").expect("modifier map").remove("leaf");
        assert!(matches!(
            expand_bindings(&owner(), &maps),
            Err(SceneModifierExpandError::MissingTarget { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_bindings_seed_defaults_respects_mirrors_and_string_specs() {
        let local: EffectGraphDef = serde_json::from_value(local_json()).expect("local parses");
        let mut seeded = seed_local_defaults(&local).expect("defaults seed");
        let node = &seeded.nodes[0];
        assert_eq!(
            node.params["amount"],
            SerializedParamValue::Float { value: 1.0 }
        );
        assert_eq!(
            node.params["text"],
            SerializedParamValue::String {
                value: "from-spec".into()
            }
        );

        let binding = &mut seeded.preset_metadata.as_mut().expect("metadata").bindings[0];
        binding.default_mirrors_node_param = true;
        seeded.nodes[0]
            .params
            .insert("amount".into(), SerializedParamValue::Float { value: 9.0 });
        let mirrored = seed_local_defaults(&seeded).expect("mirrored seed skips");
        assert_eq!(
            mirrored.nodes[0].params["amount"],
            SerializedParamValue::Float { value: 9.0 }
        );
    }
}
