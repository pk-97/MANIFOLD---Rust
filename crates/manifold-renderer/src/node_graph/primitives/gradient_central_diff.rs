//! `node.edge_slope` — per-pixel central-difference
//! gradient of one channel of an input texture.
//!
//! Outputs (dx, dy, 0, 1) in RGBA, where dx = (right − left) / 2 and
//! dy = (up − down) / 2 for the chosen channel. The standard vec2
//! gradient used by Sobel-light edge detectors, fluid-sim curl
//! extraction, height-to-normal pipelines, and any per-pixel
//! finite-difference math.

use std::borrow::Cow;

use manifold_gpu::{GpuAddressMode, GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const GRADIENT_CHANNELS: &[&str] = &["R", "G", "B", "A"];

/// Output scaling. `Texel`: dx = (R - L) * 0.5 (default — texel-space
/// finite difference, matches the legacy oily-fluid / heightmap-to-normal
/// consumers). `UV`: dx = (R - L) * W * 0.5, dy = (U - D) * H * 0.5 —
/// per-axis multiplication by the dimension halves so the output is in
/// per-UV-unit space, what fluid-sim gradient-rotate consumers need.
pub const GRADIENT_SCALE_MODES: &[&str] = &["Texel", "UV"];

/// Boundary policy. `Clamp` (default): bilinear sampling with the default
/// clamp-to-edge sampler — matches existing behaviour, suitable for
/// non-cyclic textures (heightmaps, oily-fluid normals). `Repeat`: the
/// neighbour taps wrap toroidally via a repeat sampler, required for
/// fluid sims whose density field is cyclic.
pub const GRADIENT_WRAP_MODES: &[&str] = &["Clamp", "Repeat"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientUniforms {
    channel: u32,
    scale_mode: u32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: GradientCentralDiff,
    type_id: "node.edge_slope",
    purpose: "Per-pixel central-difference gradient of a single input channel. Output: (dx, dy, 0, 1) in RGBA. `scale_mode` selects Texel-space (`(R - L) * 0.5` — default, matches oily-fluid / heightmap-to-normal usage) or UV-space (`(R - L) * W * 0.5` per-axis — multiplies by the dimension halves so output is in per-UV-unit space, what fluid-sim gradient-rotate needs). `wrap_mode` selects Clamp (default, bilinear-clamp sampler) or Repeat (toroidal sampler for cyclic fluid sims). The standard vec2 gradient atom: feeds Sobel edge detectors, fluid-sim curl-from-color extraction, heightmap→normal pipelines, reaction-diffusion flow seeding.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("channel"),
            label: "Channel",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_CHANNELS,
        },
        ParamDef {
            name: Cow::Borrowed("scale_mode"),
            label: "Scale Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_SCALE_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("wrap_mode"),
            label: "Wrap Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: GRADIENT_WRAP_MODES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Output is a SIGNED vec2 field. Pair with `node.normalize` for direction-only gradients (used in fluid-sim curl forcing), or feed directly into `node.rotate_vector` for arbitrary-angle curl flow. For a per-channel gradient of an RG texture (oily-fluid pattern), instance this primitive twice with channel=R and channel=G and combine downstream. Defaults (Texel + Clamp) preserve legacy oily-fluid / heightmap behaviour. Use scale_mode=UV + wrap_mode=Repeat to compose with `scale_offset_texture` + `rotate_vec2_by_angle` as the decomposed `fluid_gradient_rotate` pipeline.",
    examples: [],
    picker: { label: "Edge Slope", category: Atom },
    summary: "Measures how fast a value changes across the image, giving the direction and steepness of edges. The base for normal maps and edge effects.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["gradient", "edge slope", "gradient central diff", "derivative", "sobel"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/gradient_central_diff_body.wgsl"),
    input_access: [Gather],
    // D6(a) deliberately does NOT mark `in` precision_critical: same
    // reasoning as node.surface_bumps — `in` reads via `Gather`
    // (`textureSampleLevel`, a real filtering sampler), not `GatherTexel`,
    // so a non-filterable Rgba32Float producer would break this atom's own
    // read. This atom's OWN output already has a per-instance fp32 opt-in
    // (`output_format_override` below) for exactly the precision-sensitive
    // case (FluidSim's flow field feedback) — that is the sanctioned path
    // here, not the D6(a) input-side promotion.
    extra_fields: {
        repeat_sampler: Option<manifold_gpu::GpuSampler> = None,
        // fp32-output opt-in: an `outputFormats` override (rgba32float) lands here
        // so this atom can serve as a FULL-PRECISION intermediate inside a chaotic
        // feedback loop (FluidSim flow field) — letting the unfused editor store
        // exactly and the fused kernel keep f32 registers, so fused == unfused.
        output_format_override: Option<manifold_gpu::GpuTextureFormat> = None,
    },
}

impl Primitive for GradientCentralDiff {
    /// Report the fp32 override so the build's `outputFormats` audit accepts it and
    /// the executor allocates the output at this format (see [`set_output_format`]).
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        if port == "out" {
            self.output_format_override
        } else {
            None
        }
    }

    /// Store an `outputFormats` override (rgba16float default / rgba32float opt-in).
    fn set_output_format(&mut self, port: &str, format: manifold_gpu::GpuTextureFormat) {
        if port == "out" {
            self.output_format_override = Some(format);
        }
    }

    /// `in` is a `Gather` (4-neighbour central difference), so a fused region
    /// must bind the SAME sampler this atom would create standalone: a Repeat
    /// (toroidal) sampler under `wrap_mode = Repeat`, else the default clamp.
    /// Mirrors the `wrap_repeat` read in `run()` exactly — the fused flow field
    /// (FluidSim) wraps, so without this the fused kernel clamps at the edges
    /// and the look shifts from the unfused editor.
    fn fused_gather_sampler_mode(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> manifold_gpu::GpuAddressMode {
        let wrap_repeat = match params.get("wrap_mode") {
            Some(ParamValue::Enum(v)) => *v == 1,
            Some(ParamValue::Float(f)) => f.round() as u32 == 1,
            _ => false,
        };
        if wrap_repeat {
            manifold_gpu::GpuAddressMode::Repeat
        } else {
            manifold_gpu::GpuAddressMode::ClampToEdge
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let channel = match ctx.params.get("channel") {
            Some(ParamValue::Enum(v)) => (*v).min(3),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(3),
            _ => 0,
        };
        let scale_mode = match ctx.params.get("scale_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let wrap_repeat = match ctx.params.get("wrap_mode") {
            Some(ParamValue::Enum(v)) => *v == 1,
            Some(ParamValue::Float(f)) => f.round() as u32 == 1,
            _ => false,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        // Generate the kernel at the output's declared format (f16 default, fp32
        // when overridden) so the standalone dst binding matches the texture the
        // executor allocated — the fp32-intermediate path for in-loop fusion.
        let out_fmt = self
            .output_format_override
            .unwrap_or(manifold_gpu::GpuTextureFormat::Rgba16Float);
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: `in` is a Gather input (4-neighbour central
            // difference). Generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3);
            // the body recovers the texel step from `dims` and ignores wrap_mode
            // (the sampler below carries the address mode).
            // gradient_central_diff.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec_fmt::<Self>(out_fmt)
                    .expect("node.edge_slope standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.edge_slope",
            )
        });
        let clamp_sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let repeat_sampler = self.repeat_sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                ..Default::default()
            })
        });
        let sampler = if wrap_repeat {
            repeat_sampler
        } else {
            clamp_sampler
        };

        let uniforms = GradientUniforms {
            channel,
            scale_mode,
            _pad0: 0.0,
            _pad1: 0.0,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.edge_slope",
        );
    }
}
