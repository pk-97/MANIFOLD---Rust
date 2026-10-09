//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::whitewater_grid;
use crate::primitives::emission_count::extent::emission_count;

fn turbulence_emission_count(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    emission_count(x)?;
    x.covers("turbulence", x.items("particles").unwrap_or(0) * 4)?;
    let (nodes, _) = whitewater_grid(x)?;
    x.covers("influence", nodes * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.turbulence_emission_count", check: turbulence_emission_count }
}
