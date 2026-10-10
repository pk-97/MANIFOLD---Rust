//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::{KNOWN_VALUE, whitewater_grid};

fn lattice_curvature(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("out", cells * KNOWN_VALUE)
}
inventory::submit! {
    ExtentRule { type_id: "node.lattice_curvature", check: lattice_curvature }
}
