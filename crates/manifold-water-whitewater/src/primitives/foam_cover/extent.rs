//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::nodes_total;

fn foam_cover(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    // `run` owns the cover and the fixed-point scratch, one u32 per node each,
    // at least 16 bytes.
    let cover = (nodes_total(nodes) * 4).max(16);
    let scratch = (nodes_total(nodes) * 4).max(16);
    x.provide("cover", cover);
    x.hold(scratch + cover);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.foam_cover", check: foam_cover }
}
