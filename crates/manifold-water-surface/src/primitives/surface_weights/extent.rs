//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::extent::{nodes_total, searched};

fn surface_weights(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    searched(x)?;
    let node_count = nodes_total(nodes);
    x.covers("reference", node_count * 4)?;
    let particles = x
        .bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    // `run` owns its outputs and the per-particle counts, each at least 16 bytes.
    let counts = (particles / std::mem::size_of::<FluidParticle>() as u64 * 4).max(16);
    let weights = (node_count * 4).max(16);
    let velocity = (node_count * 12).max(16);
    x.provide("weights", weights);
    x.provide("velocity", velocity);
    x.hold(counts + weights + velocity);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_weights", check: surface_weights }
}
