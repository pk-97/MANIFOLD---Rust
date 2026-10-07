//! Buffer extent rule owned by this node.
use manifold_node_engine::water::liquid::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.bake_environment", check: texture_only }
}
