//! Node implementations for the catalog defined in `docs/NODE_CATALOG.md`.
//!
//! This module hosts both atoms (small generic composable building blocks
//! like Mix, Feedback, Gaussian Blur) and the wrapped legacy effects
//! (Bloom, Watercolor, Halation, etc.) — all as `EffectNode` impls behind
//! flat `node.*` type IDs. The atom/effect split is presentation metadata,
//! not a structural divide.

mod affine_transform;
mod auto_gain;
mod blob_tracking;
mod bloom;
mod chromatic_offset;
mod clamp_stretch;
mod color;
mod color_grade;
mod compose;
mod depth_of_field;
mod dither_pattern;
mod edge_detect;
mod filter;
mod glitch;
mod halation;
mod highlight_boost;
mod infrared;
mod invert;
mod kaleido_fold;
mod lut1d;
mod separable_gaussian;
mod strobe;
mod temporal;
mod uv;
mod voronoi_prism;
mod watercolor;
mod wet_dry_mix;
mod wireframe_depth;

pub use affine_transform::AffineTransform;
pub use auto_gain::{AutoGain, AUTO_GAIN_CHARACTERS, AUTO_GAIN_TYPE_ID};
pub use blob_tracking::{BlobTracking, BLOB_TRACKING_TYPE_ID};
pub use bloom::{Bloom, BLOOM_TYPE_ID};
pub use chromatic_offset::ChromaticOffset;
pub use clamp_stretch::ClampStretch;
pub use color::{
    ChannelMix, ColorRamp, Brightness, CHANNEL_MIX_TYPE_ID, COLOR_RAMP_TYPE_ID,
    BRIGHTNESS_TYPE_ID,
};
pub use color_grade::ColorGrade;
pub use compose::{Blend, Mix, BLEND_MODES, BLEND_TYPE_ID, MIX_MODES, MIX_TYPE_ID};
pub use depth_of_field::{
    DepthOfField, DEPTH_OF_FIELD_FOCUS_MODES, DEPTH_OF_FIELD_QUALITIES, DEPTH_OF_FIELD_TYPE_ID,
};
pub use dither_pattern::DitherPattern;
pub use edge_detect::EdgeDetect;
pub use glitch::Glitch;
pub use halation::{Halation, HALATION_TYPE_ID};
pub use highlight_boost::HighlightBoost;
pub use infrared::{Infrared, INFRARED_PALETTES, INFRARED_TYPE_ID};
pub use invert::Invert;
pub use kaleido_fold::KaleidoFold;
pub use lut1d::ColorLut;
pub use separable_gaussian::{
    GaussianBlur, GAUSSIAN_BLUR_AXES, GAUSSIAN_BLUR_KERNELS,
    GAUSSIAN_BLUR_TYPE_ID,
};
pub use strobe::{Strobe, NOTE_RATES as STROBE_NOTE_RATES};
pub use filter::{
    Blur, MipChain, Threshold, BLUR_MODES, BLUR_TYPE_ID, MIP_CHAIN_TYPE_ID, THRESHOLD_TYPE_ID,
};
pub use temporal::{Feedback, FEEDBACK_MODES, FEEDBACK_TYPE_ID};
pub use voronoi_prism::VoronoiPrism;
pub use watercolor::{Watercolor, WATERCOLOR_TYPE_ID};
pub use wet_dry_mix::{WetDry, WET_DRY_TYPE_ID};
pub use wireframe_depth::{
    WireframeDepth, WIREFRAME_DEPTH_BLEND_MODES, WIREFRAME_DEPTH_MESH_RATES,
    WIREFRAME_DEPTH_ONOFF, WIREFRAME_DEPTH_TYPE_ID,
};
pub use uv::{
    Sample, Transform, SAMPLE_FILTER_MODES, SAMPLE_TYPE_ID, SAMPLE_WRAP_MODES,
    TRANSFORM_MODES, TRANSFORM_TYPE_ID,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{
        compile, validate, EffectNode, Executor, FinalOutput, FrameTime, Graph, ParamType,
        ParamValue, Source,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Iterate one boxed instance of each V1 primitive so tests can assert
    /// invariants over the whole catalog without listing them by hand.
    fn all_primitives() -> Vec<Box<dyn EffectNode>> {
        vec![
            Box::new(Brightness::new()),
            Box::new(ChannelMix::new()),
            Box::new(ColorRamp::new()),
            Box::new(Mix::new()),
            Box::new(Blend::new()),
            Box::new(Threshold::new()),
            Box::new(Blur::new()),
            Box::new(MipChain::new()),
            Box::new(Transform::new()),
            Box::new(Sample::new()),
            Box::new(Feedback::new()),
            Box::new(WetDry::new()),
        ]
    }

    #[test]
    fn all_v1_primitives_have_unique_type_ids() {
        let primitives = all_primitives();
        let ids: HashSet<&str> = primitives.iter().map(|p| p.type_id().as_str()).collect();
        assert_eq!(ids.len(), 12, "primitive type IDs must be unique");
    }

    #[test]
    fn all_v1_primitive_type_ids_have_node_prefix() {
        for p in all_primitives() {
            assert!(
                p.type_id().as_str().starts_with("node."),
                "node type IDs must start with `node.` — got {}",
                p.type_id().as_str()
            );
        }
    }

    #[test]
    fn all_v1_primitives_produce_at_least_one_output() {
        for p in all_primitives() {
            assert!(
                !p.outputs().is_empty(),
                "primitive {} has no outputs",
                p.type_id().as_str()
            );
        }
    }

    #[test]
    fn parameter_defaults_match_declared_types() {
        // Catches typos like `default: ParamValue::Float(...)` on a
        // `ty: ParamType::Vec3` parameter.
        for p in all_primitives() {
            for def in p.parameters() {
                let ok = matches!(
                    (def.ty, def.default),
                    (ParamType::Float, ParamValue::Float(_))
                        | (ParamType::Int, ParamValue::Int(_))
                        | (ParamType::Bool, ParamValue::Bool(_))
                        | (ParamType::Vec2, ParamValue::Vec2(_))
                        | (ParamType::Vec3, ParamValue::Vec3(_))
                        | (ParamType::Vec4, ParamValue::Vec4(_))
                        | (ParamType::Color, ParamValue::Color(_))
                        | (ParamType::Enum, ParamValue::Enum(_))
                );
                assert!(
                    ok,
                    "{} param `{}`: default {:?} does not match declared type {:?}",
                    p.type_id().as_str(),
                    def.name,
                    def.default,
                    def.ty,
                );
            }
        }
    }

    #[test]
    fn enum_param_defaults_are_in_range() {
        for p in all_primitives() {
            for def in p.parameters() {
                if def.ty == ParamType::Enum {
                    let ParamValue::Enum(idx) = def.default else {
                        unreachable!("enforced by parameter_defaults_match_declared_types");
                    };
                    assert!(
                        (idx as usize) < def.enum_values.len(),
                        "{} param `{}`: default index {} out of bounds for {} options",
                        p.type_id().as_str(),
                        def.name,
                        idx,
                        def.enum_values.len(),
                    );
                }
            }
        }
    }

    /// Hero integration test: assemble the V1 Bloom-as-composite topology
    /// from primitives + boundary nodes, compile it, execute it. Validates
    /// that the trait shape and pool work for a real composite preset.
    ///
    /// Topology:
    ///
    /// ```text
    ///   Source ──→ MipChain ──→ Blur ──→ Blend.overlay ─→ FinalOutput
    ///       │                              ↑
    ///       └─────────────────────────→ Blend.base
    /// ```
    ///
    /// (Threshold is omitted from this test for simplicity — the four-node
    /// shape is enough to exercise fan-out, multi-input, and the boundary.
    /// Real Bloom preset will include Threshold before MipChain.)
    #[test]
    fn bloom_shape_composite_compiles_and_executes() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let mips = g.add_node(Box::new(MipChain::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let blend = g.add_node(Box::new(Blend::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (mips, "source")).unwrap();
        g.connect((mips, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (blend, "base")).unwrap();
        g.connect((blur, "out"), (blend, "overlay")).unwrap();
        g.connect((blend, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 5);

        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    /// Mix has two required inputs; both must be wired or validate() fails.
    #[test]
    fn mix_requires_both_inputs_to_be_wired() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let _mix = g.add_node(Box::new(Mix::new()));
        // Don't wire either of mix's inputs.
        assert!(matches!(
            validate(&g),
            Err(crate::node_graph::GraphError::RequiredInputUnwired { .. })
        ));
    }

    /// Param values can be set on a primitive instance through the Graph API.
    #[test]
    fn primitive_params_accept_typed_overrides() {
        let mut g = Graph::new();
        let id = g.add_node(Box::new(Threshold::new()));
        g.set_param(id, "level", ParamValue::Float(0.7)).unwrap();
        g.set_param(id, "softness", ParamValue::Float(0.1)).unwrap();
        // Unknown param is rejected.
        assert!(g
            .set_param(id, "missing", ParamValue::Float(0.0))
            .is_err());
    }
}
