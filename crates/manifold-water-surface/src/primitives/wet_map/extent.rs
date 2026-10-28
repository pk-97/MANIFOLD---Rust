//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::wet_map::{SIDE, map_bytes};
use manifold_water_liquid::extent::tile_pool;

fn wet_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("solid", elements * 4)?;
    x.covers("extent", 32)?;
    // `run` owns the map and the clear's three group counts.
    let map = map_bytes(SIDE);
    x.provide("map", map);
    x.hold(map + 16);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.wet_map", check: wet_map }
}
