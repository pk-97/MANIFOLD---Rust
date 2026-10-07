//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn whitewater_influence(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let bytes = x.bytes("values").unwrap_or(0);
    x.covers("solid", bytes)?;
    x.covers("source", bytes * 4)?;
    x.covers("out", bytes)
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_influence", check: whitewater_influence }
}
