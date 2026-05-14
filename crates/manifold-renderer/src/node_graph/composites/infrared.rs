//! [`build_infrared`] — `Brightness → ColorRamp`.
//!
//! Two-node composite: extract luma, remap into a two-stop gradient. The
//! cleanest demonstration of how primitives compose into a recognisable
//! effect — the entire implementation is "two nodes wired in series".

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::primitives::{ColorRamp, Brightness};
use crate::node_graph::validation::GraphError;

pub const INFRARED_TYPE_ID: &str = "composite.infrared";

/// Infrared = `Brightness → ColorRamp`. Exposes `color_a` and `color_b`
/// from the inner ColorRamp for user customisation.
pub fn build_infrared(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let lum = graph.add_node(Box::new(Brightness::new()));
    let grad = graph.add_node(Box::new(ColorRamp::new()));

    graph.connect(source, (lum, "source"))?;
    graph.connect((lum, "out"), (grad, "source"))?;

    let mut handle = CompositeHandle::new(INFRARED_TYPE_ID, (grad, "out"));
    handle.add_inner(lum);
    handle.add_inner(grad);
    handle.expose_param("color_a", grad, "color_a");
    handle.expose_param("color_b", grad, "color_b");
    Ok(handle)
}
