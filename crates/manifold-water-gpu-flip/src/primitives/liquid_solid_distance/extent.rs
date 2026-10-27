//! Buffer extent rule owned by this node.
use super::CLOCK_PLAN_BYTES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn liquid_solid_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One thread per pool element over storage sized from the same pool.
    let solid = tile_pool(x)? * 4;
    x.provide("solid", solid);
    x.hold(solid + CLOCK_PLAN_BYTES);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_solid_distance", check: liquid_solid_distance }
}
