//! Buffer extent rule owned by this node.
use crate::matter::MATTER_REACH;
use crate::matter::grid_accum_bytes;
use crate::matter::pool_extent;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::{pool_len, table_words};
use manifold_water_liquid::fluid_particles::SAND_BULK_BYTES;
use manifold_water_liquid::primitives::sort_particles_into_cells::{block_keys, key_storage_bytes};
use manifold_water_liquid::world_tiles::{args_words, held_bytes};

fn matter_to_grid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let pool = pool_extent(x)?;
    x.covers("accum", grid_accum_bytes(pool))?;
    // The node provides the world tile table and its held list, args and
    // fresh list, and holds the table's build scratch, the sort's ranges and
    // scratch, and one order entry a point slot.
    x.provide("tiles", table_words(pool) * 4);
    x.provide("held", u64::from(pool) * 4);
    x.provide("held_args", args_words(MATTER_REACH.ring_max) * 4);
    x.provide("fresh", u64::from(pool) * 4);
    let order_bytes = x.items("points").unwrap_or(0) * 4;
    x.hold(
        held_bytes(pool, MATTER_REACH.ring_max) + key_storage_bytes(block_keys(pool)) + order_bytes,
    );
    // The bulk, while a reader takes it.
    if x.feeds("bulk") {
        let bulk = pool_len(pool) * SAND_BULK_BYTES;
        x.provide("bulk", bulk);
        x.hold(bulk);
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_to_grid", check: matter_to_grid }
}
