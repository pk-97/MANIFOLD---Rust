//! Soft Focus — separable Gaussian blur composited back over the original.
//!
//! ```text
//! Source ──▶ Blur ──▶ Mix.b
//! Source ───────────▶ Mix.a
//! Mix.out ─────────▶ next stage
//! ```
//!
//! Exposes:
//! - `radius` → `Blur.radius` (0..32, shader caps internally)
//! - `amount` → `Mix.amount` (0 = sharp original, 1 = full blur)

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::SOFT_FOCUS_GRAPH,
        display_name: "Soft Focus",
        category: "Stylize",
        available: true,
        osc_prefix: "soft_focus_graph",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("radius", "Radius", 0.0, 64.0, 6.0, "F1", "px"),
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

