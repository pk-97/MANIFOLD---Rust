//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

/// The copy reaches at most the chosen input's size, and `out` holds every
/// wired input.
pub(in super::super) fn wired_inputs_fit(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    for port in super::SWITCH_INPUTS {
        if let Some(bytes) = x.bytes(port) {
            x.covers_if_bound("out", bytes)?;
        }
    }
    Ok(())
}

inventory::submit! {
    ExtentRule { type_id: "node.switch_mesh", check: wired_inputs_fit }
}
