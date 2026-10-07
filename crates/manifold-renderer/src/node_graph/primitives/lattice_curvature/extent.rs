//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, KNOWN_VALUE, Verdict, whitewater_grid};

fn lattice_curvature(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("out", cells * KNOWN_VALUE)
}
inventory::submit! {
    ExtentRule { type_id: "node.lattice_curvature", check: lattice_curvature }
}
