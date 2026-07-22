//! `node.edge_slope` — per-pixel central-difference
//! gradient of one channel of an input texture.
//!
//! Outputs (dx, dy, 0, 1) in RGBA, where dx = (right − left) / 2 and
//! dy = (up − down) / 2 for the chosen channel. The standard vec2
//! gradient used by Sobel-light edge detectors, fluid-sim curl
//! extraction, height-to-normal pipelines, and any per-pixel
//! finite-difference math.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

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

/// Boundary policy for the 4 integer-texel neighbour taps. `Clamp`
/// (default): the neighbour index clamps to the texture bounds (manual
/// ClampToEdge) — matches existing behaviour, suitable for non-cyclic
/// textures (heightmaps, oily-fluid normals). `Repeat`: the neighbour index
/// wraps toroidally (manual modulo), required for fluid sims whose density
/// field is cyclic (FluidSim2D's flow field). D6(a): resolved entirely
/// inside the body now (`gcd_wrap_coord`) — no sampler is bound at all
/// (`in` is `GatherTexel`, not `Gather`).
pub const GRADIENT_WRAP_MODES: &[&str] = &["Clamp", "Repeat"];

// D6(a) fix (found while converting to GatherTexel): the OLD struct packed
// `wrap_mode` as an unused `_pad0: f32` word (always uploaded as 0.0 / bit
// pattern 0) because the old body never read it — Clamp/Repeat lived
// entirely in run()'s host-side sampler choice. Now the body genuinely
// branches on `wrap_mode` (`gcd_wrap_coord`), so this struct MUST match the
// codegen's generated `Params` layout exactly: 3 declared params (channel,
// scale_mode, wrap_mode), padded to the next 16-byte multiple with one u32
// word — `struct Params { channel: u32, scale_mode: u32, wrap_mode: u32,
// _pad0: u32 }` (freeze::codegen's header_words/pad_words math). A silent
// byte-layout mismatch here would have been invisible before (the ignored
// word could hold anything); it is load-bearing now.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientUniforms {
    channel: u32,
    scale_mode: u32,
    wrap_mode: u32,
    _pad0: u32,
}

crate::primitive! {
    name: GradientCentralDiff,
    type_id: "node.edge_slope",
    purpose: "Per-pixel central-difference gradient of a single input channel. Output: (dx, dy, 0, 1) in RGBA. `scale_mode` selects Texel-space (`(R - L) * 0.5` — default, matches oily-fluid / heightmap-to-normal usage) or UV-space (`(R - L) * W * 0.5` per-axis — multiplies by the dimension halves so output is in per-UV-unit space, what fluid-sim gradient-rotate needs). `wrap_mode` selects Clamp (default, clamps the neighbour texel index to the texture bounds) or Repeat (modulo-wraps the neighbour index toroidally, for cyclic fluid sims) — resolved by an exact integer textureLoad, no sampler. The standard vec2 gradient atom: feeds Sobel edge detectors, fluid-sim curl-from-color extraction, heightmap→normal pipelines, reaction-diffusion flow seeding.",
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
    // D6(a) follow-up (DEPTH_RELIGHT_DESIGN.md, root-cause fix requested
    // after the P4 audit): converted from `Gather` (filtering-sampler
    // `textureSampleLevel`) to `GatherTexel` (exact integer `textureLoad`,
    // boundary policy resolved manually in-body from `wrap_mode` — see
    // `gcd_wrap_coord` in the body fragment). The 4 central-difference taps
    // always land on an exact texel center (offset is exactly ±1 texel from
    // `uv = (id+0.5)/dims`), and a Repeat-address sampler's periodic wrap at
    // an exact-texel offset is exactly a modulo of the integer index — so
    // both Clamp and Repeat are value-preserving conversions (proven by
    // `gpu_tests::gather_texel_conversion_is_value_preserving_clamp` and
    // `..._repeat`, an old-vs-new A/B dispatch on the same synthetic field).
    // This is what lets `in` carry `precision_critical`.
    input_access: [GatherTexel],
    // Differentiates the field via central difference — fp16's ~10-bit
    // mantissa quantizes the (R-L)/(U-D) difference into visible banding on
    // smooth gradients (the flow fields this atom feeds — FluidSim's curl
    // forcing, oily-fluid's normal tangent).
    precision_critical: ["in"],
    extra_fields: {
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

    /// KEPT after the D6(a) `GatherTexel` conversion even though this atom's
    /// own body no longer binds any sampler at all: `InputAccess::is_gather()`
    /// is true for `GatherTexel` too (both are "the wire feeding it never
    /// unions into a threaded register"), so `install.rs`'s
    /// `resolve_gather_sampler_mode` still walks this atom when it shares a
    /// FUSED REGION with an actual sampler-consuming `Gather` member — this
    /// override is what lets that CO-MEMBER's shared sampler still resolve to
    /// Repeat when this atom's own `wrap_mode = Repeat` (FluidSim's flow
    /// field), even though this atom's own read no longer uses that sampler.
    /// Removing the override would silently default every co-member to
    /// ClampToEdge whenever this atom sits in the same region with
    /// `wrap_mode = Repeat` set — a real, if narrow, correctness trap, so it
    /// stays even though it looks orphaned from this file alone.
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
        // wrap_mode now travels as a REAL uniform field the body reads
        // directly (gcd_wrap_coord) — no host-side sampler selection.
        let wrap_mode = match ctx.params.get("wrap_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
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
            // Single-source: `in` is a GatherTexel input (4-neighbour central
            // difference via exact textureLoad, no sampler). Generated kernel
            // binds uniform(0)/tex(1)/dst(2); the body resolves the boundary
            // policy from `wrap_mode` itself (gcd_wrap_coord).
            // gradient_central_diff.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec_fmt::<Self>(out_fmt)
                    .expect("node.edge_slope standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.edge_slope",
            )
        });

        let uniforms = GradientUniforms {
            channel,
            scale_mode,
            wrap_mode,
            _pad0: 0,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.edge_slope",
        );
    }
}

/// D6(a) follow-up (`docs/DEPTH_RELIGHT_DESIGN.md`): proves the `Gather` →
/// `GatherTexel` conversion is value-preserving for BOTH boundary policies
/// (Clamp and — the load-bearing one, FluidSim2D ships `wrap_mode = Repeat`
/// — Repeat), and that the generated kernel matches the hand-maintained
/// oracle `gradient_central_diff.wgsl`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_gpu::{
        GpuAddressMode, GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture,
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{GradientCentralDiff, GradientUniforms};
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{generate_standalone, StandaloneKernelSpec, ENTRY};
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::render_target::RenderTarget;

    /// Verbatim copy of the pre-conversion `gradient_central_diff_body.wgsl`
    /// — frozen here ONLY as an old-vs-new comparison fixture, never used as
    /// a runtime kernel.
    const OLD_GATHER_SAMPLER_BODY: &str = r#"
fn gcd_select_channel(c: vec4<f32>, idx: u32) -> f32 {
    switch idx {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        default: { return c.a; }
    }
}
fn body(in_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, channel: u32, scale_mode: u32, wrap_mode: u32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;
    let cL = textureSampleLevel(in_tex, samp, uv + vec2<f32>(-inv.x, 0.0), 0.0);
    let cR = textureSampleLevel(in_tex, samp, uv + vec2<f32>( inv.x, 0.0), 0.0);
    let cD = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0, -inv.y), 0.0);
    let cU = textureSampleLevel(in_tex, samp, uv + vec2<f32>(0.0,  inv.y), 0.0);
    let diff_x = gcd_select_channel(cR, channel) - gcd_select_channel(cL, channel);
    let diff_y = gcd_select_channel(cU, channel) - gcd_select_channel(cD, channel);
    let scale_xy = select(
        vec2<f32>(0.5, 0.5),
        vec2<f32>(dims.x * 0.5, dims.y * 0.5),
        scale_mode == 1u,
    );
    let dx = diff_x * scale_xy.x;
    let dy = diff_y * scale_xy.y;
    return vec4<f32>(dx, dy, 0.0, 1.0);
}
"#;

    fn upload_field(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
        assert_eq!(raw.len(), (w * h) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "edge-slope-field",
            mip_levels: 1,
        });
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, &v) in raw.iter().enumerate() {
            px[i * 4] = f16::from_f32(v);
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(&px[..]))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("edge-slope-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    /// Non-periodic-looking synthetic field (so a Repeat wrap genuinely
    /// exercises different values than Clamp at the edges).
    fn synthetic_field(w: u32, h: u32) -> Vec<f32> {
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let fx = x as f32 / (w.max(1) - 1).max(1) as f32;
                    let fy = y as f32 / (h.max(1) - 1).max(1) as f32;
                    0.1 + 0.6 * fx * fx + 0.2 * fy
                })
            })
            .collect()
    }

    fn dispatch_uniform(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding<'_>],
        w: u32,
        h: u32,
    ) {
        let mut enc = device.create_encoder("edge-slope-dispatch");
        enc.dispatch_compute(pipeline, bindings, [w.div_ceil(16), h.div_ceil(16), 1], "test");
        enc.commit_and_wait_completed();
    }

    fn assert_pixels_close(a: &[[f32; 4]], b: &[[f32; 4]], tol: f32, label: &str) {
        assert_eq!(a.len(), b.len());
        for (i, (pa, pb)) in a.iter().zip(b.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (pa[c] - pb[c]).abs() < tol,
                    "{label}: texel {i} channel {c}: a={} b={} (tol {tol})",
                    pa[c],
                    pb[c]
                );
            }
        }
    }

    /// Runs the OLD (Gather, filtering sampler at the given address mode)
    /// and NEW (GatherTexel, `wrap_mode` uniform) kernels against the same
    /// field and asserts they agree.
    fn assert_old_matches_new(address_mode: GpuAddressMode, wrap_mode: u32, label: &str) {
        let device = crate::test_device();
        let (w, h) = (17u32, 11u32); // odd, non-square — exercises both axes' edges
        let raw = synthetic_field(w, h);
        let field_tex = upload_field(&device, w, h, &raw);
        let sampler = device.create_sampler(&GpuSamplerDesc {
            address_mode_u: address_mode,
            address_mode_v: address_mode,
            address_mode_w: address_mode,
            ..Default::default()
        });

        let uniforms = GradientUniforms { channel: 0, scale_mode: 1, wrap_mode, _pad0: 0 };
        let bytes = bytemuck::bytes_of(&uniforms);

        let old_wgsl = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: FusionKind::Pointwise,
            body: OLD_GATHER_SAMPLER_BODY,
            inputs: GradientCentralDiff::INPUTS,
            params: GradientCentralDiff::PARAMS,
            input_access: &[InputAccess::Gather],
            derived_uniforms: GradientCentralDiff::DERIVED_UNIFORMS,
            outputs: GradientCentralDiff::OUTPUTS,
            stencil_fetch: false,
            includes: &[],
        })
        .expect("old Gather-sampler standalone codegen");
        let old_pipeline = device.create_compute_pipeline(&old_wgsl, ENTRY, "edge-slope-old");
        let old_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "old-out");
        dispatch_uniform(
            &device,
            &old_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &field_tex },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &old_out.texture },
            ],
            w,
            h,
        );
        let old_pixels = readback_rgba(&device, &old_out.texture, w, h);

        let new_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<GradientCentralDiff>()
                .expect("new GatherTexel standalone codegen");
        let new_pipeline = device.create_compute_pipeline(&new_wgsl, ENTRY, "edge-slope-new");
        let new_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "new-out");
        dispatch_uniform(
            &device,
            &new_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &field_tex },
                GpuBinding::Texture { binding: 2, texture: &new_out.texture },
            ],
            w,
            h,
        );
        let new_pixels = readback_rgba(&device, &new_out.texture, w, h);

        assert_pixels_close(&old_pixels, &new_pixels, 1e-4, label);
    }

    #[test]
    fn gather_texel_conversion_is_value_preserving_clamp() {
        assert_old_matches_new(GpuAddressMode::ClampToEdge, 0, "old Clamp-sampler vs new GatherTexel+clamp");
    }

    /// The load-bearing case: FluidSim2D.json ships `node.edge_slope` with
    /// `wrap_mode = Repeat` (its flow field's toroidal density read) — this
    /// is the exact behavior a naive clamp-everywhere conversion would have
    /// silently broken.
    #[test]
    fn gather_texel_conversion_is_value_preserving_repeat() {
        assert_old_matches_new(GpuAddressMode::Repeat, 1, "old Repeat-sampler vs new GatherTexel+modulo");
    }

}
