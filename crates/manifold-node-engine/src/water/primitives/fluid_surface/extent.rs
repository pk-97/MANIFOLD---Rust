//! Buffer extent rule owned by this node.
use std::mem::size_of;
#[cfg(feature = "gpu-proofs")]
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
#[cfg(feature = "gpu-proofs")]
use crate::water::primitives::matter_domain::fill_region;
#[cfg(feature = "gpu-proofs")]
use crate::water::primitives::matter_fill::fill_cells;
use crate::mesh::MeshVertex;
use crate::water::fluid_particles::FluidParticle;
#[cfg(feature = "gpu-proofs")]
use crate::water::primitives::fluid_surface::boundary_collisions;
#[cfg(feature = "gpu-proofs")]
use crate::water::primitives::fluid_surface::fluid_settings;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, lattice_total};

fn fluid_surface(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let faces = boundary_collisions(x.params()).map_err(Verdict::Refused)?;
    let settings = fluid_settings(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        x.transform("initial_volume"),
        faces,
        0,
    );
    let layout = settings.domain_layout().map_err(Verdict::Refused)?;
    layout.admit_flip_grid(x.param("grid_budget_mcells", 8.0)).map_err(Verdict::Refused)?;
    settings.validate().map_err(Verdict::Refused)?;
    let (bounds, nodes) = layout.solid_lattice();
    // The ring grows a slot to each frame it captures, so the published
    // count never exceeds its slot. The walk takes the fill FLIP seeds, eight
    // per cell; emission grows it at run time.
    let (pool, column) = fill_region(&layout, settings.fill_height, settings.initial_volume)
        .map_err(|error| x.uncovered(format!("the fill model disagrees with FLIP's settings: {error}")))?;
    let count = fill_cells(layout.cells, pool, column) * 8;
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    x.publish_transform("grid_bounds", bounds);
    for (port, n) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(nodes) {
        x.publish(port, n as f32);
    }
    let particles = count.max(1) * size_of::<FluidParticle>() as u64;
    let solid = lattice_total(nodes) * 4;
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.provide("solid_a", solid);
    x.provide("solid_b", solid);
    x.hold(crate::water::fluid::particle_ring::RING_SLOTS as u64 * (particles + solid));
    // Tick-zero empty storage stays independent of worker-owned ring slots.
    // Include it in peak admission while the first real frame is prepared.
    x.hold(size_of::<FluidParticle>() as u64 + solid);
    // The CPU mesh grows its buffer by half again when a surface needs more,
    // through device admission; whitewater uploads stop at their capacity.
    let vertices = u64::from((x.param("max_capacity", 786_432.0).clamp(3.0, 3_145_728.0) as u32 / 3) * 3) * size_of::<MeshVertex>() as u64;
    x.provide("vertices", vertices);
    x.hold(vertices);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: FLIP_DOMAIN_TYPE_ID, check: fluid_surface }
}
