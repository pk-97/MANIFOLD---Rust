//! Buffer extent rule owned by this node.
use crate::matter::grid_bytes;
use crate::matter::node_extent;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::liquid_lattice;

fn grid_to_matter(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = liquid_lattice(x)?;
    node_extent(x, &lattice)?;
    // Active points clamp to the points array.
    x.covers("grid", grid_bytes(lattice.nodes()))
}
inventory::submit! {
    ExtentRule { type_id: "node.grid_to_matter", check: grid_to_matter }
}
