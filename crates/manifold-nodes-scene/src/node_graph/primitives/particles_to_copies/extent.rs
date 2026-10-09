//! Buffer extent rule owned by this node.
use std::mem::size_of;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn particles_to_copies(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("copies", x.items("particles").unwrap_or(0) * size_of::<InstanceTransform>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.particles_to_copies", check: particles_to_copies }
}
