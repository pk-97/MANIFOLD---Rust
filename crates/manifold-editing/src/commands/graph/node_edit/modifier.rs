//! Nested graph edits for a scene-modifier target.
//!
//! A modifier graph is authored inside its generator's owner graph. This
//! module keeps the owner graph and the instance layer in one reversible
//! transaction while leaving ordinary effect/generator node edits untouched.

use std::collections::HashSet;

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::project::Project;
use manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters;

use super::super::descend_level;
use super::super::scene_modifier::{InstanceLayerSnapshot, prune_instance_params};
use super::subtree_node_ids;

#[derive(Debug, Clone)]
pub(super) struct RemovedNode {
    owner_graph: Option<EffectGraphDef>,
    instance: InstanceLayerSnapshot,
}

/// Remove a node from a modifier-local graph and reconcile its public macros.
/// Invalid targets, scopes, or node ids return `None` before touching the live
/// owner. The local draft is intentionally not schema-validated here; runtime
/// graph admission owns that diagnostic.
pub(super) fn execute(
    project: &mut Project,
    target: &GraphTarget,
    node_id: u32,
    scope: &[u32],
) -> Option<RemovedNode> {
    let GraphTarget::SceneModifier { owner, modifier_id } = target else {
        return None;
    };
    let host = project.graph_target_owner_mut(owner)?;
    let previous_graph = host.graph.clone()?;
    let previous_instance = InstanceLayerSnapshot::capture(host);
    let mut candidate = previous_graph.clone();

    let removed_node = {
        let local = target.graph_in_mut(&mut candidate)?;
        let (nodes, wires) = descend_level(&mut local.nodes, &mut local.wires, scope)?;
        let position = nodes.iter().position(|node| node.id == node_id)?;
        let node = nodes.remove(position);
        wires.retain(|wire| wire.from_node != node_id && wire.to_node != node_id);
        node
    };

    let removed_ids = subtree_node_ids(&removed_node);
    remove_local_bindings_and_specs(&mut candidate, target, &removed_ids);

    // Draft topology may be incomplete; metadata reconciliation itself must
    // succeed so a control can never outlive the node it addressed.
    let (candidate, removed_param_ids) =
        match reconcile_scene_modifier_parameters(&candidate, modifier_id) {
            Ok(edit) => (edit.graph, edit.removed_param_ids),
            Err(error) => {
                eprintln!("[manifold-editing] modifier node removal rejected: {error}");
                return None;
            }
        };
    host.graph = Some(candidate);
    prune_instance_params(host, &removed_param_ids);
    if !removed_param_ids.is_empty() {
        host.refresh_manifest_from_graph();
    }
    host.bump_graph_structure_version();

    Some(RemovedNode {
        owner_graph: Some(previous_graph),
        instance: previous_instance,
    })
}

pub(super) fn undo(project: &mut Project, target: &GraphTarget, state: &RemovedNode) -> bool {
    let Some(owner) = target.host_target() else {
        return false;
    };
    let Some(host) = project.graph_target_owner_mut(owner) else {
        return false;
    };
    host.graph = state.owner_graph.clone();
    state.instance.clone().restore(host);
    host.bump_graph_structure_version();
    true
}

/// Remove local bindings targeting the deleted subtree. A local spec can be
/// shared by another remaining binding, so descriptors are removed only when
/// no numeric or string binding still carries that id.
fn remove_local_bindings_and_specs(
    owner_graph: &mut EffectGraphDef,
    target: &GraphTarget,
    removed_nodes: &[NodeId],
) {
    let Some(local) = target.graph_in_mut(owner_graph) else {
        return;
    };
    let Some(metadata) = local.preset_metadata.as_mut() else {
        return;
    };
    let removed: HashSet<String> = metadata
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, .. } if removed_nodes.iter().any(|id| id == node_id) => {
                Some(binding.id.clone())
            }
            _ => None,
        })
        .chain(
            metadata
                .string_bindings
                .iter()
                .filter_map(|binding| match &binding.target {
                    BindingTarget::Node { node_id, .. }
                        if removed_nodes.iter().any(|id| id == node_id) =>
                    {
                        Some(binding.id.clone())
                    }
                    _ => None,
                }),
        )
        .collect();
    if removed.is_empty() {
        return;
    }
    metadata.bindings.retain(|binding| {
        !matches!(&binding.target, BindingTarget::Node { node_id, .. }
            if removed_nodes.iter().any(|id| id == node_id))
    });
    metadata.string_bindings.retain(|binding| {
        !matches!(&binding.target, BindingTarget::Node { node_id, .. }
            if removed_nodes.iter().any(|id| id == node_id))
    });
    let still_used = metadata
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .chain(
            metadata
                .string_bindings
                .iter()
                .map(|binding| binding.id.as_str()),
        )
        .collect::<HashSet<_>>();
    metadata
        .params
        .retain(|param| !removed.contains(&param.id) || still_used.contains(param.id.as_str()));
    metadata
        .string_params
        .retain(|param| !removed.contains(&param.id) || still_used.contains(param.id.as_str()));
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{modifier_draft_fixture, modifier_exposure_command};
    use super::super::{RemoveGraphNodeCommand, SetGraphNodeParamCommand};
    use crate::command::Command;
    use manifold_core::effect_graph_def::SerializedParamValue;

    #[test]
    fn scene_modifier_node_edit_remove_restores_exposed_control_and_other_instance() {
        let (mut project, target, default) = modifier_draft_fixture();
        let mut expose = modifier_exposure_command(target.clone(), default.clone(), true);
        expose.execute(&mut project);
        let before = project.graph_target_owner(&target).unwrap().graph.clone();
        let params = project.graph_target_owner(&target).unwrap().params.clone();
        let mut remove = RemoveGraphNodeCommand::new(target.clone(), 1, default.clone());
        remove.execute(&mut project);
        assert!(remove.was_applied());
        let owner = project.graph_target_owner(&target).unwrap();
        assert!(
            target
                .graph_in(owner.graph.as_ref().unwrap())
                .unwrap()
                .nodes
                .is_empty()
        );
        assert_eq!(
            owner.graph.as_ref().unwrap().scene_modifiers[1],
            before.as_ref().unwrap().scene_modifiers[1]
        );
        assert!(owner.params.is_empty());
        remove.undo(&mut project);
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.graph, before);
        assert_eq!(owner.params, params);
        remove.execute(&mut project);
        assert!(remove.was_applied());
        remove.undo(&mut project);
        let version = project.graph_target_owner(&target).unwrap().graph_version;
        let mut invalid = RemoveGraphNodeCommand::new(target.clone(), 999, default);
        invalid.execute(&mut project);
        assert!(!invalid.was_applied());
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph_version,
            version
        );
    }

    #[test]
    fn scene_modifier_node_edit_redirect_composes_local_and_host_transforms() {
        let (mut project, target, default) = modifier_draft_fixture();
        let mut expose = modifier_exposure_command(target.clone(), default.clone(), true);
        expose.execute(&mut project);
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        let outer = &mut graph.preset_metadata.as_mut().unwrap().bindings[0];
        outer.scale = 2.0;
        outer.offset = 1.0;
        let id = outer.id.clone();
        let local = target.graph_in_mut(graph).unwrap();
        let binding = &mut local.preset_metadata.as_mut().unwrap().bindings[0];
        binding.scale = 3.0;
        binding.offset = 4.0;
        let before = owner.graph.clone();
        let base = owner.get_base_param(&id);
        let mut write = SetGraphNodeParamCommand::new(
            target.clone(),
            1,
            "value".into(),
            SerializedParamValue::Float { value: 10.0 },
            default,
        );
        write.execute(&mut project);
        let owner = project.graph_target_owner(&target).unwrap();
        assert!((owner.get_base_param(&id) - 0.5).abs() < 1e-6);
        assert_eq!(owner.graph, before);
        write.undo(&mut project);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&id),
            base
        );
    }
}
