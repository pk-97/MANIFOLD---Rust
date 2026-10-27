//! Buffer extent rule owned by this node.
use crate::matter::STATS_WORDS;
use crate::matter::{grid_accum_bytes, grid_bytes, pool_extent};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn matter_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let pool = pool_extent(x)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    x.covers("grid", grid_bytes(pool))?;
    x.covers("accum", grid_accum_bytes(pool))
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_stats", check: matter_stats }
}
