//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;
use manifold_water_liquid::fluid_particles::FaceSample;

fn turbulence_field(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("faces", elements * std::mem::size_of::<FaceSample>() as u64)?;
    x.covers("out", elements * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.turbulence_field", check: turbulence_field }
}
