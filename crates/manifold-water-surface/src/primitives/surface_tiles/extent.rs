//! Buffer extent rule owned by this node.
use super::{Layout, REACH, START_POOL_TILES, display_cell_size};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::table_words;
use manifold_water_liquid::world_tiles::held_bytes;

fn surface_tiles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cell_size = display_cell_size(
        x.scalar("sim_cell_size", 0.0625),
        x.param("resolution_scale", 1.0),
    )
    .map_err(Verdict::Refused)?;
    // The start pool; it grows at run time with the water.
    let pool = START_POOL_TILES;
    let slots = |port: &str| -> Result<u32, Verdict> {
        u32::try_from(x.items(port).unwrap_or(0))
            .map_err(|_| x.uncovered(format!("{port} holds more records than a pass indexes")))
    };
    if !x.wired("particles") {
        return Err(x.uncovered("particles is unbound".into()));
    }
    let layout = Layout {
        particles: slots("particles")?,
        pool_tiles: pool,
        interior: x
            .items("interior")
            .and_then(|items| super::super::interior_fill::interior_pool(4 * items))
            .map_or(0, |pool| pool.saturating_mul(8)),
    };
    x.provide("tiles", table_words(pool) * 4);
    x.provide("extent", 32);
    x.publish("pool_tiles", pool as f32);
    x.publish("cell_size", cell_size);
    // The table with its lists and build scratch, then the node's own records.
    x.hold(held_bytes(pool, REACH.ring_max) + layout.bytes());
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_tiles", check: surface_tiles }
}
