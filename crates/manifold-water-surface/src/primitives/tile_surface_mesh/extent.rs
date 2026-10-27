//! Buffer extent rule owned by this node.
use super::{COUNTS_BYTES, START_TRIANGLES, VERTEX_BYTES};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn tile_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("levelset", elements * 4)?;
    // Provided and grown at run time from the surface's own count.
    let start = START_TRIANGLES * 3 * VERTEX_BYTES;
    x.provide("vertices", start);
    x.hold(start + COUNTS_BYTES);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.tile_surface_mesh", check: tile_surface_mesh }
}
