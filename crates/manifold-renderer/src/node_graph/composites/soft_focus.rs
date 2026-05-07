//! [`build_soft_focus`] — `Blur` + `Mix(source, blurred)`.
//!
//! Mixes the original source with a blurred copy of itself. Demonstrates
//! source fan-out within a composite: the outer source feeds both the
//! Blur input and Mix's "a" input.

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::primitives::{Blur, Mix};
use crate::node_graph::validation::GraphError;

pub const SOFT_FOCUS_TYPE_ID: &str = "composite.soft_focus";

/// SoftFocus = `source → Blur → Mix.b`, with `source → Mix.a` directly.
/// The Mix amount controls how much of the blurred copy is layered on
/// top: 0 = sharp original, 1 = full blur.
///
/// Inner node handles registered for V2 user-exposed parameter
/// bindings:
/// - `"blur"` → the Blur node
/// - `"mix"` → the dry/wet Mix
///
/// **Single-composite-per-graph constraint**: handle names are
/// unique within a `Graph`. Multi-composite usage will require a
/// prefix-aware variant in a future phase.
pub fn build_soft_focus(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let blur = graph.add_node_named("blur", Box::new(Blur::new()));
    let mix = graph.add_node_named("mix", Box::new(Mix::new()));

    graph.connect(source, (blur, "source"))?;
    graph.connect(source, (mix, "a"))?;
    graph.connect((blur, "out"), (mix, "b"))?;

    let mut handle = CompositeHandle::new(SOFT_FOCUS_TYPE_ID, (mix, "out"));
    handle.add_inner(blur);
    handle.add_inner(mix);
    handle.expose_param("radius", blur, "radius");
    handle.expose_param("amount", mix, "amount");
    Ok(handle)
}
