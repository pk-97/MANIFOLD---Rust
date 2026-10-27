//! Buffer extent rule owned by this node.
use crate::extent::tile_pool;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

pub(crate) fn field_smooth_blend(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    for port in ["smoothed", "prior", "weights", "reference", "field"] {
        x.covers(port, elements * 4)?;
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.field_smooth_blend", check: field_smooth_blend }
}
