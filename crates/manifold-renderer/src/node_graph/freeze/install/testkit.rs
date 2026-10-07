//! Mesh-rule composition without exposing the installation implementation.
use super::*;
pub(crate) fn compose_region_mesh_rules(region: &Region, members: &[&RegionMember], nodes: &[Box<dyn crate::node_graph::EffectNode>], alias: Option<usize>) -> Vec<PreparedMeshOutputRule> {
    super::compose_region_mesh_rules(region, members, nodes, alias)
}
