//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn blob_bounds(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("bounds", 8)
}
inventory::submit! {
    ExtentRule { type_id: "node.blob_bounds", check: blob_bounds }
}
