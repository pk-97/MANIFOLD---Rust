//! Buffer extent rule owned by this node: each copy group's instances.
use std::mem::size_of;
use manifold_core::scene_surface::PHYSICS_WORLD_DEFAULT_COPY_GROUPS;
use manifold_node_engine::mesh::InstanceTransform;
use crate::physics::MAX_COPIES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn physics_world(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let groups = x.param("copy_groups", PHYSICS_WORLD_DEFAULT_COPY_GROUPS as f32).round().max(0.0) as usize;
    for group in 0..groups {
        x.covers_if_bound(&format!("instances_{group}"), MAX_COPIES as u64 * size_of::<InstanceTransform>() as u64)?;
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.physics_world", check: physics_world }
}
