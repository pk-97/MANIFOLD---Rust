//! Compatibility markers for projects authored against the retired CPU FLIP
//! product path.
//!
//! The graph and parameter data remain part of the project file.  This module
//! only identifies content that must be reported as preserved and must never
//! be replaced with a bundled generator fallback.

use crate::effect_graph_def::{EffectGraphDef, EffectGraphNode};
use crate::PresetTypeId;

/// Product preset IDs that used the native CPU FLIP solver.
pub const RETIRED_CPU_FLIP_PRESET_IDS: &[&str] = &[
    "WaterBasin",
    "WaterDamBreak",
    "WaterDamBreakGpu",
    "HoneyDamBreak",
];

/// The node type that identifies an authored CPU FLIP graph, including when it
/// is nested inside one or more graph groups.
pub const RETIRED_CPU_FLIP_NODE_TYPE_ID: &str = crate::liquid_domain::FLIP_DOMAIN_TYPE_ID;

/// Whether `id` names a product preset removed with the CPU FLIP path.
#[must_use]
pub fn is_retired_cpu_flip_preset(id: &PresetTypeId) -> bool {
    RETIRED_CPU_FLIP_PRESET_IDS.contains(&id.as_str())
}

/// Whether a graph contains the retired CPU FLIP node at any nesting depth.
#[must_use]
pub fn graph_contains_retired_cpu_flip_node(graph: &EffectGraphDef) -> bool {
    nodes_contain_retired_cpu_flip_node(&graph.nodes)
}

/// Stable graph paths for every retired CPU FLIP node. Each path is composed
/// of `nodeId` values, with the numeric graph id as a fallback for legacy
/// nodes that predate stable node identities. Group nesting is separated by
/// `/`, making the load notice useful when several authored graphs are present.
#[must_use]
pub fn retired_cpu_flip_node_identities(graph: &EffectGraphDef) -> Vec<String> {
    let mut identities = Vec::new();
    collect_retired_node_identities(&graph.nodes, "", &mut identities);
    identities
}

/// Whether an authored graph must remain the source of truth after CPU FLIP
/// retirement.  The renderer uses this to refuse a canonical-preset fallback
/// after an authored graph fails to instantiate.
#[must_use]
pub fn preserve_authored_cpu_flip_graph(
    generator_type: &PresetTypeId,
    graph: &EffectGraphDef,
) -> bool {
    is_retired_cpu_flip_preset(generator_type)
        || graph_contains_retired_cpu_flip_node(graph)
}

fn nodes_contain_retired_cpu_flip_node(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.type_id == RETIRED_CPU_FLIP_NODE_TYPE_ID
            || node
                .group
                .as_deref()
                .is_some_and(|group| nodes_contain_retired_cpu_flip_node(&group.nodes))
    })
}

fn collect_retired_node_identities(
    nodes: &[EffectGraphNode],
    parent_path: &str,
    identities: &mut Vec<String>,
) {
    for node in nodes {
        let local_identity = if !node.node_id.is_empty() {
            node.node_id.as_str().to_string()
        } else if let Some(handle) = node.handle.as_deref() {
            handle.to_string()
        } else {
            format!("node#{}", node.id)
        };
        let path = if parent_path.is_empty() {
            local_identity
        } else {
            format!("{parent_path}/{local_identity}")
        };
        if node.type_id == RETIRED_CPU_FLIP_NODE_TYPE_ID {
            identities.push(path.clone());
        }
        if let Some(group) = node.group.as_deref() {
            collect_retired_node_identities(&group.nodes, &path, identities);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{EffectGraphNode, GroupDef, GroupInterface};
    use std::collections::{BTreeMap, BTreeSet};

    fn node(type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id: 1,
            node_id: crate::NodeId::default(),
            type_id: type_id.to_string(),
            handle: None,
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

    fn graph(nodes: Vec<EffectGraphNode>) -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes,
            wires: Vec::new(),
        }
    }

    #[test]
    fn retired_ids_and_nested_nodes_are_detected() {
        assert!(is_retired_cpu_flip_preset(&PresetTypeId::new("WaterBasin")));
        assert!(!is_retired_cpu_flip_preset(&PresetTypeId::new("WaterStillPoolMatter")));
        assert!(graph_contains_retired_cpu_flip_node(&graph(vec![node(
            RETIRED_CPU_FLIP_NODE_TYPE_ID,
        )])));

        let mut group_node = node("group");
        group_node.node_id = crate::NodeId::new("group-node");
        let mut fluid_node = node(RETIRED_CPU_FLIP_NODE_TYPE_ID);
        fluid_node.node_id = crate::NodeId::new("fluid-node");
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Vec::new(),
            },
            nodes: vec![fluid_node],
            wires: Vec::new(),
            tint: None,
        }));
        let nested = graph(vec![group_node]);
        assert!(graph_contains_retired_cpu_flip_node(&nested));
        assert_eq!(
            retired_cpu_flip_node_identities(&nested),
            vec!["group-node/fluid-node".to_string()]
        );
    }

    #[test]
    fn ordinary_graph_is_not_marked_for_preservation() {
        let ordinary = graph(vec![node("node.value")]);
        assert!(!preserve_authored_cpu_flip_graph(
            &PresetTypeId::new("Plasma"),
            &ordinary,
        ));
    }
}
