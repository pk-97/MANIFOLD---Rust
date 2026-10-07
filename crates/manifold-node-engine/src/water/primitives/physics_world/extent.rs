//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::mesh::InstanceTransform;
use crate::water::physics::MAX_COPIES;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn physics_world(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers_if_bound("instances", MAX_COPIES as u64 * size_of::<InstanceTransform>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.physics_world", check: physics_world }
}
