//! `node.downsample` — integer-factor box-filter downsample.
//!
//! Reduces a Texture2D's resolution by a fixed integer factor (2 / 4
//! / 8), averaging the `factor × factor` source pixels per output
//! pixel. Used as the front of a quarter-res blur pipeline — `oily
//! fluid`'s velocity field gets downsampled to 1/4 area, blurred at
//! that resolution (cheap), then sampled from the small texture by
//! `texture_advect` alongside the original full-res velocity. The
//! pattern is reusable for any multi-res sim (fluid coarse grids,
//! bloom-style mip chains, depth-of-field CoC pyramids, etc.).
//!
//! Output dims are derived from the input's dims via
//! [`EffectNode::output_dims`] — the executor's plan compiler picks
//! this up and allocates the downstream slot at the smaller size,
//! pool-keyed on (PortType, GpuTextureFormat, dims) so a quarter-res
//! slot can't accidentally alias a full-res one.
//!
//! Format: writes to rgba16float regardless of input format. Reading
//! an fp32 input downsamples in fp32 (more accurate average), but
//! the box-filter result is averaged so fp16 precision is plenty for
//! a downstream blur or sample. If a future use case needs fp32
//! pass-through we'd specialise the shader per output format — for
//! now this matches what the legacy `cs_downsample` did.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const DOWNSAMPLE_FACTORS: &[&str] = &["2x", "4x", "8x"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DownsampleUniforms {
    factor: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: Downsample,
    type_id: "node.downsample",
    purpose: "Integer-factor (2x / 4x / 8x) box-filter downsample of a Texture2D. Output dims = input dims / factor; the executor allocates the downstream slot at the smaller size so subsequent passes (e.g. a gaussian blur) run at reduced bandwidth. Used as the front of multi-resolution pipelines — quarter-res velocity blur in oily fluid, bloom mip starts, CoC pyramids, etc.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("factor"),
            label: "Factor",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: Some((0.0, 2.0)),
            enum_values: DOWNSAMPLE_FACTORS,
        },
    ],
    depth_rule: Warp,
    composition_notes: "Output is sized to input_dims / factor (rounded down, min 1). Pair with `node.gaussian_blur` or `node.variable_blur` to do cheap multi-pass blur at reduced resolution. The downsample uses a uniform-weight box filter (each output pixel = mean of factor×factor inputs); for a higher-quality kernel apply a Gaussian blur AFTER downsampling, not before.",
    examples: ["preset.generator.oily_fluid"],
    picker: { label: "Downsample", category: Atom },
    summary: "Shrinks the image by a whole-number factor with a box filter, trading detail for speed. Good before a heavy effect or for a blocky look.",
    category: Routing,
    role: Filter,
    aliases: ["downsample", "downscale", "shrink", "Resolution TOP", "Pixelate"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/downsample_body.wgsl"),
    input_access: [Gather],
}

/// Decode the `factor` enum param into the integer downsample factor.
/// Default to 4 (the enum's default value `1`) for unset / malformed
/// params; matches the run-time fallback.
fn read_factor(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("factor") {
        Some(ParamValue::Enum(n)) => match *n {
            0 => 2u32,
            1 => 4u32,
            2 => 8u32,
            _ => 4u32,
        },
        Some(ParamValue::Float(f)) => match f.round() as i32 {
            0 => 2u32,
            1 => 4u32,
            2 => 8u32,
            _ => 4u32,
        },
        _ => 4u32,
    }
}

impl Primitive for Downsample {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        // Output dims = input dims / factor when input dim is known.
        // When `in`'s producer is canvas-default OR a state-capture
        // back-edge whose dim isn't yet resolved, returning `None`
        // here lets the executor fall back to
        // [`output_canvas_scale`] below — which lands the slot at
        // `canvas / factor` so the shader sees the dim ratio it
        // expects. Without that fallback, the old behaviour was to
        // allocate the slot at full canvas and dispatch a shader
        // that strides by `factor` — OOB reads returned zero and
        // poisoned everything outside the top-left 1/factor² of the
        // texture (the OilyFluid top-left-tile bug).
        let (src_w, src_h) = input_dims
            .iter()
            .find(|(name, _)| *name == "in")
            .map(|(_, d)| *d)?;
        let factor = read_factor(params);
        let out_w = (src_w / factor).max(1);
        let out_h = (src_h / factor).max(1);
        Some((out_w, out_h))
    }

    fn output_canvas_scale(
        &self,
        port: &str,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        // When `output_dims` couldn't resolve (input dim unknown),
        // declare the output as `canvas × 1 / factor`. The executor
        // computes this against the live canvas at slot-acquire time
        // and the shader's `id.xy * factor` indexing then reads
        // within bounds. State-capture back-edges from `node.feedback`
        // (and any future stateful primitive whose state texture is
        // canvas-sized) flow through here cleanly.
        Some((1, read_factor(params)))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let factor = read_factor(ctx.params);

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (dst.width, dst.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `in` is a Gather input (the body reads it via textureLoad at input-
            // pixel coords, deriving the box factor from in_dims/out_dims). The
            // generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3) — the sampler
            // is bound but unused (textureLoad), matching the hand shader.
            // downsample.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.downsample standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.downsample",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DownsampleUniforms {
            factor,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: dst,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.downsample",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn downsample_declares_one_input_one_output_and_factor_param() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Downsample::TYPE_ID, "node.downsample");
        assert_eq!(Downsample::INPUTS.len(), 1);
        assert_eq!(Downsample::INPUTS[0].name, "in");
        assert_eq!(Downsample::INPUTS[0].ty, PortType::Texture2D);
        assert!(Downsample::INPUTS[0].required);
        assert_eq!(Downsample::OUTPUTS.len(), 1);
        assert_eq!(Downsample::OUTPUTS[0].name, "out");
        assert_eq!(Downsample::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(Downsample::PARAMS.len(), 1);
        assert_eq!(Downsample::PARAMS[0].name, "factor");
    }

    /// Build a `ParamValues` map containing only the default `factor`
    /// (enum 1 = 4×) — enough to drive `output_dims` /
    /// `output_canvas_scale` in tests without exercising the full
    /// graph param-init path.
    fn default_params() -> crate::node_graph::effect_node::ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(std::borrow::Cow::Borrowed("factor"), ParamValue::Enum(1));
        p
    }

    #[test]
    fn output_dims_divides_input_by_factor() {
        // Default factor = 4×. With known input dims the planner gets
        // a concrete output dim and uses it directly.
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let params = default_params();
        let dims = node.output_dims("out", (1920, 1080), &[("in", (1920, 1080))], &params);
        assert_eq!(dims, Some((480, 270)));
    }

    #[test]
    fn output_dims_returns_none_when_input_dim_is_unknown() {
        // When `in`'s producer is canvas-default or a state-capture
        // back-edge, the compile-time dim isn't known. `output_dims`
        // returns None; the planner then consults `output_canvas_scale`
        // (next test) which lands the slot at canvas/factor.
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let params = default_params();
        let dims = node.output_dims("out", (800, 600), &[], &params);
        assert_eq!(dims, None);
    }

    #[test]
    fn output_canvas_scale_is_one_over_factor() {
        // The fallback used by the planner when `output_dims` returns
        // None. At runtime the executor resolves this against the live
        // canvas: `(canvas_w * 1 / 4, canvas_h * 1 / 4)`. The shader's
        // `id.xy * factor` indexing then reads within bounds (vs. the
        // pre-fix bug where the slot landed at full canvas and OOB
        // reads zero-poisoned everything past the top-left 1/16).
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let params = default_params();
        let scale = node.output_canvas_scale("out", &params);
        assert_eq!(scale, Some((1, 4)));
    }

    #[test]
    fn output_canvas_scale_tracks_factor_enum() {
        // Per-instance factor → per-instance scale. Verifies that the
        // canvas-scale hint reads from the live param value rather
        // than a hardcoded constant — so a JSON preset that sets
        // factor=0 (2×) lands its slot at canvas/2, not canvas/4.
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        for (enum_v, expected_den) in [(0u32, 2u32), (1, 4), (2, 8)] {
            let mut params = ahash::AHashMap::default();
            params.insert(std::borrow::Cow::Borrowed("factor"), ParamValue::Enum(enum_v));
            assert_eq!(
                node.output_canvas_scale("out", &params),
                Some((1, expected_den)),
                "factor enum {enum_v} should map to canvas/{expected_den}",
            );
        }
    }

    #[test]
    fn output_dims_clamps_to_at_least_one() {
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let params = default_params();
        // Input smaller than factor → output rounds to 0 → clamp to 1
        // so the executor doesn't try to allocate a zero-sized
        // texture (would crash in the backend).
        let dims = node.output_dims("out", (1920, 1080), &[("in", (3, 3))], &params);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.downsample");
    }
}
