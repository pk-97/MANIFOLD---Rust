//! Fusion classification metadata (design doc §12, §3).
//!
//! Every primitive declares — via the `primitive!` macro, defaulting to the
//! conservative [`FusionKind::Boundary`] — whether and how it can fold into a
//! fused kernel. The fusion region-grower reads this off each node (through
//! [`EffectNode::fusion_kind`](crate::node_graph::effect_node::EffectNode::fusion_kind))
//! to grow maximal same-domain pure regions and cut at the rest. Conservative
//! by construction: an unclassified atom never fuses.

/// How a primitive participates in fusion.
///
/// For v1 (texture-pointwise), the two fusable kinds carry an implied
/// contract that keeps the classifier simple: both iterate **output-sized**
/// (grid from the destination) and read every input at the **same element**
/// (own pixel / coincident UV). Richer per-input read-semantics — a
/// texel-load atom (dither) that can't cross a resolution seam, or a
/// dependent gather — get their own variants + per-input markers when the
/// first such atom is converted; adding them is additive and does not
/// invalidate existing `Pointwise`/`MultiInputCoincident` atoms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FusionKind {
    /// Not fusable — the default for every primitive until it opts in.
    /// CPU/control nodes, stateful nodes (feedback/accumulators), gathers,
    /// resamples, IO endpoints. A region is cut at every boundary, so the
    /// compiler only ever fuses what an atom explicitly declares fusable.
    #[default]
    Boundary,
    /// Reads only its own element (own pixel / own particle) and writes one
    /// element. Output-sized iteration, same-element read. The textbook
    /// fusable atom — gain, contrast, saturation, hue_saturation, colorize,
    /// clamp_texture.
    Pointwise,
    /// Reads the SAME element from N≥2 inputs (coincident) and writes one.
    /// Output-sized iteration. Fusable when all inputs resolve to the same
    /// element-space (the DD10 resolution-seam guard enforces this). e.g.
    /// `node.mix` — `a` and `b` sampled at the same UV.
    MultiInputCoincident,
}

impl FusionKind {
    /// Whether this primitive can be folded into a fused kernel at all.
    /// `Boundary` is the only non-fusable kind.
    pub fn is_fusable(self) -> bool {
        !matches!(self, FusionKind::Boundary)
    }
}

#[cfg(test)]
mod tests {
    use super::FusionKind;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::primitives::Gain;

    #[test]
    fn default_is_boundary() {
        assert_eq!(FusionKind::default(), FusionKind::Boundary);
        assert!(!FusionKind::Boundary.is_fusable());
        assert!(FusionKind::Pointwise.is_fusable());
        assert!(FusionKind::MultiInputCoincident.is_fusable());
    }

    /// The macro slot propagates a converted atom's kind + body through the
    /// `EffectNode` trait object (the surface the region-grower + codegen read).
    #[test]
    fn converted_atom_exposes_kind_and_body() {
        let g = Gain::new();
        let node: &dyn EffectNode = &g;
        assert_eq!(node.fusion_kind(), FusionKind::Pointwise);
        let body = node.wgsl_body().expect("converted gain exposes a fusable body");
        assert!(body.contains("fn body"), "body fragment must define `fn body`");
        assert!(body.contains("gain"), "gain body must reference the gain param");
    }
}
