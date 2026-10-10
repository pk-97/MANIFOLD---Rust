//! Buffer extent rule owned by this node.
use std::mem::size_of;
use manifold_water_liquid::fluid_particles::FluidBlob;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::searched;

fn shape_particle_blobs(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    searched(x)?;
    // One blob per sorted slot.
    let slots = x.items("sorted").unwrap_or(0);
    x.covers("blobs", slots * size_of::<FluidBlob>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.shape_particle_blobs", check: shape_particle_blobs }
}
