//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn spray_mist(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    // `run` owns the mist and the fixed-point scratch, one u32 per element
    // each.
    let mist = elements * 4;
    x.provide("mist", mist);
    x.hold(2 * mist);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.spray_mist", check: spray_mist }
}
