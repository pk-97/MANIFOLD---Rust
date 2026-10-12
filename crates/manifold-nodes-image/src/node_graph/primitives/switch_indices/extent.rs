//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::ExtentRule;

use super::super::switch_mesh::extent::wired_inputs_fit;

inventory::submit! {
    ExtentRule { type_id: "node.switch_indices", check: wired_inputs_fit }
}
