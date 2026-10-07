//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, KNOWN_VALUE, Verdict, particle_values, whitewater_grid};

fn wavecrest_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("curvature", cells * KNOWN_VALUE)?;
    x.covers("cells", cells * 4)?;
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.wavecrest_potential", check: wavecrest_potential }
}
