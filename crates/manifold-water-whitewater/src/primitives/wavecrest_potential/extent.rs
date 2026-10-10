//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{KNOWN_VALUE, particle_values, whitewater_grid};

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
