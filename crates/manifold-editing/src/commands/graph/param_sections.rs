//! Keep a modifier's local section labels and exclusive host macros together.

use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, NodeId};

pub(super) fn rename_modifier_sections(
    project: &mut Project,
    target: &GraphTarget,
    inside: &[NodeId],
    old: &str,
    new: &str,
) -> Vec<(String, Option<String>)> {
    let Some(local) = project.graph_for_target(target, None) else {
        return vec![];
    };
    let Some(meta) = local.preset_metadata.as_ref() else {
        return vec![];
    };
    let ids: std::collections::HashSet<_> = meta
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, .. } if inside.contains(node_id) => {
                Some(binding.id.as_str())
            }
            _ => None,
        })
        .collect();
    let changed: Vec<_> = meta
        .params
        .iter()
        .filter(|param| ids.contains(param.id.as_str()) && param.section.as_deref() == Some(old))
        .map(|param| (param.id.clone(), param.section.clone()))
        .collect();
    for (id, _) in &changed {
        set_modifier_section(project, target, id, Some(new.to_owned()));
    }
    changed
}

pub(super) fn set_modifier_section(
    project: &mut Project,
    target: &GraphTarget,
    param_id: &str,
    section: Option<String>,
) {
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return;
    };
    let Some(owner) = project.graph_target_owner_mut(target) else {
        return;
    };
    let Some(graph) = owner.graph.as_mut() else {
        return;
    };
    let Some(local) = target.graph_in_mut(graph) else {
        return;
    };
    let Some(spec) = local
        .preset_metadata
        .as_mut()
        .and_then(|meta| meta.params.iter_mut().find(|param| param.id == param_id))
    else {
        return;
    };
    spec.section = section.clone();
    let Some(meta) = graph.preset_metadata.as_mut() else {
        return;
    };
    // A shared macro has its own host-facing section; renaming one consumer
    // must not rename controls shared with another modifier.
    let ids: Vec<_> = meta.bindings.iter().filter(|binding|matches!(&binding.target,
        BindingTarget::SceneModifier { modifier_id:mid, param_id:pid } if mid == modifier_id && pid == param_id))
        .filter(|binding|meta.bindings.iter().filter(|other|other.id==binding.id).count()==1)
        .map(|binding|binding.id.clone()).collect();
    for id in ids {
        if let Some(spec) = meta.params.iter_mut().find(|param| param.id == id) {
            spec.section = section.clone();
        }
        if let Some(param) = owner.params.get_mut(&id) {
            param.spec.section = section.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{modifier_draft_fixture, modifier_exposure_command};
    use super::*;
    use crate::command::Command;

    #[test]
    fn scene_modifier_sections_preserve_shared_macro_labels_and_other_instances() {
        let (mut project, target, default) = modifier_draft_fixture();
        let mut expose = modifier_exposure_command(target.clone(), default, true);
        expose.execute(&mut project);
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        let meta = graph.preset_metadata.as_mut().unwrap();
        let macro_id = meta.params[0].id.clone();
        meta.params[0].section = Some("Old".into());
        owner.params.get_mut(&macro_id).unwrap().spec.section = Some("Old".into());
        let local = target.graph_in_mut(graph).unwrap();
        let spec = &mut local.preset_metadata.as_mut().unwrap().params[0];
        spec.section = Some("Old".into());
        let local_id = spec.id.clone();
        let saved =
            rename_modifier_sections(&mut project, &target, &[NodeId::new("value")], "Old", "New");
        assert_eq!(saved, vec![(local_id.clone(), Some("Old".into()))]);
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(
            owner.params.get(&macro_id).unwrap().spec.section.as_deref(),
            Some("New")
        );
        set_modifier_section(&mut project, &target, &local_id, Some("Old".into()));
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        graph.scene_modifiers[1].graph = graph.scene_modifiers[0].graph.clone();
        let meta = graph.preset_metadata.as_mut().unwrap();
        let mut shared = meta.bindings[0].clone();
        shared.target = BindingTarget::SceneModifier {
            modifier_id: NodeId::new("b"),
            param_id: local_id.clone(),
        };
        meta.bindings.push(shared);
        rename_modifier_sections(
            &mut project,
            &target,
            &[NodeId::new("value")],
            "Old",
            "Shared Consumer",
        );
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(
            owner.params.get(&macro_id).unwrap().spec.section.as_deref(),
            Some("Old")
        );
        let graph = owner.graph.as_ref().unwrap();
        assert_eq!(
            graph.scene_modifiers[0]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params[0]
                .section
                .as_deref(),
            Some("Shared Consumer")
        );
        assert_eq!(
            graph.scene_modifiers[1]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params[0]
                .section
                .as_deref(),
            Some("Old")
        );
    }
}
