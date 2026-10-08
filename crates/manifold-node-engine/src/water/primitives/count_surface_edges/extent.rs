//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, nodes_total};

fn count_surface_edges(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("counts", nodes_total(nodes) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.count_surface_edges", check: count_surface_edges }
}
