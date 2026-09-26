use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};

/// Copy the scene-panel numeric bindings whose targets belong to a duplicated
/// object. Both root-level physics nodes and grouped scene nodes store these
/// exposures in the shared preset metadata. Each copied binding gets a fresh
/// id while fanout bindings retain one id for all cloned targets.
pub(super) fn clone_scene_bindings(def: &mut EffectGraphDef, node_id_map: &[(NodeId, NodeId)]) {
    // The cloned nodes are already in the graph when this metadata pass runs.
    // Snapshot handles before borrowing preset metadata mutably.
    let node_handles = collect_node_handles(&def.nodes);
    let Some(meta) = def.preset_metadata.as_mut() else {
        return;
    };
    let mut binding_ids = meta
        .bindings
        .iter()
        .map(|binding| binding.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut param_ids = meta
        .params
        .iter()
        .map(|param| param.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let source_bindings = meta.bindings.clone();
    let source_params = meta.params.clone();
    let mut cloned_id_by_source = std::collections::HashMap::<String, String>::new();
    let mut cloned_params = Vec::new();
    let mut cloned_bindings = Vec::new();

    for binding in source_bindings {
        // D11 card exposures are deliberate identities owned by the source
        // object. Scene-panel auto exposures are the only bindings that may
        // be copied to give a duplicate its own transform/material controls.
        if binding.user_added {
            continue;
        }
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some((_, new_node_id)) = node_id_map.iter().find(|(old, _)| old == node_id) else {
            continue;
        };
        let new_binding_id = if let Some(existing) = cloned_id_by_source.get(&binding.id) {
            existing.clone()
        } else {
            let base = format!("{}_duplicate", binding.id);
            let mut candidate = base.clone();
            let mut suffix = 2;
            while binding_ids.contains(&candidate) {
                candidate = format!("{base}_{suffix}");
                suffix += 1;
            }
            binding_ids.insert(candidate.clone());
            cloned_id_by_source.insert(binding.id.clone(), candidate.clone());
            if let Some(source_param) = source_params
                .iter()
                .find(|param_spec| param_spec.id == binding.id)
            {
                let mut param_spec = source_param.clone();
                param_spec.id = candidate.clone();
                param_spec.section = cloned_owned_section(
                    param_spec.section.as_deref(),
                    node_id,
                    &node_handles,
                    node_id_map,
                );
                if param_ids.insert(candidate.clone()) {
                    cloned_params.push(param_spec);
                }
            }
            candidate
        };
        let mut cloned = binding.clone();
        cloned.id = new_binding_id;
        cloned.target = BindingTarget::Node {
            node_id: new_node_id.clone(),
            param: param.clone(),
        };
        cloned_bindings.push(cloned);
    }
    meta.params.extend(cloned_params);
    meta.bindings.extend(cloned_bindings);
}

struct SectionOwner {
    id: NodeId,
    handle: Option<String>,
    parent: Option<NodeId>,
}

fn collect_node_handles(nodes: &[EffectGraphNode]) -> Vec<SectionOwner> {
    let mut handles = Vec::new();
    fn visit(nodes: &[EffectGraphNode], parent: Option<&NodeId>, handles: &mut Vec<SectionOwner>) {
        for node in nodes {
            handles.push(SectionOwner {
                id: node.node_id.clone(),
                handle: node.handle.clone(),
                parent: parent.cloned(),
            });
            if let Some(group) = node.group.as_deref() {
                visit(&group.nodes, Some(&node.node_id), handles);
            }
        }
    }
    visit(nodes, None, &mut handles);
    handles
}

fn cloned_owned_section(
    section: Option<&str>,
    source: &NodeId,
    owners: &[SectionOwner],
    node_id_map: &[(NodeId, NodeId)],
) -> Option<String> {
    let section = section?;
    let mut current = Some(source);
    while let Some(source) = current {
        let Some(owner) = owners.iter().find(|owner| &owner.id == source) else {
            break;
        };
        let clone = node_id_map
            .iter()
            .find(|(old, _)| old == source)
            .and_then(|(_, new)| owners.iter().find(|owner| &owner.id == new));
        let renamed = cloned_scene_section(
            Some(section),
            owner.handle.as_deref(),
            clone.and_then(|owner| owner.handle.as_deref()),
        );
        if renamed.as_deref() != Some(section) {
            return renamed;
        }
        current = owner.parent.as_ref();
    }
    Some(section.to_owned())
}

/// Replace the source owner handle while retaining generated suffixes such as
/// ` — Transform` or ` Fluid Role`. Sections outside that owner stay unchanged.
fn cloned_scene_section(
    section: Option<&str>,
    source_handle: Option<&str>,
    clone_handle: Option<&str>,
) -> Option<String> {
    let section = section?;
    let (Some(source_handle), Some(clone_handle)) = (source_handle, clone_handle) else {
        return Some(section.to_string());
    };
    if section == source_handle {
        return Some(clone_handle.to_string());
    }
    section
        .strip_prefix(source_handle)
        .filter(|suffix| suffix.starts_with(' '))
        .map(|suffix| format!("{clone_handle}{suffix}"))
        .or_else(|| Some(section.to_string()))
}
