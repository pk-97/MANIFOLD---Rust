//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, whitewater_grid};

fn whitewater_type(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("cells", cells * 4)?;
    x.covers("out", x.bytes("spawns").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_type", check: whitewater_type }
}
