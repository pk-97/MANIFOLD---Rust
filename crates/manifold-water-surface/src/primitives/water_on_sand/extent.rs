//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::extent::tile_pool;

use super::PUSH_BYTES;

fn water_on_sand(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("levelset", elements * 4)?;
    x.covers("velocity", elements * 12)?;
    // `run` owns the push: one record a grain slot.
    let grains = x.bytes("sand").unwrap_or(0) / size_of::<FluidParticle>() as u64;
    let push = grains.max(1) * PUSH_BYTES;
    x.provide("push", push);
    x.hold(push);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.water_on_sand", check: water_on_sand }
}
