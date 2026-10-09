//! Buffer extent rule owned by this node.
use crate::water::primitives::particle_volume::volume_scale;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{nodes_total, required_blob_bounds, searched};

fn lattice_bricks(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    use crate::water::primitives::{lattice_bricks::brick_layout, prefix_scan::storage_words};
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    let layout = brick_layout(nodes.map(|n| n as u32), volume_scale(x.params()))
        .ok_or_else(|| x.uncovered("brick lattice cannot be indexed in u32".into()))?;
    searched(x)?;
    x.covers("solid", nodes_total(nodes) * 4)?;
    let bytes = u64::from(layout.words) * 4;
    x.provide("bricks", bytes);
    // Bricks, the scan, and one scatter hit word per brick.
    let hits = (u64::from(layout.count) * 4).max(16);
    x.hold(bytes + storage_words(layout.count as usize) as u64 * 4 + hits);
    required_blob_bounds(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.lattice_bricks", check: lattice_bricks }
}
