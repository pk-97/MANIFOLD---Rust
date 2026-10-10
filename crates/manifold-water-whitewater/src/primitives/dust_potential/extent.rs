//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{particle_values, whitewater_grid};

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
