//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{particle_values, whitewater_pool};

fn dust_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    x.covers("solid", elements * 4)?;
    x.covers("source", elements * 16)?;
    x.covers("turbulence", elements * 4)?;
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.dust_potential", check: dust_potential }
}
