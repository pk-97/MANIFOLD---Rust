//! Buffer extent rule owned by this node.
use crate::extent::tile_pool;
use manifold_node_engine::exec::extent::ExtentRule;

inventory::submit! {
    ExtentRule { type_id: "node.smooth_lattice", check: |x| {
        let elements = tile_pool(x)?;
        for port in ["levelset", "smoothed"] {
            x.covers(port, elements * 4)?;
        }
        Ok(())
    } }
}
