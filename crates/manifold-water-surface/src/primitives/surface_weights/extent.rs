//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::extent::tile_pool;
use manifold_water_liquid::fluid_particles::CellRange;

fn surface_weights(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers(
        "cell_ranges",
        elements * std::mem::size_of::<CellRange>() as u64,
    )?;
    x.covers("reference", elements * 4)?;
    let particles = x
        .bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    // `run` owns its outputs and the per-particle counts, each at least 16 bytes.
    let counts = (particles / std::mem::size_of::<FluidParticle>() as u64 * 4).max(16);
    let weights = (elements * 4).max(16);
    let velocity = (elements * 12).max(16);
    x.provide("weights", weights);
    x.provide("velocity", velocity);
    x.hold(counts + weights + velocity);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_weights", check: surface_weights }
}
