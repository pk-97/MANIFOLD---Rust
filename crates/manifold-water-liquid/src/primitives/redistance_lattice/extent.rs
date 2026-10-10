//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::extent::nodes_total;
use crate::primitives::offset_lattice::extent::offset_lattice;

fn redistance_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    offset_lattice(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.redistance_lattice", check: redistance_lattice }
}
