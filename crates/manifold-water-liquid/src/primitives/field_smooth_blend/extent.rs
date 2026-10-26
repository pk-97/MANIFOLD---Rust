//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

pub(crate) fn field_smooth_blend(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("field", x.bytes("smoothed").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.field_smooth_blend", check: field_smooth_blend }
}
