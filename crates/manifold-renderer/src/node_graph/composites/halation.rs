//! [`build_halation`] — `Threshold → MipChain → Blur → ChannelMix(tint) → Blend(Add)`.
//!
//! Same skeleton as Bloom plus a ChannelMix tint pass between the blur
//! and the additive blend, giving the halo a warm-red film-stock cast.
//! Validates that two composites can share most of their internal shape
//! and still differ meaningfully — exactly the design-doc claim that
//! "once Bloom is a composite, Halation becomes almost a preset".

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::{Blend, Blur, ChannelMix, MipChain, Threshold};
use crate::node_graph::validation::GraphError;

pub const HALATION_TYPE_ID: &str = "composite.halation";

/// Halation topology:
///
/// ```text
///  source ──→ Threshold ──→ MipChain ──→ Blur ──→ ChannelMix ──→ Blend.overlay
///       │                                                              ↑
///       └──────────────────────────────────────────────────────→ Blend.base
/// ```
///
/// The ChannelMix is pre-loaded with a warm-red tint matrix. Blend
/// mode is pre-set to `Add`.
pub fn build_halation(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
) -> Result<CompositeHandle, GraphError> {
    let thresh = graph.add_node(Box::new(Threshold::new()));
    let mips = graph.add_node(Box::new(MipChain::new()));
    let blur = graph.add_node(Box::new(Blur::new()));
    let tint = graph.add_node(Box::new(ChannelMix::new()));
    let blend = graph.add_node(Box::new(Blend::new()));

    // Warm-red tint: dampen green and blue, keep red full. Identity-ish
    // matrix with G and B rows scaled down. (The ChannelMix default is
    // identity, so we override rows 1 and 2.)
    graph.set_param(tint, "row1", ParamValue::Vec4([0.0, 0.4, 0.0, 0.0]))?;
    graph.set_param(tint, "row2", ParamValue::Vec4([0.0, 0.0, 0.2, 0.0]))?;

    graph.connect(source, (thresh, "source"))?;
    graph.connect((thresh, "out"), (mips, "source"))?;
    graph.connect((mips, "out"), (blur, "source"))?;
    graph.connect((blur, "out"), (tint, "source"))?;

    graph.connect(source, (blend, "base"))?;
    graph.connect((tint, "out"), (blend, "overlay"))?;

    // Blend mode = Add (index 1, see primitives::BLEND_MODES).
    graph.set_param(blend, "mode", ParamValue::Enum(1))?;

    let mut handle = CompositeHandle::new(HALATION_TYPE_ID, (blend, "out"));
    handle.add_inner(thresh);
    handle.add_inner(mips);
    handle.add_inner(blur);
    handle.add_inner(tint);
    handle.add_inner(blend);
    handle.expose_param("threshold_level", thresh, "level");
    handle.expose_param("blur_radius", blur, "radius");
    handle.expose_param("intensity", blend, "opacity");
    Ok(handle)
}
