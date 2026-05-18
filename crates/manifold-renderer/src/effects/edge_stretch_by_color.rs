//! Edge Stretch By Colour — first demo of the Phase A mask primitives.
//!
//! ```text
//! Source ─┬─────────────────────────────────────┬─→ MaskedMix.a    (untouched)
//!         ├─→ ChromaKey ──────────────────────────→ MaskedMix.mask (where to apply)
//!         └─→ ClampStretch ───────────────────────→ MaskedMix.b    (effect)
//!                                                 MaskedMix.out → next stage
//! ```
//!
//! ChromaKey produces a mask isolating pixels close to a target colour
//! (red by default — the colour lives inside the graph, editable from
//! the graph canvas). ClampStretch produces the stretched version of
//! the whole frame. MaskedMix uses the chroma mask to blend the
//! stretched version on top of the original — so the stretch is only
//! visible where the colour matches.
//!
//! Outer-card sliders:
//! - `amount`     → MaskedMix.amount (gate; effect skips at 0)
//! - `tolerance`  → ChromaKey.tolerance (how wide the colour band is)
//! - `softness`   → ChromaKey.softness (mask edge falloff)
//! - `stretch`    → ClampStretch.source_width (effect intensity)
//!
//! The key colour (default red) and stretch direction (default
//! horizontal) live inside the graph — users edit them from the
//! graph canvas, not the effect card. That's the intended pattern
//! for advanced composition: card = curated performance surface,
//! canvas = full control.

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::EDGE_STRETCH_BY_COLOR,
        display_name: "Edge Stretch By Colour",
        category: "Stylize",
        available: true,
        osc_prefix: "edge_stretch_by_color",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount",    "Amount",    0.0, 1.0, 1.0, "F2", ""),
            ParamSpec::continuous("tolerance", "Tolerance", 0.0, 1.0, 0.3, "F2", ""),
            ParamSpec::continuous("softness",  "Softness",  0.0, 1.0, 0.1, "F2", ""),
            ParamSpec::continuous("stretch",   "Stretch",   0.1, 0.9, 0.5, "F2", ""),
        ],
    }
}

