//! [`build_mirror`] — alias preset of `UVTransform[mode=Mirror]`.
//!
//! The simplest possible composite: one inner node, one fixed parameter.
//! Demonstrates that "alias presets" (the design doc's name for thin
//! effects that are just a primitive with one mode pre-selected) cost
//! nothing more than a few lines of glue.

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::UVTransform;
use crate::node_graph::validation::GraphError;

pub const MIRROR_TYPE_ID: &str = "composite.mirror";

/// Mirror flips the source horizontally. Internally a single
/// `UVTransform` with `mode` pre-set to `Mirror`.
pub fn build_mirror(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let xform = graph.add_node(Box::new(UVTransform::new()));
    // UVTransform mode index: 0=Identity, 1=Mirror, 2=MirrorX, 3=MirrorY,
    // 4=FlipY, 5=QuadMirror. See `primitives::UV_TRANSFORM_MODES`.
    graph.set_param(xform, "mode", ParamValue::Enum(1))?;
    graph.connect(source, (xform, "source"))?;

    let mut handle = CompositeHandle::new(MIRROR_TYPE_ID, (xform, "out"));
    handle.add_inner(xform);
    Ok(handle)
}
