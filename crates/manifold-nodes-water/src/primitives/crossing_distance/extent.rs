//! Buffer extent rule owned by this node.
use crate::whitewater::SURFACE_CROSSING_BYTES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::whitewater_grid;

fn crossing_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("solid", nodes * 4)?;
    x.covers("out", cells * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.crossing_distance", check: crossing_distance }
}
