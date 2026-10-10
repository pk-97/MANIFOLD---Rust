//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{ExtentRule, in_place};

inventory::submit! {
    ExtentRule { type_id: "node.zero_array", check: in_place }
}
