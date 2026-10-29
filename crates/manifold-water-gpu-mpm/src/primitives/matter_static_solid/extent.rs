//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::mesh::Vec4Vertex;
use std::mem::size_of;

fn matter_static_solid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One element per pooled node of the tile pool, owned by the node.
    let pool_tiles = x.count("pool_tiles", 1.0)?.max(1);
    let bytes = super::pool_len(pool_tiles) * size_of::<Vec4Vertex>() as u64;
    x.provide("solid", bytes);
    x.hold(bytes);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_static_solid", check: matter_static_solid }
}
