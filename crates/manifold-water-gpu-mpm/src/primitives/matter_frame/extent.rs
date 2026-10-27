//! Buffer extent rule owned by this node.
use crate::matter::MatterPoint;
use crate::matter::STATS_WORDS;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::frame_ring::RING;
use std::mem::size_of;

fn matter_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let count = x.count("count", 0.0)?;
    x.covers("points", u64::from(count) * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    let particles = u64::from(count.max(1)) * size_of::<FluidParticle>() as u64;
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.hold(RING as u64 * particles);
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_frame", check: matter_frame }
}
