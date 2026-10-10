//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{brick_schedule, nodes_total};

fn clamp_liquid_to_solids(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    let solid = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
    if nodes.iter().chain(&solid).any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}, solid {solid:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("clamped", nodes_total(nodes) * 4)?;
    x.covers("solid", nodes_total(solid) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.clamp_liquid_to_solids", check: clamp_liquid_to_solids }
}
