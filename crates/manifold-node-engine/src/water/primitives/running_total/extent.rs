//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn running_total(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The scan runs over min(count, in, out).
    x.covers("out", x.bytes("in").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.running_total", check: running_total }
}
