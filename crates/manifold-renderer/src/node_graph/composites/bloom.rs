//! [`build_bloom`] — `Threshold → MipChain → Blur → Blend(Add)` with
//! source fan-out.
//!
//! Bloom is the V1 hero composite: it exercises every interesting graph
//! shape in one preset — fan-out (source feeds both Threshold and Blend's
//! base), multi-stage chain (Threshold → MipChain → Blur), and a
//! merge-back via Blend at the end. If anything's wrong with how
//! composites compose, Bloom is the test that finds it.

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::{Blend, Blur, MipChain, Threshold};
use crate::node_graph::validation::GraphError;

pub const BLOOM_TYPE_ID: &str = "composite.bloom";

/// Bloom topology:
///
/// ```text
///  source ──→ Threshold ──→ MipChain ──→ Blur ──→ Blend.overlay
///       │                                              ↑
///       └─────────────────────────────────────→ Blend.base
/// ```
///
/// The Blend mode is pre-set to `Add`. Exposed parameters:
///   - `threshold_level`: cutoff above which pixels contribute to the bright pass
///   - `blur_radius`: how soft the bright stuff gets
///   - `intensity`: how much of the bloomed copy is layered on top (Blend opacity)
pub fn build_bloom(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let thresh = graph.add_node(Box::new(Threshold::new()));
    let mips = graph.add_node(Box::new(MipChain::new()));
    let blur = graph.add_node(Box::new(Blur::new()));
    let blend = graph.add_node(Box::new(Blend::new()));

    // Bright pass through the mip chain and blur.
    graph.connect(source, (thresh, "source"))?;
    graph.connect((thresh, "out"), (mips, "source"))?;
    graph.connect((mips, "out"), (blur, "source"))?;

    // Blend the bright pass back over the original.
    graph.connect(source, (blend, "base"))?;
    graph.connect((blur, "out"), (blend, "overlay"))?;

    // Blend mode index: 0=Normal, 1=Add, 2=Multiply, 3=Screen, 4=Overlay,
    // 5=Difference. See `primitives::BLEND_MODES`.
    graph.set_param(blend, "mode", ParamValue::Enum(1))?;

    let mut handle = CompositeHandle::new(BLOOM_TYPE_ID, (blend, "out"));
    handle.add_inner(thresh);
    handle.add_inner(mips);
    handle.add_inner(blur);
    handle.add_inner(blend);
    handle.expose_param("threshold_level", thresh, "level");
    handle.expose_param("blur_radius", blur, "radius");
    handle.expose_param("intensity", blend, "opacity");
    Ok(handle)
}
