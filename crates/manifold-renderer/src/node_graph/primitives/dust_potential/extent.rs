//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, particle_values, whitewater_grid};

fn dust_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("solid", nodes * 4)?;
    x.covers("source", nodes * 16)?;
    x.covers("turbulence", cells * 4)?;
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.dust_potential", check: dust_potential }
}
