//! Buffer extent rule owned by this node.
use crate::primitives::emission_count::extent::emission_count;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn turbulence_emission_count(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    emission_count(x)?;
    x.covers("turbulence", x.items("particles").unwrap_or(0) * 4)?;
    let elements = tile_pool(x)?;
    x.covers("influence", elements * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.turbulence_emission_count", check: turbulence_emission_count }
}
