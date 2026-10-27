//! Buffer extent rule owned by this node.
use crate::extent::tile_pool;
use manifold_node_engine::exec::extent::ExtentRule;

inventory::submit! {
    ExtentRule { type_id: "node.upwind_distance", check: |x| {
        let elements = tile_pool(x)?;
        for port in ["levelset","valid","out"] { x.covers(port,elements*4)?; } Ok(())
    } }
}
