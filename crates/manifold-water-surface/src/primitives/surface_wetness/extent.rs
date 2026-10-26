//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::nodes_total;

fn surface_wetness(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    let node_count = nodes_total(nodes);
    x.covers("field", node_count * 4)?;
    // `run` owns the wetness, one f32 per node, at least 16 bytes.
    let wetness = (node_count * 4).max(16);
    x.provide("wetness", wetness);
    x.hold(wetness);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_wetness", check: surface_wetness }
}
