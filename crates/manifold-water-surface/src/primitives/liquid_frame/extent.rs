//! Buffer extent rule owned by this node.
use crate::primitives::liquid_frame::WHITEWATER_INPUTS;
use crate::primitives::liquid_frame::WHITEWATER_OUTPUTS;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_water_liquid::extent::PARTICLE;
use manifold_water_liquid::frame_history::H_MAX;
use manifold_water_liquid::primitives::liquid_stats::LIQUID_STATS_WORDS;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;

fn liquid_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The retained history at its budget: every slot admitted whole.
    const SLOTS: u64 = H_MAX as u64;
    x.covers("identity", IDENTITY_BYTES)?;
    // Every slot of the state: the history keeps room for all of them, and the
    // count outputs plan for them.
    let count = x
        .items("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    if count > u64::from(EXACT_F32_COUNT) {
        return Err(x.uncovered(format!(
            "particles holds {count} records, more than the {EXACT_F32_COUNT} a particle count carries exactly"
        )));
    }
    let count = count as u32;
    let particles = u64::from(count.max(1)) * PARTICLE;
    x.hold(
        manifold_water_liquid::primitives::particle_publication::scratch_bytes(count) + SLOTS * 16,
    );
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.hold(SLOTS * particles);
    // Each wired whitewater class is kept per slot, frame B's copy provided.
    for (input, output) in WHITEWATER_INPUTS.into_iter().zip(WHITEWATER_OUTPUTS) {
        let bytes = if x.wired(input) {
            x.bytes(input).unwrap_or(4).max(4)
        } else {
            4
        };
        x.provide(output, bytes);
        if x.wired(input) {
            x.hold(SLOTS * bytes);
        }
    }
    x.publish("presented_time", 0.0);
    x.publish("publications_skipped", 0.0);
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    x.covers("stats", u64::from(LIQUID_STATS_WORDS) * 4)?;
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_frame", check: liquid_frame }
}
