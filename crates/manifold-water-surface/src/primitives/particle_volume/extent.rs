//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;
use manifold_water_liquid::fluid_particles::CellRange;

fn particle_volume(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers(
        "cell_ranges",
        elements * std::mem::size_of::<CellRange>() as u64,
    )?;
    x.covers("solid", elements * 4)?;
    x.covers("levelset", elements * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.particle_volume", check: particle_volume }
}
