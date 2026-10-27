//! Buffer extent rule owned by this node.
use crate::matter::MATTER_REACH;
use crate::matter::grid_accum_bytes;
use crate::matter::pool_extent;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::table_words;
use manifold_water_liquid::primitives::sort_particles_into_cells::{block_keys, key_storage_bytes};
use manifold_water_liquid::world_tiles::held_bytes;

fn matter_to_grid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let pool = pool_extent(x)?;
    x.covers("accum", grid_accum_bytes(pool))?;
    // The node provides the world tile table, and holds the table's build
    // scratch, the sort's ranges and scratch, and one order entry a point slot.
    x.provide("tiles", table_words(pool) * 4);
    let order_bytes = x.items("points").unwrap_or(0) * 4;
    x.hold(
        held_bytes(pool, MATTER_REACH.ring_max) + key_storage_bytes(block_keys(pool)) + order_bytes,
    );
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_to_grid", check: matter_to_grid }
}
