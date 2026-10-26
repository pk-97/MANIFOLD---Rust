//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

use super::super::switch_mesh::extent::inputs_fit;

fn wired_inputs_fit(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    inputs_fit(x, &super::PORT_NAMES)
}

inventory::submit! {
    ExtentRule { type_id: "node.switch_array", check: wired_inputs_fit }
}
