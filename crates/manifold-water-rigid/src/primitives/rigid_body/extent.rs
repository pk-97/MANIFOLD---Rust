//! Buffer extent rule owned by this node: its copies instances output.
use crate::physics::MAX_COPIES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::mesh::InstanceTransform;
use std::mem::size_of;

fn rigid_body(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers_if_bound(
        "instances",
        MAX_COPIES as u64 * size_of::<InstanceTransform>() as u64,
    )
}
inventory::submit! {
    ExtentRule { type_id: "node.rigid_body", check: rigid_body }
}
