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

// ─── Distort & Warp ──────────────────────────────────────────────────

crate::param_tooltips!("node.chromatic_displace", {
    "amount" => "How far the red and blue channels pull apart, in pixels. Negative values swap which way they shift.",
});

crate::param_tooltips!("node.mirror_axis", {
    "angle" => "Rotates the mirror line. Set it for a horizontal, vertical, or diagonal flip.",
});

crate::param_tooltips!("node.radial_fold_uv", {
    "segments" => "How many mirrored wedges to fold the image into. More wedges give a finer pattern.",
    "cx" => "Horizontal center of the fold. 0 is the left edge, 1 is the right.",
    "cy" => "Vertical center of the fold. 0 is the top, 1 is the bottom.",
});

// ─── Blur & Sharpen ──────────────────────────────────────────────────

crate::param_tooltips!("node.gaussian_blur", {
    "axis" => "Which direction this pass blurs, horizontal or vertical. Pair the two for a full blur.",
    "radius" => "How far the blur reaches, in pixels.",
    "kernel_size" => "How many samples the blur uses. More is smoother but slower.",
    "address_mode" => "What the blur does at the edges of the frame, clamp or repeat.",
});

crate::param_tooltips!("node.sharpen", {
    "amount" => "How much to sharpen. 0 passes through untouched, higher makes edges crisper.",
});

// ─── Stylize ─────────────────────────────────────────────────────────

crate::param_tooltips!("node.film_grain", {
    "amount" => "How heavy the grain is. Low adds subtle texture, high adds strong noise.",
});

crate::param_tooltips!("node.dither", {
    "amount" => "How few brightness levels to crush the image into. Lower gives a harder, more banded look.",
});

crate::param_tooltips!("node.flash", {
    "amount" => "How strong the flash is. Wire a beat gate or envelope here for strobes and hits.",
    "mode" => "Which way it flashes, toward black, toward white, or a brightness boost.",
});

crate::param_tooltips!("node.vignette", {
    "shape" => "The shape of the darkened border, circle, oval, or rectangle.",
    "size" => "How far in from the edges the darkening starts.",
    "softness" => "How gradually the darkening fades in.",
    "strength" => "How dark the border gets.",
});

// ─── Composite ───────────────────────────────────────────────────────

crate::param_tooltips!("node.mix", {
    "amount" => "How much to crossfade from the first image to the second.",
    "mode" => "How the two images combine, like Add, Screen, Multiply, or Overlay.",
});

crate::param_tooltips!("node.wet_dry", {
    "wet_dry" => "How much of the processed image to show over the original. 0 is dry, 1 is fully wet.",
});

// ─── Mask ────────────────────────────────────────────────────────────

crate::param_tooltips!("node.box_mask", {
    "cx" => "Horizontal center of the rectangle. 0 is the left edge, 1 is the right.",
    "cy" => "Vertical center. 0 is the top, 1 is the bottom.",
    "half_width" => "How far the rectangle reaches from its center to the left and right. At 0.5 it fills the frame.",
    "half_height" => "How far it reaches up and down from the center.",
    "rotation" => "Spins the rectangle around its center. Wire a knob or LFO to turn it over time.",
    "softness" => "Fades the edge of the mask. 0 gives a clean hard edge, higher values blur it out.",
});

crate::param_tooltips!("node.posterize", {
    "levels" => "How many brightness steps each colour gets crushed into. Fewer levels give a chunkier look.",
});

crate::param_tooltips!("node.voronoi_2d", {
    "scale" => "How many cells fill the frame. Higher values pack in more, smaller cells.",
    "offset_x" => "Slides the whole cell pattern sideways. Wire an LFO here and the cells drift.",
    "offset_y" => "Slides the cell pattern up and down.",
    "jitter" => "How irregular the cells are. At 0 you get a perfect grid, at 1 they scatter into random shapes.",
    "out_scale" => "Brightens or dims the result, handy for pushing the pattern into a usable range.",
});

// ─── Control ─────────────────────────────────────────────────────────

crate::param_tooltips!("node.math", {
    "a" => "The first value in.",
    "b" => "The second value in.",
    "op" => "What to do with the two values, like add, multiply, min, or max.",
});

crate::param_tooltips!("node.value", {
    "value" => "The constant number this outputs. Set it by hand or expose it to drive from outside.",
});

crate::param_tooltips!("node.beat_gate", {
    "rate" => "How often the gate fires, as a note division of the beat.",
    "amount" => "The value output while the gate is on. Off is always 0.",
    "duty" => "How much of each cycle the gate stays on, from a short blip to nearly always on.",
    "phase" => "Shifts where in the beat the gate fires.",
});

crate::param_tooltips!("node.beat_ramp", {
    "rate" => "How many times the ramp rises and resets per beat.",
    "attack" => "How quickly the ramp climbs at the start of each cycle. Lower is a sharper snap up.",
});

crate::param_tooltips!("node.frequency_ratio", {
    "index" => "Picks a ratio from the table of musical intervals. Each step is a different X-to-Y relationship.",
});

crate::param_tooltips!("node.one_euro_filter", {
    "min_cutoff" => "How much to smooth when the signal is still. Lower is smoother but laggier at rest.",
    "beta" => "How much to ease off the smoothing as the signal moves faster. Higher keeps fast moves crisp.",
    "d_cutoff" => "Smoothing on the speed estimate itself. Usually left alone.",
});

crate::param_tooltips!("node.affine_scalar", {
    "a" => "The value coming in to rescale.",
    "scale" => "Multiplies the value. Set it negative to flip the signal.",
    "offset" => "Adds to the value after scaling, shifting its range.",
});

crate::param_tooltips!("node.smoothing", {
    "time_constant" => "How long the smoothing takes to settle, in seconds. Higher is smoother and slower to react.",
});

crate::param_tooltips!("node.envelope_decay", {
    "decay_rate" => "How fast the envelope fades after each trigger. Higher decays quicker.",
});

crate::param_tooltips!("node.envelope_follower_ar", {
    "attack" => "How fast the envelope rises when the input gets louder, in seconds.",
    "release" => "How fast it falls when the input gets quieter, in seconds.",
});

crate::param_tooltips!("node.compressor_envelope", {
    "ratio" => "How hard to duck the gain when the input is loud. Higher squeezes more.",
    "sensitivity" => "How quickly the envelope reacts to changes in the input level.",
    "target" => "The level the compressor aims to hold the signal at.",
});
