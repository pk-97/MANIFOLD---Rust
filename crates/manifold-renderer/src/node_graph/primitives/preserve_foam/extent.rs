//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, searched};

fn preserve_foam(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    searched(x)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.preserve_foam", check: preserve_foam }
}
