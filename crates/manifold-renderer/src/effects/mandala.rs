//! Mandala — kaleidoscope whose mirrored segments persist across frames.
//!
//! ```text
//! Source ──▶ KaleidoFold ──▶ Feedback ──▶ ChromaticOffset ──▶ next stage
//! ```
//!
//! Six sliders:
//! - `amount`      → KaleidoFold.amount       (gate; effect skips at 0)
//! - `segments`    → KaleidoFold.segments     (2..16, int)
//! - `persistence` → Feedback.amount          (how strongly previous frames linger)
//! - `zoom`        → Feedback.zoom            (per-frame scale of the feedback buffer)
//! - `drift`       → Feedback.rotation        (deg/frame, symmetric ±10 for clean Ableton loop)
//! - `spectrum`    → ChromaticOffset.amount   (RGB split on the trailing ghosts)

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::MANDALA,
        display_name: "Mandala",
        category: "Stylize",
        available: true,
        osc_prefix: "mandala",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("amount",      "Amount",      0.0,  1.0, 1.0, "F2", ""),
            ParamSpec::whole     ("segments",    "Segments",    2.0, 16.0, 6.0,       ""),
            ParamSpec::continuous("persistence", "Persistence", 0.0,  1.0, 0.7, "F2", ""),
            ParamSpec::continuous("zoom",        "Zoom",        0.9,  1.1, 0.99,"F3", ""),
            ParamSpec::continuous("drift",       "Drift",     -10.0, 10.0, 1.5, "F1", "°/f"),
            ParamSpec::continuous("spectrum",    "Spectrum",    0.0,  1.0, 0.4, "F2", ""),
        ],
    }
}

