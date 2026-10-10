//! Buffer extent rule owned by this node.
use crate::whitewater::SURFACE_CROSSING_BYTES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::whitewater_grid;

fn nearest_crossing(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("out", cells * SURFACE_CROSSING_BYTES)
}
inventory::submit! {
    ExtentRule { type_id: "node.nearest_crossing", check: nearest_crossing }
}
