//! Mesh-rule composition without exposing the installation implementation.
use super::*;
pub fn compose_region_mesh_rules(region: &Region, members: &[&RegionMember], nodes: &[Box<dyn crate::exec::effect_node::EffectNode>], alias: Option<usize>) -> Vec<PreparedMeshOutputRule> {
    super::compose_region_mesh_rules(region, members, nodes, alias)
}
