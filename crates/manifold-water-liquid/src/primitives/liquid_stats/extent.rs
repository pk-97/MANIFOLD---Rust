//! Buffer extent rule owned by this node.
use crate::extent::PARTICLE;
use crate::primitives::liquid_stats::{ARGS_BYTES, LIQUID_STATS_WORDS, partial_bytes};
use crate::primitives::particle_identity::IDENTITY_BYTES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn liquid_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The reduction reaches the slots in use, which the particle buffer holds
    // at most; its scratch is sized for all of them.
    x.covers("particles", PARTICLE)?;
    let slots = x
        .items("particles")
        .ok_or_else(|| x.uncovered("particles is not an array".to_string()))?;
    x.covers("identity", IDENTITY_BYTES)?;
    x.covers("stats", u64::from(LIQUID_STATS_WORDS) * 4)?;
    x.hold(partial_bytes(slots.min(u64::from(u32::MAX)) as u32) + ARGS_BYTES);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_stats", check: liquid_stats }
}
