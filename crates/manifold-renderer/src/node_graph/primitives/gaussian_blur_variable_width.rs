//! `node.variable_blur` — per-pixel-width separable
//! Gaussian blur. One dispatch = one axis; pair two with ping-pong
//! for a full 2D blur.
//!
//! Adapted from `effects/shaders/fx_depth_of_field_compute.wgsl`'s
//! 9-/17-/25-tap blur kernels, with the width source decoupled from
//! input.alpha (separate Texture2D input now) so the primitive
//! composes with any width source — DoF's CoC pass, a procedural
//! mask, a depth-gradient texture, etc.
//!
//! Two specialization knobs flatten dead branches at pipeline
//! creation time:
//!
//!   * `quality`        — Low (9-tap) / Medium (17-tap) / High (25-tap).
//!   * `weighting_mode` — None (plain Gaussian) / ScatterAsGatherByCoC
//!     (foreground-bleed guard for CoC-driven blurs).
//!
//! The 6 specialized pipelines are lazily compiled on first use and
//! cached. Defaults preserve the original behaviour (Medium + None).

use std::borrow::Cow;

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BLUR_VARIABLE_AXES: &[&str] = &["Horizontal", "Vertical"];
pub const BLUR_VARIABLE_QUALITIES: &[&str] = &["Low", "Medium", "High"];
pub const BLUR_VARIABLE_WEIGHTINGS: &[&str] = &["None", "ScatterAsGatherByCoC"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction: u32,
    max_radius: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GaussianBlurVariableWidth,
    type_id: "node.variable_blur",
    purpose: "Separable Gaussian blur where the per-pixel kernel width is sampled from a `width` Texture2D's R channel. One dispatch handles one axis (horizontal or vertical); pair two with ping-pong textures for a 2D blur. Three quality levels (9-/17-/25-tap kernels at σ≈2/4/6) and an optional CoC-driven scatter-as-gather weighting that prevents sharp pixels bleeding into blurry regions — load-bearing for DoF-class effects.",
    inputs: {
        in: Texture2D required,
        width: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_VARIABLE_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("max_radius"),
            label: "Max Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(12.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("quality"),
            label: "Quality",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: None,
            enum_values: BLUR_VARIABLE_QUALITIES,
        },
        ParamDef {
            name: Cow::Borrowed("weighting_mode"),
            label: "Weighting Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: BLUR_VARIABLE_WEIGHTINGS,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "step_size = width_sample × max_radius + 1.0 along the chosen axis. width_sample < 0.005 produces a pass-through (in-focus). For a full 2D blur: dispatch this primitive twice with axis=Horizontal then axis=Vertical, ping-ponging between two Rgba16Float textures. ScatterAsGatherByCoC: each neighbor only contributes if its CoC (sampled from the `width` texture's R channel) ≥ the center pixel's CoC, OR the center is itself very blurry (CoC > 0.5). For DoF parity set max_radius = 6.0 and weighting_mode = ScatterAsGatherByCoC; the kernel matches the legacy DoF blur byte-for-byte (step_size stays under the tap-densification threshold at this setting — see BUG-138 below). BUG-138: at step_size (px) > 8.0 each of the fixed 9/17/25 taps densifies into up to 4 sub-samples that fill the gap back toward the previous tap, so large CoC radii (e.g. 64px) no longer leave visible ring gaps between samples; below that threshold the kernel is bit-identical to the original fixed-tap-per-sample behaviour.",
    examples: [],
    picker: { label: "Variable Blur", category: Atom },
    summary: "A Gaussian blur whose strength changes per pixel from a control image, so some areas blur more than others. Feed a mask or depth map into the width input for selective focus.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["variable blur", "gaussian blur", "depth blur", "selective blur", "depth of field", "Compound Blur"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/gaussian_blur_variable_width_body.wgsl"),
    input_access: [Gather, Gather],
    stencil_fetch: true,
    wgsl_specialization: [("QUALITY_LEVEL", "quality"), ("WEIGHTING_MODE", "weighting_mode")],
    extra_fields: {
        pipelines: AHashMap<u32, GpuComputePipeline> = AHashMap::new(),
    },
}

fn read_enum(ctx: &EffectNodeContext<'_, '_>, name: &str, default: u32) -> u32 {
    match ctx.params.get(name) {
        Some(ParamValue::Enum(n)) => *n,
        Some(ParamValue::Float(f)) => f.round() as u32,
        _ => default,
    }
}

fn pipeline_key(quality: u32, weighting: u32) -> u32 {
    weighting * 3 + quality
}

impl Primitive for GaussianBlurVariableWidth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let direction = read_enum(ctx, "axis", 0);
        let max_radius = match ctx.params.get("max_radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 12.0,
        };
        let quality = read_enum(ctx, "quality", 1).min(2);
        let weighting = read_enum(ctx, "weighting_mode", 0).min(1);

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(width_tex) = ctx.inputs.texture_2d("width") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let key = pipeline_key(quality, weighting);
        let pipeline = self
            .pipelines
            .entry(key)
            .or_insert_with(|| {
                // Single-source: `in` + `width` are both Gather inputs (sampled at
                // body-computed tap offsets); generated kernel binds uniform(0)/
                // tex_in(1)/tex_width(2)/samp(3)/dst(4), matching the set below. The
                // body references the QUALITY_LEVEL/WEIGHTING_MODE specialization
                // tokens, so we still specialize the GENERATED WGSL per (quality,
                // weighting) — dead tap branches flatten away, perf preserved.
                // gaussian_blur_variable_width.wgsl is the parity oracle.
                let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.variable_blur standalone codegen");
                let quality_str = match quality {
                    0 => "0u",
                    2 => "2u",
                    _ => "1u",
                };
                let weighting_str = if weighting == 1 { "1u" } else { "0u" };
                let label = format!(
                    "node.variable_blur.q{quality}.w{weighting}"
                );
                gpu.device.create_specialized_compute_pipeline(
                    &wgsl,
                    crate::node_graph::freeze::codegen::ENTRY,
                    &[
                        ("QUALITY_LEVEL", quality_str),
                        ("WEIGHTING_MODE", weighting_str),
                    ],
                    &label,
                )
            });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = BlurUniforms {
            direction,
            max_radius,
            _pad0: 0,
            _pad1: 0,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: width_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.variable_blur",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn gaussian_blur_variable_width_declares_two_texture_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            GaussianBlurVariableWidth::TYPE_ID,
            "node.variable_blur"
        );
        assert_eq!(GaussianBlurVariableWidth::INPUTS.len(), 2);
        assert_eq!(GaussianBlurVariableWidth::INPUTS[0].name, "in");
        assert_eq!(GaussianBlurVariableWidth::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(GaussianBlurVariableWidth::INPUTS[1].name, "width");
        assert_eq!(GaussianBlurVariableWidth::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(GaussianBlurVariableWidth::OUTPUTS.len(), 1);
        assert_eq!(GaussianBlurVariableWidth::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn gaussian_blur_variable_width_has_axis_radius_quality_weighting_params() {
        let names: Vec<&str> = GaussianBlurVariableWidth::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["axis", "max_radius", "quality", "weighting_mode"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GaussianBlurVariableWidth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.variable_blur");
    }

    /// BUG-138 — numeric proof that tap count now scales with radius instead
    /// of holding fixed. Mirrors `vbw_subtap_count()` /
    /// `gaussian_blur_variable_width_body.wgsl`'s `VBW_GAP_THRESHOLD_PX` /
    /// `VBW_SUBTAP_CAP` (kept in sync by hand — this is a test-only oracle,
    /// never used at runtime; the GPU kernel does the real computation).
    /// Total effective taps for a quality tier with `n_half` logical
    /// positions per side is `1 + 2 * n_half * subtaps`.
    fn vbw_subtap_count(step_size: f32) -> i32 {
        const GAP_THRESHOLD_PX: f32 = 8.0;
        const SUBTAP_CAP: i32 = 4;
        let raw = (step_size / GAP_THRESHOLD_PX).ceil() as i32;
        raw.clamp(1, SUBTAP_CAP)
    }

    #[test]
    fn bug_138_small_radius_stays_at_the_original_fixed_tap_count() {
        // max_radius = 6.0 DoF-parity setting (composition_notes): at full
        // CoC, step_size = 1.0 * 6.0 + 1.0 = 7.0, under the 8.0px threshold.
        let step_size = 1.0f32 * 6.0 + 1.0;
        assert_eq!(vbw_subtap_count(step_size), 1);
        let n_half = 12; // High quality, 25-tap
        let total_taps = 1 + 2 * n_half * vbw_subtap_count(step_size);
        assert_eq!(total_taps, 25, "DoF-parity radius must stay byte-identical to the original 25-tap kernel");
    }

    #[test]
    fn bug_138_large_radius_scales_tap_count_above_the_old_fixed_ceiling() {
        // The bug's own repro: max_radius = 64px, full CoC.
        let step_size = 1.0f32 * 64.0 + 1.0;
        let subtaps = vbw_subtap_count(step_size);
        assert!(subtaps > 1, "large CoC radius must trigger tap densification");
        assert_eq!(subtaps, 4, "worst-case subtap multiplier is capped at 4");

        for (n_half, old_fixed_taps) in [(4, 9), (8, 17), (12, 25)] {
            let total_taps = 1 + 2 * n_half * subtaps;
            assert!(
                total_taps > old_fixed_taps,
                "quality tier with {n_half} half-taps must sample MORE at 64px radius \
                 than its old fixed count ({old_fixed_taps}); got {total_taps}"
            );
        }
        // High quality goes from the old fixed 25 taps to 97.
        assert_eq!(1 + 2 * 12 * subtaps, 97);
    }

    #[test]
    fn bug_138_subtap_count_is_monotonic_non_decreasing_in_step_size() {
        let mut prev = vbw_subtap_count(1.0);
        for step_size_px in [2, 4, 8, 9, 16, 24, 32, 65, 200] {
            let cur = vbw_subtap_count(step_size_px as f32);
            assert!(cur >= prev, "subtap count regressed at step_size={step_size_px}");
            assert!((1..=4).contains(&cur), "subtap count must stay within [1, 4]");
            prev = cur;
        }
    }
}
