//! Local recipe exposure uses the ordinary user-binding metadata and the
//! owner's existing manifest/modulation storage.
use super::super::{InstanceLayerSnapshot, prune_instance_params};
use super::*;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::effects::{PresetInstance, UserParamBinding};
use manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters;

pub(super) fn execute(command: &mut ToggleNodeParamExposeCommand, project: &mut Project) {
    command.reverse = NodeExposeReverse::None;
    let Some((candidate, exposure_ids)) = prepare(command, project) else {
        return;
    };
    let Some(owner) = project.graph_target_owner_mut(&command.target) else {
        return;
    };
    let previous = owner.graph.clone();
    let instance = InstanceLayerSnapshot::capture(owner);
    owner.graph = Some(candidate.graph);
    prune_instance_params(owner, &candidate.removed_param_ids);
    owner.refresh_manifest_from_graph();
    for id in exposure_ids {
        owner.set_param_exposed(&id, command.expose);
    }
    owner.bump_graph_structure_version();
    command.reverse = NodeExposeReverse::Modifier {
        graph: previous,
        instance,
    };
}

pub(super) fn undo(command: &mut ToggleNodeParamExposeCommand, project: &mut Project) {
    let NodeExposeReverse::Modifier { graph, instance } = std::mem::take(&mut command.reverse)
    else {
        return;
    };
    if let Some(owner) = project.graph_target_owner_mut(&command.target) {
        owner.graph = graph;
        instance.restore(owner);
        owner.bump_graph_structure_version();
    }
}

fn prepare(
    command: &ToggleNodeParamExposeCommand,
    project: &Project,
) -> Option<(
    manifold_core::scene_modifier_edit::SceneModifierGraphEdit,
    Vec<String>,
)> {
    let GraphTarget::SceneModifier { modifier_id, .. } = &command.target else {
        return None;
    };
    let owner = project.graph_target_owner(&command.target)?;
    let before = owner.graph.as_ref().unwrap_or(&command.catalog_default);
    let mut candidate = before.clone();
    let local = command.target.graph_in_mut(&mut candidate)?;
    let node_id = if command.node_id.is_empty() {
        NodeId::new(command.node_handle.as_str())
    } else {
        command.node_id.clone()
    };
    materialize_binding_exposures(local);
    let static_slot = static_slot_for(local, &node_id, &command.inner_param);
    let section = innermost_group_display_name(&local.nodes, &command.scope_path);
    let (nodes, _) = descend_level(&mut local.nodes, &mut local.wires, &command.scope_path)?;
    flip_node_exposed(
        nodes,
        command.node_u32_id,
        &command.inner_param,
        command.expose,
    )?;
    let metadata = local.preset_metadata.as_mut()?;
    let static_id = static_slot
        .and_then(|slot| metadata.params.get(slot))
        .map(|p| p.id.clone());
    if static_id.is_none() {
        let existing = metadata
            .bindings
            .iter()
            .find(|binding| {
                binding.user_added
                    && matches!(&binding.target, BindingTarget::Node { node_id: nid, param }
                if nid == &node_id && param == &command.inner_param)
            })
            .cloned();
        if command.expose && existing.is_none() {
            let ids: Vec<_> = metadata
                .params
                .iter()
                .map(|p| p.id.clone())
                .chain(metadata.string_params.iter().map(|p| p.id.clone()))
                .collect();
            let id = crate::commands::effects::generate_user_param_id(
                &command.node_handle,
                &command.inner_param,
                &ids,
            );
            let binding = UserParamBinding {
                id,
                label: command.inner_label.clone(),
                node_id: node_id.clone(),
                legacy_node_handle: None,
                inner_param: command.inner_param.clone(),
                min: command.inner_min,
                max: command.inner_max,
                default_value: command.inner_default,
                convert: command.inner_meta.unwrap_or_default(),
                is_angle: command.inner_is_angle,
                invert: false,
                curve: Default::default(),
                scale: 1.0,
                offset: 0.0,
                value_labels: command.inner_value_labels.clone(),
                section,
            };
            metadata.params.push(binding.param_spec());
            metadata.bindings.push(binding.binding_def());
        } else if !command.expose
            && let Some(binding) = existing
        {
            // Freeze the effective macro value through both binding transforms.
            let effective = effective_local_value(owner, before, modifier_id, &binding)?;
            metadata.bindings.retain(|b| b.id != binding.id);
            metadata.params.retain(|p| p.id != binding.id);
            let node =
                find_node_by_id_or_handle_mut(&mut local.nodes, &node_id, &command.node_handle)?;
            node.params.insert(
                command.inner_param.clone(),
                effective_value_to_serialized(binding.convert, effective),
            );
        }
    }
    let exposure_ids = static_id.map(|id| {
        before.preset_metadata.iter().flat_map(|meta| &meta.bindings)
            .filter(|binding| matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: mid, param_id }
                if mid == modifier_id && param_id == &id))
            .map(|binding| binding.id.clone()).collect()
    }).unwrap_or_default();
    match reconcile_scene_modifier_parameters(&candidate, modifier_id) {
        Ok(edit) => Some((edit, exposure_ids)),
        Err(error) => {
            eprintln!("[manifold-editing] modifier exposure rejected: {error}");
            None
        }
    }
}

fn effective_local_value(
    owner: &PresetInstance,
    graph: &EffectGraphDef,
    modifier_id: &NodeId,
    local: &manifold_core::effect_graph_def::BindingDef,
) -> Option<f32> {
    let outer = graph
        .preset_metadata
        .as_ref()?
        .bindings
        .iter()
        .find(|binding| {
            matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: mid, param_id }
            if mid == modifier_id && param_id == &local.id)
        })?;
    let param = owner.params.get(&outer.id)?;
    Some(manifold_core::effects::apply_card_reshape(
        param.value,
        param.spec.min,
        param.spec.max,
        param.spec.invert,
        param.spec.curve,
        outer.scale * local.scale,
        outer.offset * local.scale + local.offset,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{
        modifier_draft_fixture as fixture, modifier_exposure_command as command,
    };
    use super::*;

    #[test]
    fn scene_modifier_exposure_uses_host_macro_and_restores_exact_snapshot() {
        let (mut project, target, before) = fixture();
        let mut expose = command(target.clone(), before.clone(), true);
        expose.execute(&mut project);
        assert!(expose.was_applied());
        let host = project.graph_target_owner(&target).unwrap();
        let graph = host.graph.as_ref().unwrap();
        assert_eq!(graph.scene_modifiers[1], before.scene_modifiers[1]);
        let macro_id = graph.preset_metadata.as_ref().unwrap().params[0].id.clone();
        assert_eq!(host.params.get(&macro_id).unwrap().value, 0.2);
        let exposed_graph = graph.clone();
        let host = project.graph_target_owner_mut(&target).unwrap();
        host.params.get_mut(&macro_id).unwrap().value = 0.75;
        host.params.get_mut(&macro_id).unwrap().base = 0.6;
        let live_params = host.params.clone();
        let mut hide = command(target.clone(), before.clone(), false);
        hide.execute(&mut project);
        assert!(hide.was_applied());
        let host = project.graph_target_owner(&target).unwrap();
        assert!(host.params.get(&macro_id).is_none());
        assert_eq!(
            target.graph_in(host.graph.as_ref().unwrap()).unwrap().nodes[0].params["value"],
            SerializedParamValue::Float { value: 0.75 }
        );
        hide.undo(&mut project);
        let host = project.graph_target_owner(&target).unwrap();
        assert_eq!(host.graph.as_ref().unwrap(), &exposed_graph);
        assert_eq!(host.params, live_params);
        expose.undo(&mut project);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .graph
                .as_ref()
                .unwrap(),
            &before
        );
        expose.execute(&mut project);
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .graph
                .as_ref()
                .unwrap(),
            &exposed_graph
        );
    }

    #[test]
    fn scene_modifier_exposure_invalid_node_is_atomic() {
        let (mut project, target, before) = fixture();
        let version = project.graph_target_owner(&target).unwrap().graph_version;
        let mut expose = command(target.clone(), before.clone(), true);
        expose.node_u32_id = 999;
        expose.execute(&mut project);
        assert!(!expose.was_applied());
        let host = project.graph_target_owner(&target).unwrap();
        assert_eq!(host.graph.as_ref().unwrap(), &before);
        assert_eq!(host.graph_version, version);
    }
}
