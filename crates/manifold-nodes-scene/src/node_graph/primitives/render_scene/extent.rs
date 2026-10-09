//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{ExtentRule, size_bounded};

inventory::submit! {
    ExtentRule { type_id: "node.render_scene", check: size_bounded }
}
