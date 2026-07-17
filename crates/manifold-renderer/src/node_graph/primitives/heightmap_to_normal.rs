//! `node.surface_bumps` — scalar height field → unit normal map via
//! central-difference gradient.
//!
//! Reads `in.r` as height per pixel, computes `(dh/dx, dh/dy)` via
//! half-difference of adjacent samples. Two output coordinate spaces,
//! picked by the `coord_space` param:
//!
//! - **TangentZ** (default — flat-surface tangent-space convention used
//!   by `lambert_directional`, `matcap_two_tone`, `blinn_specular` and
//!   the rest of the OilyFluid-shaped screen-space PBR family):
//!   `N = normalize(-dh/dx, -dh/dy * aspect, z_scale)`. Surface-normal
//!   direction sits on the `.z` channel.
//!
//! - **WorldYUp** (3D mesh laid out in the world XZ plane with Y up —
//!   matches the MetallicGlass full-resolution-reflection trick where a
//!   per-pixel surface normal is derived from the displaced height field
//!   rather than from the under-sampled vertex normals):
//!   `N = normalize(-dh/dx, z_scale, -dh/dy * aspect)`. Surface-normal
//!   direction sits on the `.y` channel.
//!
//! The `aspect` input scales the Y-axis gradient so non-square world
//! quads keep the right relative slope (default 1.0 = no correction).
//! Larger `z_scale` = flatter normals; smaller = steeper. Output is
//! signed (range [-1, 1] per channel), alpha = 1.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HeightmapNormalUniforms {
    z_scale: f32,
    aspect: f32,
    coord_space: u32, // 0 = TangentZ (default), 1 = WorldYUp
    _pad0: f32,
}

crate::primitive! {
    name: HeightmapToNormal,
    type_id: "node.surface_bumps",
    purpose: "Scalar height field (read from `in.r`) → unit normal map (RGB) via central-difference gradient. Coord_space picks the output convention: TangentZ for flat-surface tangent-space shading (OilyFluid lambert/matcap/blinn), WorldYUp for 3D meshes laid out in the XZ plane with Y up (MetallicGlass full-resolution-reflection trick). Larger `z_scale` flattens the normal; smaller steepens it. `aspect` scales the Y-axis gradient for non-square world quads. Output is SIGNED (range [-1, 1] per channel).",
    inputs: {
        in: Texture2D required,
        z_scale: ScalarF32 optional,
        aspect: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("z_scale"),
            label: "Z Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.001, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aspect"),
            label: "Aspect",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("coord_space"),
            label: "Coord Space",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &["TangentZ", "WorldYUp"],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Height is read from the R channel only. If your height is a derived quantity (e.g. `length(color.rg)` in the oily-fluid family) wire `node.vector_length` upstream first. TangentZ (default) pairs with `node.basic_light`, `node.matcap_two_tone`, `node.rim_light`, `node.shininess` for the flat-surface screen-space shading family. WorldYUp feeds `node.render_mesh`'s `normal_map` input for full PBR on a 3D mesh laid out in the XZ plane (the renderer's `node.pbr_material` owns the Cook-Torrance + IBL shading) — the MetallicGlass pattern. The `aspect` input is normally wired from `system.generator_input.aspect` so reflections stay correct across canvas aspect ratios.",
    examples: [],
    picker: { label: "Surface Bumps", category: Atom },
    summary: "Turns a grayscale height image into a normal map, so light and dark become bumps and dents the lighting can catch. The way to add surface detail from a texture.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["surface bumps", "heightmap to normal", "normal map", "bump map", "heightmap"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/heightmap_to_normal_body.wgsl"),
    // D6(a) follow-up (DEPTH_RELIGHT_DESIGN.md, root-cause fix requested
    // after the P4 audit): converted from `Gather` (filtering-sampler
    // `textureSampleLevel`) to `GatherTexel` (exact integer `textureLoad`,
    // manual ClampToEdge) — the 4 central-difference taps always land on
    // an exact texel center (offset is exactly ±1 texel from `uv =
    // (id+0.5)/dims`), so a filtering sampler and a texel-exact load agree
    // bit-for-bit; the conversion is value-preserving (proven by
    // `gpu_tests::gather_texel_conversion_is_value_preserving`, an old-vs-
    // new A/B dispatch on the same synthetic height field). This is what
    // lets `in` carry `precision_critical` — a non-filterable `Rgba32Float`
    // producer needs a texel-exact consumer to be safe on Apple GPUs.
    input_access: [GatherTexel],
    // Differentiates the height field via central difference — fp16's
    // ~10-bit mantissa quantizes the (hR-hL)/(hU-hD) difference into
    // visible normal-map banding on smooth gradients.
    precision_critical: ["in"],
}

impl Primitive for HeightmapToNormal {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let z_scale = read("z_scale", 0.5);
        let aspect = read("aspect", 1.0);
        let coord_space: u32 = match ctx.params.get("coord_space") {
            Some(ParamValue::Enum(v)) => *v,
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `in` is a GatherTexel input (4-neighbour central difference via
            // exact textureLoad, no sampler). Generated kernel binds
            // uniform(0)/tex(1)/dst(2). heightmap_to_normal.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.surface_bumps standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.surface_bumps",
            )
        });

        let uniforms = HeightmapNormalUniforms {
            z_scale,
            aspect,
            coord_space,
            _pad0: 0.0,
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
            "node.surface_bumps",
        );
    }
}

/// D6(a) follow-up (`docs/DEPTH_RELIGHT_DESIGN.md`): proves the `Gather` →
/// `GatherTexel` conversion is value-preserving, and that the generated
/// (GatherTexel) kernel matches the hand-maintained oracle
/// `heightmap_to_normal.wgsl`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{HeightmapNormalUniforms, HeightmapToNormal};
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{generate_standalone_ext, ENTRY};
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::render_target::RenderTarget;

    /// Verbatim copy of the pre-conversion `heightmap_to_normal_body.wgsl` —
    /// frozen here ONLY as an old-vs-new comparison fixture, never used as a
    /// runtime kernel. If this ever needs updating, the conversion under
    /// test has changed scope.
    const OLD_GATHER_SAMPLER_BODY: &str = r#"
fn body(tex_in: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, z_scale: f32, aspect: f32, coord_space: u32) -> vec4<f32> {
    let inv = vec2<f32>(1.0) / dims;
    let hL = textureSampleLevel(tex_in, samp, uv + vec2<f32>(-inv.x, 0.0), 0.0).r;
    let hR = textureSampleLevel(tex_in, samp, uv + vec2<f32>( inv.x, 0.0), 0.0).r;
    let hD = textureSampleLevel(tex_in, samp, uv + vec2<f32>(0.0, -inv.y), 0.0).r;
    let hU = textureSampleLevel(tex_in, samp, uv + vec2<f32>(0.0,  inv.y), 0.0).r;
    let gx = (hR - hL) * 0.5;
    let gy = (hU - hD) * 0.5 * aspect;
    let z = max(z_scale, 1e-4);
    var n: vec3<f32>;
    if coord_space == 1u {
        n = normalize(vec3<f32>(-gx, z, -gy));
    } else {
        n = normalize(vec3<f32>(-gx, -gy, z));
    }
    return vec4<f32>(n, 1.0);
}
"#;

    fn upload_height(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
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
            label: "surface-bumps-height",
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
        let mut enc = device.create_encoder("surface-bumps-readback");
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

    /// Smooth (non-degenerate-gradient) synthetic height field: enough
    /// texture to exercise every axis of the central difference without
    /// hitting a flat plateau.
    fn synthetic_height_field(w: u32, h: u32) -> Vec<f32> {
        (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let fx = x as f32 / (w.max(1) - 1).max(1) as f32;
                    let fy = y as f32 / (h.max(1) - 1).max(1) as f32;
                    0.2 + 0.5 * fx + 0.3 * fy * fy
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
        let mut enc = device.create_encoder("surface-bumps-dispatch");
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

    /// D6(a): the `Gather` (filtering-sampler) → `GatherTexel` (exact
    /// textureLoad) conversion must not change the output — every neighbour
    /// offset lands exactly on a texel center. Builds BOTH kernels from the
    /// SAME wrapper (`generate_standalone_ext`, same inputs/params/outputs),
    /// differing only in `input_access` + body text, and dispatches both
    /// against the same synthetic height field.
    #[test]
    fn gather_texel_conversion_is_value_preserving() {
        let device = crate::test_device();
        let (w, h) = (17u32, 11u32); // odd, non-square — exercises both axes' edges
        let raw = synthetic_height_field(w, h);
        let height_tex = upload_height(&device, w, h, &raw);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let uniforms = HeightmapNormalUniforms {
            z_scale: 0.35,
            aspect: 1.3,
            coord_space: 1, // WorldYUp — exercises the other branch too
            _pad0: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let old_wgsl = generate_standalone_ext(
            FusionKind::Pointwise,
            OLD_GATHER_SAMPLER_BODY,
            HeightmapToNormal::INPUTS,
            HeightmapToNormal::PARAMS,
            &[InputAccess::Gather],
            HeightmapToNormal::DERIVED_UNIFORMS,
            HeightmapToNormal::OUTPUTS,
            false,
            &[],
        )
        .expect("old Gather-sampler standalone codegen");
        let old_pipeline = device.create_compute_pipeline(&old_wgsl, ENTRY, "surface-bumps-old");
        let old_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "old-out");
        dispatch_uniform(
            &device,
            &old_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &height_tex },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &old_out.texture },
            ],
            w,
            h,
        );
        let old_pixels = readback_rgba(&device, &old_out.texture, w, h);

        let new_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<HeightmapToNormal>()
            .expect("new GatherTexel standalone codegen");
        let new_pipeline = device.create_compute_pipeline(&new_wgsl, ENTRY, "surface-bumps-new");
        let new_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "new-out");
        dispatch_uniform(
            &device,
            &new_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &height_tex },
                GpuBinding::Texture { binding: 2, texture: &new_out.texture },
            ],
            w,
            h,
        );
        let new_pixels = readback_rgba(&device, &new_out.texture, w, h);

        assert_pixels_close(&old_pixels, &new_pixels, 1e-4, "old Gather vs new GatherTexel");
    }

    /// `docs/ADDING_PRIMITIVES.md` codegen-path mandate: generated kernel
    /// matches the hand-maintained oracle `heightmap_to_normal.wgsl`.
    #[test]
    fn generated_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (13u32, 9u32);
        let raw = synthetic_height_field(w, h);
        let height_tex = upload_height(&device, w, h, &raw);

        let uniforms = HeightmapNormalUniforms {
            z_scale: 0.6,
            aspect: 0.8,
            coord_space: 0,
            _pad0: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let hand_wgsl = include_str!("shaders/heightmap_to_normal.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "surface-bumps-hand");
        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        dispatch_uniform(
            &device,
            &hand_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &height_tex },
                GpuBinding::Texture { binding: 2, texture: &hand_out.texture },
            ],
            w,
            h,
        );
        let hand_pixels = readback_rgba(&device, &hand_out.texture, w, h);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<HeightmapToNormal>()
            .expect("node.surface_bumps standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(&gen_wgsl, ENTRY, "surface-bumps-gen");
        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        dispatch_uniform(
            &device,
            &gen_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &height_tex },
                GpuBinding::Texture { binding: 2, texture: &gen_out.texture },
            ],
            w,
            h,
        );
        let gen_pixels = readback_rgba(&device, &gen_out.texture, w, h);

        assert_pixels_close(&hand_pixels, &gen_pixels, 1e-4, "hand vs generated");
    }
}
