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

use std::borrow::Cow;

use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::primitives::{ChromaticOffset, Feedback, KaleidoFold};
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamBinding, ParamConvert, ParamTarget, SkipMode,
    SpliceResult,
};

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

fn splice_mandala(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let kaleido = graph.add_node(Box::new(KaleidoFold::new()));
    graph
        .connect(source, (kaleido, "in"))
        .expect("wire source → KaleidoFold.in");

    let feedback = graph.add_node(Box::new(Feedback::new()));
    graph
        .connect((kaleido, "out"), (feedback, "source"))
        .expect("wire KaleidoFold.out → Feedback.source");

    let chroma = graph.add_node(Box::new(ChromaticOffset::new()));
    graph
        .connect((feedback, "out"), (chroma, "in"))
        .expect("wire Feedback.out → ChromaticOffset.in");

    SpliceResult {
        output: (chroma, "out"),
        handles: vec![
            (Cow::Borrowed("kaleidoscope"), kaleido),
            (Cow::Borrowed("feedback"), feedback),
            (Cow::Borrowed("chromatic"), chroma),
        ],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::MANDALA,
        splice: splice_mandala,
        bindings: &[
            ParamBinding {
                id: Cow::Borrowed("amount"),
                label: "Amount",
                default_value: 1.0,
                target: ParamTarget::HandleNode { handle: "kaleidoscope", param: "amount" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("segments"),
                label: "Segments",
                default_value: 6.0,
                target: ParamTarget::HandleNode { handle: "kaleidoscope", param: "segments" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("persistence"),
                label: "Persistence",
                default_value: 0.7,
                target: ParamTarget::HandleNode { handle: "feedback", param: "amount" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("zoom"),
                label: "Zoom",
                default_value: 0.99,
                target: ParamTarget::HandleNode { handle: "feedback", param: "zoom" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("drift"),
                label: "Drift",
                default_value: 1.5,
                target: ParamTarget::HandleNode { handle: "feedback", param: "rotation" },
                convert: ParamConvert::Float,
            },
            ParamBinding {
                id: Cow::Borrowed("spectrum"),
                label: "Spectrum",
                default_value: 0.4,
                target: ParamTarget::HandleNode { handle: "chromatic", param: "amount" },
                convert: ParamConvert::Float,
            },
        ],
        // Skip when the kaleidoscope gate is off. Feedback state is held
        // by the runtime StateStore and survives a skip-on / skip-off
        // cycle on the same instance.
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}
