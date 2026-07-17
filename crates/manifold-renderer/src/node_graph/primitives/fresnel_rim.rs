//! `node.rim_light` — fresnel-based edge highlight from a
//! tangent-space normal map. Black at face-on, `color`-tinted at
//! grazing. ADDITIVE — sum with a base shading via `node.compose`
//! mode=Add (or mode=Screen for HDR-safe).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FresnelUniforms {
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    color: [f32; 4],
}

crate::primitive! {
    name: FresnelRim,
    type_id: "node.rim_light",
    purpose: "Fresnel-based edge highlight from a tangent-space normal map: `f = pow(1 - max(dot(n, view), 0), power)`, output = color.rgb * f. ADDITIVE rim term — black at face-on, `color`-tinted at grazing. Sum with a base shading (matcap, lambert) via `node.compose` mode=Add to layer the rim onto the surface. Defaults match oily-fluid PBR (view=(0,0,1), power=3, color=iridescent magenta).",
    inputs: {
        normal: Texture2D required,
        view_x: ScalarF32 optional,
        view_y: ScalarF32 optional,
        view_z: ScalarF32 optional,
        power: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("view_x"),
            label: "View X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("view_y"),
            label: "View Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("view_z"),
            label: "View Z",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("power"),
            label: "Power",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.55, 0.30, 0.85, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "view defaults to (0, 0, 1) — camera looking down +Z. Higher `power` sharpens the rim. Color alpha is ignored; output alpha = fresnel weight (handy for compositing the rim only where it's actually contributing). Port-shadowed `power` lets you pulse the rim from a beat or LFO.",
    examples: [],
    picker: { label: "Rim Light (Fresnel)", category: Atom },
    summary: "Lights up the edges of a surface where it turns away from the camera, the glowing rim you see on backlit objects.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["rim light", "fresnel rim", "fresnel", "edge glow", "backlight"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/fresnel_rim_body.wgsl"),
}

impl Primitive for FresnelRim {
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
        let view_x = read("view_x", 0.0);
        let view_y = read("view_y", 0.0);
        let view_z = read("view_z", 1.0);
        let power = read("power", 3.0);
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.55, 0.30, 0.85, 1.0],
        };

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
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
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/
            // fresnel_rim.wgsl` is retained only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.rim_light standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rim_light",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = FresnelUniforms {
            view_x,
            view_y,
            view_z,
            power,
            color,
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
                    texture: normal,
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
            "node.rim_light",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (`docs/ADDING_PRIMITIVES.md` "The codegen
    //! path is mandatory") — the standalone kernel `run()` actually dispatches
    //! (built via `standalone_for_spec::<FresnelRim>()`) must reproduce
    //! `shaders/fresnel_rim.wgsl` (the hand oracle) texel-for-texel on a
    //! synthetic non-uniform tangent-space normal map.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{FresnelRim, FresnelUniforms};
    use crate::render_target::RenderTarget;

    fn upload_rgba16f(device: &GpuDevice, w: u32, h: u32, label: &str, px: &[f16]) -> GpuTexture {
        assert_eq!(px.len(), (w * h * 4) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Non-uniform tangent-space normal map — same construction as
    /// `blinn_specular.rs`'s fixture, so grazing-angle rim behaviour is
    /// exercised at many angles, not a flat +Z fill.
    fn normal_map(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let nx = (x as f32 / (w.saturating_sub(1).max(1)) as f32) * 2.0 - 1.0;
                let ny = (y as f32 / (h.saturating_sub(1).max(1)) as f32) * 2.0 - 1.0;
                let nz = (1.0 - nx * nx * 0.3 - ny * ny * 0.3).max(0.1).sqrt();
                let n = [nx * 0.4, ny * 0.4, nz];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                px[i] = f16::from_f32(n[0] / len);
                px[i + 1] = f16::from_f32(n[1] / len);
                px[i + 2] = f16::from_f32(n[2] / len);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "fresnel-normal-map", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("fresnel-readback");
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

    /// Dispatch a fresnel_rim-shaped kernel (uniform(0), normal(1,
    /// sampler-read), sampler(2), dst(3)) and read back the full RGBA output.
    fn dispatch_fresnel(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        normal: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "fresnel-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("fresnel-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: normal },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "fresnel-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    #[test]
    fn generated_fresnel_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (16u32, 8u32);
        let normal = normal_map(&device, w, h);

        let uniforms = FresnelUniforms {
            view_x: 0.1,
            view_y: -0.2,
            view_z: 1.0,
            power: 3.5,
            color: [0.55, 0.30, 0.85, 1.0],
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let hand_wgsl = include_str!("shaders/fresnel_rim.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "fresnel-hand");
        let hand_out = dispatch_fresnel(&device, &hand_pipeline, &normal, w, h, bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<FresnelRim>()
            .expect("node.rim_light standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "fresnel-generated",
        );
        let gen_out = dispatch_fresnel(&device, &gen_pipeline, &normal, w, h, bytes);

        assert_eq!(hand_out.len(), gen_out.len());
        for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (h_px[c] - g_px[c]).abs() < 2e-3,
                    "texel {i} channel {c}: hand={} gen={}",
                    h_px[c],
                    g_px[c]
                );
            }
        }
    }
}
