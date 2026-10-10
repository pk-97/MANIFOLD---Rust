//! Buffer extent rule owned by this node.
use crate::liquid::extent::liquid_lattice;
use std::mem::size_of;
use crate::fluid_particles::CellRange;
use crate::fluid_particles::bin_total;
use crate::matter::grid_accum_bytes;
use crate::matter::lattice_blocks;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::{whole};
use crate::matter::node_extent;

fn matter_to_grid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = liquid_lattice(x)?;
    node_extent(x, &lattice)?;
    x.covers("accum", grid_accum_bytes(lattice.nodes()))?;
    if x.wired("order") && x.wired("ranges") {
        let blocks = ["blocks_x", "blocks_y", "blocks_z"].map(|name| whole(x, name, 18.0).max(1));
        if blocks != lattice_blocks(&lattice) {
            return Err(x.uncovered(format!("blocks {blocks:?} are not the lattice's {:?}", lattice_blocks(&lattice))));
        }
        x.covers("ranges", bin_total(blocks) * size_of::<CellRange>() as u64)?;
        // Active points never exceed the points array; the order holds one
        // entry per point slot.
        x.covers("order", x.items("points").unwrap_or(0) * 4)?;
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_to_grid", check: matter_to_grid }
}
