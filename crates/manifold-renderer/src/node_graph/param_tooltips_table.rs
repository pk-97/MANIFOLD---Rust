//! Per-parameter tooltips for the node library.
//!
//! Tooltips are keyed by `(type_id, param name)` and submitted through the
//! [`param_doc`](crate::node_graph::param_doc) inventory channel, so they can
//! live here in one place rather than scattered across ~200 node files. The
//! generator joins them back to each param by key.
//!
//! Voice: natural, readable, no em-dashes or semicolons, no AI-speak. Say what
//! the knob does and the one gotcha that matters. See the project memory
//! `feedback_product_copy_voice`.
//!
//! Organised by category to match the catalog. Nodes with a single obvious
//! param, or with params already self-explanatory from the label, can be left
//! out; this table fills the knobs that benefit from a line of help.

// ─── Color & Tone ────────────────────────────────────────────────────

crate::param_tooltips!("node.gain", {
    "gain" => "Multiplies the brightness. Above 1 brightens, below 1 darkens, and 0 is black.",
});

crate::param_tooltips!("node.contrast", {
    "contrast" => "How hard to push the lights and darks apart. 1 leaves it unchanged, higher is punchier, lower is flatter.",
});

crate::param_tooltips!("node.hue_saturation", {
    "hue" => "Rotates every colour around the colour wheel, measured in degrees.",
    "saturation" => "How vivid the colours are. 0 is grey, 1 is unchanged, higher is more saturated.",
    "value" => "Overall brightness. 1 leaves it unchanged.",
});

crate::param_tooltips!("node.levels", {
    "scale" => "Multiplies the brightness before everything else. The main contrast control.",
    "offset" => "Adds or subtracts brightness, lifting or lowering the whole image.",
    "lo" => "The black point. Anything below this is clipped to black.",
    "hi" => "The white point. Anything above this is clipped to white.",
    "gamma" => "Bends the midtones. Below 1 brightens them, above 1 darkens them.",
});

crate::param_tooltips!("node.colorize", {
    "amount" => "How strongly to tint the image toward the chosen colour.",
    "hue" => "The colour to tint toward, as a position on the colour wheel in degrees.",
    "saturation" => "How saturated the tint colour is.",
    "focus" => "How tightly the tint sticks to the bright neutral areas rather than spreading everywhere.",
});

crate::param_tooltips!("node.clamp_texture", {
    "min" => "The lowest value any colour is allowed to reach. Nothing goes darker than this.",
    "max" => "The highest value any colour is allowed to reach. Nothing goes brighter than this.",
});
