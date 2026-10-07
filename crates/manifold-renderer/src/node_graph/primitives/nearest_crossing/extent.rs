//! Buffer extent rule owned by this node.
use crate::node_graph::whitewater::SURFACE_CROSSING_BYTES;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, whitewater_grid};

fn nearest_crossing(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("out", cells * SURFACE_CROSSING_BYTES)
}
inventory::submit! {
    ExtentRule { type_id: "node.nearest_crossing", check: nearest_crossing }
}
