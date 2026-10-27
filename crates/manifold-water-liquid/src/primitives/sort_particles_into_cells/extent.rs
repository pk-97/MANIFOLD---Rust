//! Buffer extent rule owned by this node.
use crate::extent::tile_pool;
use crate::fluid_particles::CellRange;
use crate::primitives::sort_particles_into_cells::key_storage_bytes;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::particles::FluidParticle;
use std::mem::size_of;

fn sort_particles_into_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = x.items("particles").unwrap_or(0);
    x.covers_if_bound("sorted", capacity * size_of::<FluidParticle>() as u64)?;
    x.covers_if_bound("order", capacity * 4)?;
    x.hold(2 * capacity.max(1) * 4);
    if x.scalar("enabled", 1.0) <= 0.5 {
        x.provide("cell_ranges", size_of::<CellRange>() as u64);
        return Ok(());
    }
    let cell_size = x.scalar("cell_size", 0.0625);
    if !(cell_size.is_finite() && cell_size > 0.0) {
        return Err(x.uncovered(format!("cell size {cell_size} must be finite and positive")));
    }
    // One range per pool element, and the scan's count per element.
    let elements = tile_pool(x)?;
    let range_bytes = key_storage_bytes(elements);
    x.provide("cell_ranges", range_bytes);
    x.hold(range_bytes + elements * 4);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.sort_particles_into_cells", check: sort_particles_into_cells }
}
