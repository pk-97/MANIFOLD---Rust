//! Buffer extent rule owned by this node.
use crate::node_graph::matter::grid_bytes;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, field_reads, node_extent};

fn matter_body_reaction(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    field_reads(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_body_reaction", check: matter_body_reaction }
}
