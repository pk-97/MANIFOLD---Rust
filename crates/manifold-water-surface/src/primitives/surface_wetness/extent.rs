//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{tile_pool, whole};

fn surface_wetness(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("field", elements * 4)?;
    // `run` owns the wetness, one f32 per element, and last frame's slot rows,
    // four words a slot.
    let wetness = elements * 4;
    x.provide("wetness", wetness);
    x.hold(wetness + 16 * u64::from(whole(x, "pool_tiles", 1.0).max(1)));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_wetness", check: surface_wetness }
}
