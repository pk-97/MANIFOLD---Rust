//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::wet_map::map_words;
use manifold_water_liquid::extent::tile_pool;

fn wet_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("solid", elements * 4)?;
    // `run` owns the map: header, merge hash and two halves of decals.
    let map = map_words() * 4;
    x.provide("map", map);
    x.hold(map);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.wet_map", check: wet_map }
}
