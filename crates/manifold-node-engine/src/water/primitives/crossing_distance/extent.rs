//! Buffer extent rule owned by this node.
use crate::water::whitewater::SURFACE_CROSSING_BYTES;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, whitewater_grid};

fn crossing_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("solid", nodes * 4)?;
    x.covers("out", cells * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.crossing_distance", check: crossing_distance }
}
