//! Buffer extent rule owned by this node.
use crate::water::primitives::particle_volume::refined_nodes;
use crate::water::primitives::particle_volume::volume_scale;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{brick_schedule, lattice_total, nodes_total, required_blob_bounds, searched};

fn particle_volume(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    let refined = refined_nodes(nodes, volume_scale(x.params()));
    for (port, n) in ["volume_nodes_x", "volume_nodes_y", "volume_nodes_z"].into_iter().zip(refined) {
        x.publish(port, n as f32);
    }
    required_blob_bounds(x)?;
    searched(x)?;
    brick_schedule(x, refined)?;
    if x.wired("interior") {
        let bytes = x.bytes("interior").unwrap_or(0);
        if !bytes.is_multiple_of(4) || crate::water::liquid::lattice::interior_cells(nodes.map(|n| n as u32), bytes / 4).is_none() {
            return Err(x.uncovered("interior must hold exactly the native or solver physical cell count".into()));
        }
        x.covers("interior", bytes)?;
    }
    x.covers("solid", nodes_total(nodes) * 4)?;
    x.covers("levelset", lattice_total(refined) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.particle_volume", check: particle_volume }
}
