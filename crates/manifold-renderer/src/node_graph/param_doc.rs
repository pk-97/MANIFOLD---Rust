//! Per-parameter tooltip side-channel.
//!
//! A param's label lives on its [`ParamDef`](crate::node_graph::parameters::ParamDef),
//! but its tooltip (the plain-English help line) rides this separate
//! `inventory` channel, keyed by `(node type_id, param name)`. Same reasoning
//! as [`NodeDescriptor`](crate::node_graph::descriptor::NodeDescriptor): adding
//! a `tooltip` field to `ParamDef` would mean editing every param literal in
//! ~200 files, so the doc text lives off to the side and
//! [`catalog_gen`](crate::node_graph::catalog_gen) joins it back in by key.
//!
//! Register a node's tooltips in one block with [`param_tooltips!`]:
//!
//! ```ignore
//! param_tooltips!("node.lfo", {
//!     "rate" => "How fast it cycles. A note value like 1/4 when synced, or cycles per second when free.",
//!     "shape" => "The waveform, anything from a smooth sine to a hard square.",
//! });
//! ```
//!
//! Tooltip copy follows the house voice (natural, no em-dashes or semicolons,
//! no AI-speak). See the project memory `feedback_product_copy_voice`.

/// Tooltip for one parameter of one node, collected across the binary.
pub struct ParamDoc {
    /// The owning node's stable `type_id`.
    pub node_type_id: &'static str,
    /// The parameter's stable `name` (matches `ParamDef::name`).
    pub param: &'static str,
    /// Plain-English help text shown on hover.
    pub tooltip: &'static str,
}

inventory::collect!(ParamDoc);

/// Look up the tooltip for `node_type_id`'s `param`, if one is registered.
/// Linear scan over the inventory channel, fine for the offline doc generator.
pub fn tooltip_for(node_type_id: &str, param: &str) -> Option<&'static str> {
    inventory::iter::<ParamDoc>
        .into_iter()
        .find(|d| d.node_type_id == node_type_id && d.param == param)
        .map(|d| d.tooltip)
}

/// Register tooltips for a node's parameters in one block. Each entry is a
/// `"param_name" => "tooltip"` pair. Emits one [`ParamDoc`] per entry into the
/// inventory channel.
#[macro_export]
macro_rules! param_tooltips {
    ($type_id:literal, { $($param:literal => $tip:literal),* $(,)? }) => {
        $(
            ::inventory::submit! {
                $crate::node_graph::param_doc::ParamDoc {
                    node_type_id: $type_id,
                    param: $param,
                    tooltip: $tip,
                }
            }
        )*
    };
}
