//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn whitewater_type(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("cells", elements * 4)?;
    x.covers("out", x.bytes("spawns").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_type", check: whitewater_type }
}
