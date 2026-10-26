//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::PARTICLE;
use manifold_water_liquid::primitives::liquid_stats::LIQUID_STATS_WORDS;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::whitewater::DEFAULT_CAPACITY as STEP_CAPACITY;
use manifold_water_liquid::whitewater::MAX_CAPACITY as STEP_MAX_CAPACITY;
use std::mem::size_of;

fn liquid_state(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.provide("identity", IDENTITY_BYTES);
    // The persistent identity and the three fenced metadata copies.
    x.hold(4 * IDENTITY_BYTES);
    x.covers_if_bound("identity_in", IDENTITY_BYTES)?;
    let mut whitewater_check = Ok(());
    let capacity = x.count("whitewater_capacity", STEP_CAPACITY as f32)?;
    if !(1..=STEP_MAX_CAPACITY).contains(&capacity) {
        return Err(Verdict::Refused(format!(
            "whitewater capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}"
        )));
    }
    let pool = u64::from(capacity)
        * size_of::<manifold_water_liquid::whitewater::WhitewaterParticle>() as u64;
    for (capture, output, bytes) in [
        ("whitewater_pool_in", "whitewater_pool", pool),
        ("whitewater_state_in", "whitewater_state", 32),
        ("whitewater_counts_in", "whitewater_counts", 36),
        (
            "foam_particles_in",
            "foam_particles",
            u64::from(capacity) * PARTICLE,
        ),
        (
            "bubble_particles_in",
            "bubble_particles",
            u64::from(capacity) * PARTICLE,
        ),
        (
            "spray_particles_in",
            "spray_particles",
            u64::from(capacity) * PARTICLE,
        ),
        (
            "dust_particles_in",
            "dust_particles",
            u64::from(capacity) * PARTICLE,
        ),
    ] {
        let active = x.input(capture).is_some();
        x.provide(output, if active { bytes } else { 0 });
        if active {
            x.hold(bytes);
            // Captures become bound on the second walk. Publish ALL sizes
            // before returning an uncovered capture from the first walk.
            whitewater_check = whitewater_check.and(x.covers(capture, bytes));
        }
    }
    if x.input("whitewater_pool_in").is_some() {
        x.hold(pool);
    }

    let stats = u64::from(LIQUID_STATS_WORDS) * 4;
    // The zeroed stats a new epoch copies, and the readback ring.
    x.hold(4 * stats + 3 * 32);
    x.covers_if_bound("clock_status_in", 32)?;
    x.covers_if_bound("clock_status", 32)?;
    x.publish("tick_index", 0.0);
    let records = u64::from(x.count("count", 0.0)?) * PARTICLE;
    // A new epoch copies the fill into the state; each tick's capture copies
    // the tick's particles and stats back (written later in the plan: the
    // second pass sees them).
    for port in ["seed", "out", "in"] {
        x.covers(port, records)?;
    }
    x.covers("stats", stats)?;
    x.covers("stats_in", stats)?;
    whitewater_check
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_state", check: liquid_state }
}
