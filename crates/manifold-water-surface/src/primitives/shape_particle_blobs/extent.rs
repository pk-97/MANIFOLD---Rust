//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::fluid_particles::FluidBlob;
use std::mem::size_of;

fn shape_particle_blobs(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One blob and one count per sorted slot.
    let slots = x.items("sorted").unwrap_or(0);
    x.covers("blobs", slots * size_of::<FluidBlob>() as u64)?;
    x.covers("counts", slots * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.shape_particle_blobs", check: shape_particle_blobs }
}
