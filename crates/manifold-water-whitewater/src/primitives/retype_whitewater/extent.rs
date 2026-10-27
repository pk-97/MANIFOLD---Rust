//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::whitewater_pool;
use manifold_water_liquid::fluid_particles::FaceSample;
use std::mem::size_of;

fn retype_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("cells", elements * 4)?;
    x.covers("faces", elements * size_of::<FaceSample>() as u64)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.retype_whitewater", check: retype_whitewater }
}
