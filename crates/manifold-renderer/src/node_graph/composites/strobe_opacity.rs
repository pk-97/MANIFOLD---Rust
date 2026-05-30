//! [`build_strobe_opacity`] — Strobe (Opacity mode) as a primitive graph.
//!
//! Decomposition of the legacy Strobe Opacity branch:
//! `Source → Gain(gain = 1 - BeatGate) → out`. Validates the §12.6
//! worked example end-to-end — that a fused Strobe shader can be
//! expressed as a graph of small primitives with **pixel-exact
//! parity**. The parity holds because the gate signal flows on a
//! `Scalar(F32)` wire, which carries f32 end-to-end (no fp16
//! intermediate texture quantises the gate value).
//!
//! Only the Opacity branch is decomposed here. White-mode and
//! Gain-mode need either a `ConstantColor` primitive (for the Mix-to-
//! white path) or a different scalar-shaping chain (for the 1+2×gate
//! brightening). Both follow naturally from this pattern; deferred
//! until the V0 proof-point lands.

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::{BeatGate, Gain, Math, Value};
use crate::node_graph::validation::GraphError;

pub const STROBE_OPACITY_TYPE_ID: &str = "composite.strobe_opacity";

/// Build a decomposed Strobe-Opacity sub-graph rooted at `source`.
/// Returns a [`CompositeHandle`] whose output is the gain-modulated
/// texture and which exposes the inner `BeatGate.rate`, `amount`,
/// `duty`, and `phase` params for outer-card surfacing.
pub fn build_strobe_opacity(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let gate = graph.add_node(Box::new(BeatGate::new()));
    let one = graph.add_node(Box::new(Value::new()));
    let invert = graph.add_node(Box::new(Math::new()));
    let gain = graph.add_node(Box::new(Gain::new()));

    // `one` produces a constant 1.0. `invert` computes `1.0 - gate` so
    // the gain goes to 0 when the gate is on (image darkens) and to 1
    // when off (image passes through). Math defaults to Multiply, so
    // override to Subtract.
    graph.set_param(one, "value", ParamValue::Float(1.0))?;
    graph.set_param(invert, "op", ParamValue::Enum(1))?; // 1 = Subtract

    graph.connect((one, "out"), (invert, "a"))?;
    graph.connect((gate, "out"), (invert, "b"))?;
    graph.connect(source, (gain, "in"))?;
    graph.connect((invert, "out"), (gain, "gain"))?;

    let mut handle = CompositeHandle::new(STROBE_OPACITY_TYPE_ID, (gain, "out"));
    handle.add_inner(gate);
    handle.add_inner(one);
    handle.add_inner(invert);
    handle.add_inner(gain);
    handle.expose_param("rate", gate, "rate");
    handle.expose_param("amount", gate, "amount");
    handle.expose_param("duty", gate, "duty");
    handle.expose_param("phase", gate, "phase");
    Ok(handle)
}
