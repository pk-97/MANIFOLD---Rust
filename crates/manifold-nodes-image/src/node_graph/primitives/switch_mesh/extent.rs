//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

/// The copy reaches at most the chosen input's size, and `out` holds every
/// wired input among `ports`.
pub(in super::super) fn inputs_fit(x: &mut AtomExtent<'_>, ports: &[&str]) -> Result<(), Verdict> {
    for port in ports {
        if let Some(bytes) = x.bytes(port) {
            x.covers_if_bound("out", bytes)?;
        }
    }
    Ok(())
}

/// [`inputs_fit`] over this switch's two inputs.
pub(in super::super) fn wired_inputs_fit(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    inputs_fit(x, &super::SWITCH_INPUTS)
}

inventory::submit! {
    ExtentRule { type_id: "node.switch_mesh", check: wired_inputs_fit }
}
