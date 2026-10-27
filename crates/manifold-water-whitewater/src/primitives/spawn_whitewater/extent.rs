//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{PARTICLE, tile_pool, whole};
use manifold_water_liquid::fluid_particles::{FaceSample, WhitewaterSpawn};
use manifold_water_liquid::whitewater::DEFAULT_CAPACITY;
use std::mem::size_of;

fn spawn_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("faces", elements * size_of::<FaceSample>() as u64)?;
    x.covers("solid", elements * 4)?;
    if x.wired("emitters") {
        let emitters = u64::from(x.count("emitters", 0.0)?);
        x.covers("particles", emitters * PARTICLE)?;
        x.covers("energy", emitters * 4)?;
        x.covers("offsets", emitters * 4)?;
    }
    let slots = u64::from(whole(x, "capacity", DEFAULT_CAPACITY as f32));
    x.covers("out", slots * size_of::<WhitewaterSpawn>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.spawn_whitewater", check: spawn_whitewater }
}
