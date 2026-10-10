//! Buffer extent rule owned by this node.
use crate::primitives::liquid_stats::LIQUID_STATS_WORDS;
use crate::primitives::liquid_stats::partial_bytes;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::PARTICLE;

fn liquid_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let count = x.count("count", 0.0)?;
    x.covers("particles", u64::from(count) * PARTICLE)?;
    x.covers("stats", u64::from(LIQUID_STATS_WORDS) * 4)?;
    x.hold(partial_bytes(count));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_stats", check: liquid_stats }
}
