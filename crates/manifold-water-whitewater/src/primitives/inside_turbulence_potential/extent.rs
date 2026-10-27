//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{particle_values, tile_pool};

fn inside_turbulence_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    for p in ["distance", "turbulence", "cells"] {
        x.covers(p, elements * 4)?;
    }
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.inside_turbulence_potential", check: inside_turbulence_potential }
}
