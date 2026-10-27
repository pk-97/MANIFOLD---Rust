//! Buffer extent rule owned by this node.
use crate::extent::whitewater_pool;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn liquid_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("solid", elements * 4)?;
    x.covers("out", elements * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_cells", check: liquid_cells }
}
