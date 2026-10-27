//! Buffer extent rule owned by this node.
use crate::matter::{grid_bytes, pool_extent};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::table_words;

fn grid_to_matter(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let pool = pool_extent(x)?;
    x.covers("tiles", table_words(pool) * 4)?;
    // Active points clamp to the points array.
    x.covers("grid", grid_bytes(pool))
}
inventory::submit! {
    ExtentRule { type_id: "node.grid_to_matter", check: grid_to_matter }
}
