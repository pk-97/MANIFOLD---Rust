//! Resolve authored control values before installing a scene-object copy.
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef,
};
use manifold_core::effects::{
    PresetInstance, apply_card_reshape, invert_card_reshape, serialized_value_as_f32,
};
use manifold_core::{GraphTarget, NodeId};

pub(super) fn prepare(
    owner: &PresetInstance,
    owner_default: &EffectGraphDef,
    target: &GraphTarget,
    candidate: &EffectGraphDef,
    node_id_map: &[(NodeId, NodeId)],
) -> Result<Vec<(String, f32)>, String> {
    let previous_owner = owner.graph.as_ref().unwrap_or(owner_default);
    let mut next_owner = previous_owner.clone();
    *target
        .graph_in_mut(&mut next_owner)
        .ok_or("Duplicate Object parameter owner is unavailable")? = candidate.clone();
    if let GraphTarget::SceneModifier { modifier_id, .. } = target {
        next_owner = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            &next_owner,
            modifier_id,
        )
        .map_err(|error| format!("Duplicate Object parameters: {error}"))?
        .graph;
    }
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
        let source_slot = owner_slot(previous_owner, target, &binding.id)?;
        if !owner.params.contains(&source_slot) {
            continue;
        }
        let mut value = owner.get_base_param(&source_slot);
        if matches!(target, GraphTarget::SceneModifier { .. }) {
            let (host_spec, host_binding) = slot_definition(previous_owner, &source_slot)?;
            value = forward(value, host_spec, host_binding);
        }
        // The duplicate gesture offsets the copied transform in X. Preserve
        // that offset when the live authored value owns the node.
        let old = find_node(&candidate.nodes, node_id)
            .ok_or("Duplicate Object source control is unavailable")?;
        let new = find_node(&candidate.nodes, new_id)
            .ok_or("Duplicate Object copied control is unavailable")?;
        if param == "pos_x" && old.type_id == "node.transform_3d" {
            let x = |node: &EffectGraphNode| {
                node.params
                    .get(param)
                    .and_then(serialized_value_as_f32)
                    .unwrap_or(0.0)
            };
            let delta = x(new) - x(old);
            if delta != 0.0 {
                value = inverse(forward(value, spec, binding) + delta, spec, copy)?;
            }
        }
        let destination = owner_slot(&next_owner, target, &copy.id)?;
        if matches!(target, GraphTarget::SceneModifier { .. }) {
            let (host_spec, host_binding) = slot_definition(&next_owner, &destination)?;
            value = inverse(value, host_spec, host_binding)?;
        }
        if !value.is_finite() {
            return Err("Duplicate Object requires finite authored control values".into());
        }
        if let Some((_, previous)) = values.iter().find(|(id, _)| id == &destination) {
            if *previous != value {
                return Err("Duplicate Object cannot offset a shared control independently".into());
            }
        } else {
            values.push((destination, value));
        }
    }
    Ok(values)
}

fn owner_slot(owner: &EffectGraphDef, target: &GraphTarget, local: &str) -> Result<String, String> {
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return Ok(local.to_owned());
    };
    owner
        .preset_metadata
        .as_ref()
        .and_then(|meta| {
            meta.bindings.iter().find(|binding| {
                matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: id, param_id }
            if id == modifier_id && param_id == local)
            })
        })
        .map(|binding| binding.id.clone())
        .ok_or_else(|| "Duplicate Object modifier control is unavailable".into())
}
fn slot_definition<'a>(
    owner: &'a EffectGraphDef,
    id: &str,
) -> Result<(&'a ParamSpecDef, &'a BindingDef), String> {
    let meta = owner
        .preset_metadata
        .as_ref()
        .ok_or("Duplicate Object control metadata is unavailable")?;
    let spec = meta
        .params
        .iter()
        .find(|spec| spec.id == id)
        .ok_or("Duplicate Object control spec is unavailable")?;
    let binding = meta
        .bindings
        .iter()
        .find(|binding| binding.id == id)
        .ok_or("Duplicate Object control binding is unavailable")?;
    Ok((spec, binding))
}
fn forward(value: f32, spec: &ParamSpecDef, binding: &BindingDef) -> f32 {
    apply_card_reshape(
        value,
        spec.min,
        spec.max,
        spec.invert,
        spec.curve,
        binding.scale,
        binding.offset,
    )
}
fn inverse(value: f32, spec: &ParamSpecDef, binding: &BindingDef) -> Result<f32, String> {
    invert_card_reshape(
        value,
        spec.min,
        spec.max,
        spec.invert,
        spec.curve,
        binding.scale,
        binding.offset,
    )
    .ok_or_else(|| "Duplicate Object cannot preserve a control with zero scale".into())
}
fn find_node<'a>(nodes: &'a [EffectGraphNode], id: &NodeId) -> Option<&'a EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if &node.node_id == id {
            Some(node)
        } else {
            node.group
                .as_ref()
                .and_then(|group| find_node(&group.nodes, id))
        }
    })
}
