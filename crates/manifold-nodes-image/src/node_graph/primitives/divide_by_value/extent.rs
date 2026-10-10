//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn divide_by_value(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("divisor", 4)?;
    x.covers("out", x.bytes("values").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.divide_by_value", check: divide_by_value }
}
