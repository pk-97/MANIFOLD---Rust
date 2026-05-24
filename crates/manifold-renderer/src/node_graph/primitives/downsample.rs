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
            name: "factor",
            label: "Factor",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: Some((0.0, 2.0)),
            enum_values: DOWNSAMPLE_FACTORS,
        },
    ],
    composition_notes: "Output is sized to input_dims / factor (rounded down, min 1). Pair with `node.gaussian_blur` or `node.gaussian_blur_variable_width` to do cheap multi-pass blur at reduced resolution. The downsample uses a uniform-weight box filter (each output pixel = mean of factor×factor inputs); for a higher-quality kernel apply a Gaussian blur AFTER downsampling, not before.",
    examples: ["preset.generator.oily_fluid"],
    picker: { label: "Downsample", category: Atom },
}

impl Primitive for Downsample {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        // Output dims = input dims / factor. If `in` isn't in the
        // scratch (producer is canvas-default OR a state-capture
        // back-edge whose dim isn't yet resolved), return None so
        // the executor resolves the slot to runtime canvas dims.
        // That means the downsample shader dispatches at canvas
        // resolution and performs a 4×4 box blur without actually
        // shrinking — variable-res becomes a no-op for this slot.
        // The old behaviour of falling back to a (0,0) canvas sentinel
        // here returned (1, 1), starving the entire downstream blur
        // chain. A proper fix is runtime dim resolution (Phase 3a-
        // followup); for now this keeps the chain functional when
        // the input dim isn't compile-time-knowable.
        let (src_w, src_h) = input_dims
            .iter()
            .find(|(name, _)| *name == "in")
            .map(|(_, d)| *d)?;
        // Factor is an enum: 0 → 2x, 1 → 4x, 2 → 8x. Default param
        // value is 1 (4x). The EffectNode trait doesn't pass params
        // to output_dims, so we hardcode 4x; widen the trait surface
        // in a follow-up if more presets need per-instance factor at
        // compile time.
        let factor = 4u32;
        let out_w = (src_w / factor).max(1);
        let out_h = (src_h / factor).max(1);
        Some((out_w, out_h))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let factor = match ctx.params.get("factor") {
            Some(ParamValue::Enum(n)) => match *n {
                0 => 2u32,
                1 => 4u32,
                2 => 8u32,
                _ => 4u32,
            },
            _ => 4u32,
        };

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
            gpu.device.create_compute_pipeline(
                include_str!("shaders/downsample.wgsl"),
                "cs_main",
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

    #[test]
    fn output_dims_divides_input_by_four() {
        // Phase-3-followup note: factor is hardcoded to 4 in
        // `output_dims` until the trait passes params at compile
        // time. Assert that contract here so a future widening
        // catches this test as the place to thread params through.
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let dims = node.output_dims("out", (1920, 1080), &[("in", (1920, 1080))]);
        assert_eq!(dims, Some((480, 270)));
    }

    #[test]
    fn output_dims_returns_none_when_input_dim_is_unknown() {
        // When `in`'s producer is canvas-default or a state-capture
        // back-edge, the compile-time dim isn't known. Returning None
        // lets the executor resolve the slot to runtime canvas. Old
        // behaviour (fall back to canvas_dims sentinel = (0,0) →
        // returns (1,1)) starved the downstream blur chain.
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        let dims = node.output_dims("out", (800, 600), &[]);
        assert_eq!(dims, None);
    }

    #[test]
    fn output_dims_clamps_to_at_least_one() {
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        // Input smaller than factor → output rounds to 0 → clamp to 1
        // so the executor doesn't try to allocate a zero-sized
        // texture (would crash in the backend).
        let dims = node.output_dims("out", (1920, 1080), &[("in", (3, 3))]);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Downsample::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.downsample");
    }
}
