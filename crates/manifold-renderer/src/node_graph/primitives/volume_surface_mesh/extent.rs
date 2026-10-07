//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::mesh::MeshVertex;
use crate::node_graph::primitives::volume_surface_mesh::start_capacity;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, brick_schedule, nodes_total};

fn volume_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    if x.wired("solid") {
        let solid = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
        if solid.iter().any(|&n| !n.is_finite() || n < 2.0) {
            return Err(x.uncovered(format!("invalid solid lattice: {solid:?}")));
        }
        x.covers("solid", nodes_total(solid) * 4)?;
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("scan", nodes_total(nodes.map(|n| n - 1.0)) * 4)?;
    // Provided and grown at run time; cell emission checks the live scan total
    // against the buffer's whole-triangle slot count before writing.
    let slots = start_capacity(x.params(), nodes);
    let start = slots * size_of::<MeshVertex>() as u64;
    let indices = if x.wired("edge_scan") {
        x.covers("edge_scan", nodes_total(nodes) * 4)?;
        slots * 4
    } else { 0 };
    x.provide("vertices", start);
    x.provide("indices", indices);
    x.hold(start + indices + 4); // Unindexed ABI stub.
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.volume_surface_mesh", check: volume_surface_mesh }
}
