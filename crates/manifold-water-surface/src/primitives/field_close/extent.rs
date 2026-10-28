//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn field_close(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("levelset", elements * 4)?;
    // `run` owns the closed field and its scratch.
    x.provide("field", elements * 4);
    x.hold(2 * elements * 4);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.field_close", check: field_close }
}
