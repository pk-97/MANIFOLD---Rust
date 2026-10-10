//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::searched;

fn preserve_foam(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    searched(x)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.preserve_foam", check: preserve_foam }
}
