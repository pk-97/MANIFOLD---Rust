use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode};

/// Give a duplicated object its own card rows through the one metadata
/// copier ([`crate::binding_transfer::transfer_bindings`]); sections that
/// name the source object are renamed for the copy. The cloned nodes are
/// already in the graph. Returns each copied row as `(source id, copy id)`.
pub(super) fn clone_scene_bindings(def: &mut EffectGraphDef, node_id_map: &[(NodeId, NodeId)]) -> Vec<(String, String)> {
    let node_handles = collect_node_handles(&def.nodes);
    let Some(meta) = def.preset_metadata.as_mut() else {
        return Vec::new();
    };
    let source = meta.clone();
    let relabel = |text: &str, owner: &NodeId| {
        cloned_owned_section(Some(text), owner, &node_handles, node_id_map).unwrap_or_else(|| text.to_string())
    };
    crate::binding_transfer::transfer_bindings(&source, meta, node_id_map, &relabel)
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
