//! Buffer extent rule owned by this node.
use manifold_water_liquid::extent::liquid_lattice;
use std::mem::size_of;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::frame_ring::RING;
use crate::matter::MatterPoint;
use crate::matter::STATS_WORDS;
use crate::primitives::matter_face_component::MATTER_FACE_VALID_LAYERS;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{cover_frame_faces, provide_frame_faces};

fn matter_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = liquid_lattice(x)?;
    provide_frame_faces(x, lattice.cells(), MATTER_FACE_VALID_LAYERS as f32);
    let count = x.count("count", 0.0)?;
    x.covers("points", u64::from(count) * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    let particles = u64::from(count.max(1)) * size_of::<FluidParticle>() as u64;
    let solid = lattice.solid_bytes();
    if x.wired("solid") {
        x.covers("solid", solid)?;
        x.hold(RING as u64 * solid);
    } else {
        x.hold(solid);
    }
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.provide("solid_a", solid);
    x.provide("solid_b", solid);
    x.hold(RING as u64 * particles);
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    x.publish_transform("grid_bounds", lattice.bounds());
    for (port, n) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(lattice.nodes()) {
        x.publish(port, n as f32);
    }
    cover_frame_faces(x, lattice.cells())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_frame", check: matter_frame }
}
