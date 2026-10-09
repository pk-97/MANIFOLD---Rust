//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.coc_from_depth", check: texture_only }
}
