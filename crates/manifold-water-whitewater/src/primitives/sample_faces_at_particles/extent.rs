//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{particle_map, tile_pool};
use manifold_water_liquid::fluid_particles::FaceSample;
use std::mem::size_of;

fn sample_faces_at_particles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("faces", elements * size_of::<FaceSample>() as u64)?;
    particle_map(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.sample_faces_at_particles", check: sample_faces_at_particles }
}
