//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::fluid_particles::MAX_BINS;
use crate::node_graph::fluid_particles::bin_counts;
use crate::node_graph::fluid_particles::bin_total;
use crate::node_graph::primitives::sort_particles_into_cells::range_storage_bytes;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, search_fits};

fn sort_particles_into_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = x.items("particles").unwrap_or(0);
    x.covers_if_bound("sorted", capacity * size_of::<FluidParticle>() as u64)?;
    x.covers_if_bound("order", capacity * 4)?;
    x.hold(2 * capacity.max(1) * 4);
    if x.scalar("enabled", 1.0) <= 0.5 {
        x.provide("cell_ranges", size_of::<CellRange>() as u64);
        return Ok(());
    }
    let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.0));
    let cell_size = x.scalar("cell_size", 0.0625);
    let center = ["center_x", "center_y", "center_z"].map(|name| x.scalar(name, 0.0));
    if !(cell_size.is_finite() && cell_size > 0.0) || center.iter().chain(&size).any(|v| !v.is_finite()) || size.iter().any(|v| *v <= 0.0) {
        return Err(x.uncovered(format!("box {center:?} {size:?} and cell size {cell_size} must be finite and positive")));
    }
    let bins = bin_counts(size, cell_size);
    if bin_total(bins) > MAX_BINS {
        return Err(Verdict::Refused(format!(
            "Sort Particles Into Cells: a {}×{}×{} bin grid is more than the {MAX_BINS} bins a search can index. Raise the cell size.",
            bins[0], bins[1], bins[2]
        )));
    }
    let range_bytes = range_storage_bytes(bins);
    search_fits(x, bins, range_bytes)?;
    x.provide("cell_ranges", range_bytes);
    x.hold(range_bytes + bin_total(bins) * 4);
    for (port, n) in ["bins_x", "bins_y", "bins_z"].into_iter().zip(bins) {
        x.publish(port, n as f32);
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.sort_particles_into_cells", check: sort_particles_into_cells }
}
