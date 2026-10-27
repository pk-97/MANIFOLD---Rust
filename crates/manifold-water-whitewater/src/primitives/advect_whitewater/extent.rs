//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{field_reads, tile_pool, whole};
use manifold_water_liquid::fluid_particles::FaceSample;
use std::mem::size_of;

fn advect_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    let steps = u64::from(whole(x, "substep_count", 0.0));
    if steps > 0 {
        x.covers("substep_schedule", steps * 16)?;
        x.covers("substep_faces", steps * elements * 16)?;
    }
    field_reads(x)?;
    x.covers("faces", elements * size_of::<FaceSample>() as u64)?;
    x.covers("solid", elements * 4)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.advect_whitewater", check: advect_whitewater }
}
