//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn foam_cover(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    // `run` owns the cover and the fixed-point scratch, one u32 per element
    // each.
    let cover = elements * 4;
    x.provide("cover", cover);
    x.hold(2 * cover);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.foam_cover", check: foam_cover }
}
