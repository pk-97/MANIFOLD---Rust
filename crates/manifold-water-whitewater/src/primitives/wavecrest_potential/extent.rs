//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{KNOWN_VALUE, particle_values, whitewater_pool};

fn wavecrest_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("curvature", elements * KNOWN_VALUE)?;
    x.covers("cells", elements * 4)?;
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.wavecrest_potential", check: wavecrest_potential }
}
