//! Buffer extent rule owned by this node.
use crate::water::matter::grid_bytes;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, node_extent};

fn grid_to_matter(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    node_extent(x, &lattice)?;
    // Active points clamp to the points array.
    x.covers("grid", grid_bytes(lattice.nodes()))
}
inventory::submit! {
    ExtentRule { type_id: "node.grid_to_matter", check: grid_to_matter }
}
