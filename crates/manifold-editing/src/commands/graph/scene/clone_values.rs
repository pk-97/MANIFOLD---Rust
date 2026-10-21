//! Resolve a scene-object copy's card values before it is committed.
use manifold_core::effect_graph_def::{BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef};
use manifold_core::effects::{PresetInstance, apply_card_reshape, invert_card_reshape, serialized_value_as_f32};
use manifold_core::NodeId;

/// Each copied control's base value: the source control's live base, moved
/// by the copy's transform offset where the control owns that transform.
pub(super) fn prepare(
    owner: &PresetInstance,
    candidate: &EffectGraphDef,
    node_id_map: &[(NodeId, NodeId)],
) -> Result<Vec<(String, f32)>, String> {
    let Some(meta) = candidate.preset_metadata.as_ref() else {
        return Ok(Vec::new());
    };
    let mut values: Vec<(String, f32)> = Vec::new();
    for binding in &meta.bindings {
        if binding.user_added {
            continue;
        }
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some((_, new_id)) = node_id_map.iter().find(|(old, _)| old == node_id) else {
            continue;
        };
        let Some(copy) = meta.bindings.iter().find(|copy| {
            matches!(&copy.target,
            BindingTarget::Node { node_id, param: key } if node_id == new_id && key == param)
        }) else {
            continue;
        };
        let Some(spec) = meta.params.iter().find(|spec| spec.id == binding.id) else {
            continue;
        };
        if !owner.params.contains(&binding.id) {
            continue;
        }
        let mut value = owner.get_base_param(&binding.id);
        // The duplicate offsets the copied transform in X. Keep that offset
        // when the live authored value owns the node.
        let old = find_node(&candidate.nodes, node_id).ok_or("Duplicate Object source control is unavailable")?;
        let new = find_node(&candidate.nodes, new_id).ok_or("Duplicate Object copied control is unavailable")?;
        if param == "pos_x" && old.type_id == "node.transform_3d" {
            let x = |node: &EffectGraphNode| node.params.get(param).and_then(serialized_value_as_f32).unwrap_or(0.0);
            let delta = x(new) - x(old);
            if delta != 0.0 {
                value = inverse(forward(value, spec, binding) + delta, spec, copy)?;
            }
        }
        if !value.is_finite() {
            return Err("Duplicate Object requires finite authored control values".into());
        }
        if let Some((_, previous)) = values.iter().find(|(id, _)| id == &copy.id) {
            if *previous != value {
                return Err("Duplicate Object cannot offset a shared control independently".into());
            }
        } else {
            values.push((copy.id.clone(), value));
        }
    }
    Ok(values)
}

fn forward(value: f32, spec: &ParamSpecDef, binding: &BindingDef) -> f32 {
    apply_card_reshape(value, spec.min, spec.max, spec.invert, spec.curve, binding.scale, binding.offset)
}

fn inverse(value: f32, spec: &ParamSpecDef, binding: &BindingDef) -> Result<f32, String> {
    invert_card_reshape(value, spec.min, spec.max, spec.invert, spec.curve, binding.scale, binding.offset)
        .ok_or_else(|| "Duplicate Object cannot preserve a control with zero scale".into())
}

fn find_node<'a>(nodes: &'a [EffectGraphNode], id: &NodeId) -> Option<&'a EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if &node.node_id == id {
            Some(node)
        } else {
            node.group.as_ref().and_then(|group| find_node(&group.nodes, id))
        }
    })
}
