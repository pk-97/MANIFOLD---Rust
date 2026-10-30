//! Buffer extent rule owned by this node.
use crate::matter::{MAX_MATTER_SLOTS, STATS_WORDS};
use crate::primitives::matter_domain::FILL_ROW_WORDS;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

/// The kernels read the stats' first word and one row of the fill table;
/// every point and seed read is bounded by the smaller of the two buffers and
/// every vertex by the node's own mesh buffers, sized when it binds.
fn matter_skin(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    x.covers("fills", (FILL_ROW_WORDS * MAX_MATTER_SLOTS * 4) as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_skin", check: matter_skin }
}
