//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::liquid_lattice;
use crate::water::matter::grid_bytes;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{field_reads, node_extent};

fn matter_grid_update(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = liquid_lattice(x)?;
    let nodes = node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    x.covers("accum", nodes * 16)?;
    field_reads(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_grid_update", check: matter_grid_update }
}
