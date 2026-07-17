//! `node.matcap_two_tone` — cross-axis 4-colour matcap from a
//! tangent-space normal map.
//!
//! Two 2-tone gradients (one along normal.x, one along normal.y) summed
//! by axis. The stylised PBR base atom — pair upstream with
//! `node.surface_bumps` and downstream with `node.rim_light`
//! + `node.shininess` summed for the full PBR look.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatcapUniforms {
    color_y_low: [f32; 4],
    color_y_high: [f32; 4],
    color_x_low: [f32; 4],
    color_x_high: [f32; 4],
}

crate::primitive! {
    name: MatcapTwoTone,
    type_id: "node.matcap_two_tone",
    purpose: "Cross-axis 4-colour matcap from a tangent-space normal map. Per pixel: mc=n.xy*0.5+0.5, base=mix(y_low, y_high, mc.y), side=mix(x_low, x_high, mc.x), out=(base+side)*0.5. Two 2-tone gradients per axis combined for a 4-corner matcap look. Defaults reproduce oily-fluid's PBR base palette (deep purple → pale blue Y axis, magenta → teal X axis).",
    inputs: {
        normal: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color_y_low"),
            label: "Y Low (shadow)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.08, 0.05, 0.22, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_y_high"),
            label: "Y High (highlight)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.55, 0.75, 0.95, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_x_low"),
            label: "X Low (left)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.25, 0.10, 0.45, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_x_high"),
            label: "X High (right)",
            ty: ParamType::Color,
            default: ParamValue::Color([0.15, 0.55, 0.60, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Output is fully opaque RGB. Sum with `node.rim_light` (additive) and `node.shininess` (additive) via `node.compose` (mode=Add) to build the full stylised-PBR shading layer. For a single-axis 2-tone matcap, set the unused-axis colors equal (e.g. color_x_low = color_x_high) — the side contribution becomes a constant added to the base.",
    examples: [],
    picker: { label: "Matcap Two-Tone", category: Atom },
    summary: "Shades a surface by mapping its normals into a two-tone sphere lookup, a fast stylised material that needs no real lights.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["matcap", "two tone", "sphere map", "lit sphere"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matcap_two_tone_body.wgsl"),
}

impl Primitive for MatcapTwoTone {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_color = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Color(c)) => *c,
                _ => default,
            }
        };
        let color_y_low = read_color("color_y_low", [0.08, 0.05, 0.22, 1.0]);
        let color_y_high = read_color("color_y_high", [0.55, 0.75, 0.95, 1.0]);
        let color_x_low = read_color("color_x_low", [0.25, 0.10, 0.45, 1.0]);
        let color_x_high = read_color("color_x_high", [0.15, 0.55, 0.60, 1.0]);

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
            // matcap_two_tone.wgsl` is retained only as the gpu_tests parity
            // oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.matcap_two_tone standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.matcap_two_tone",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MatcapUniforms {
            color_y_low,
            color_y_high,
            color_x_low,
            color_x_high,
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
            "node.matcap_two_tone",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (`docs/ADDING_PRIMITIVES.md` "The codegen
    //! path is mandatory") — the standalone kernel `run()` actually dispatches
    //! (built via `standalone_for_spec::<MatcapTwoTone>()`) must reproduce
    //! `shaders/matcap_two_tone.wgsl` (the hand oracle) texel-for-texel on a
    //! synthetic non-uniform tangent-space normal map. Four `ParamType::Color`
    //! params in one atom — the heaviest standalone-path Color exercise.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{MatcapTwoTone, MatcapUniforms};
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
    /// `blinn_specular.rs`'s fixture, so the mc.x/mc.y matcap lookup spans
    /// the full [0,1] range on both axes.
    fn normal_map(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let nx = (x as f32 / (w.saturating_sub(1).max(1)) as f32) * 2.0 - 1.0;
                let ny = (y as f32 / (h.saturating_sub(1).max(1)) as f32) * 2.0 - 1.0;
                let nz = (1.0 - nx * nx * 0.3 - ny * ny * 0.3).max(0.1).sqrt();
                let n = [nx * 0.8, ny * 0.8, nz];
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                px[i] = f16::from_f32(n[0] / len);
                px[i + 1] = f16::from_f32(n[1] / len);
                px[i + 2] = f16::from_f32(n[2] / len);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "matcap-normal-map", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("matcap-readback");
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

    /// Dispatch a matcap_two_tone-shaped kernel (uniform(0), normal(1,
    /// sampler-read), sampler(2), dst(3)) and read back the full RGBA output.
    fn dispatch_matcap(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        normal: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "matcap-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("matcap-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: normal },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "matcap-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    #[test]
    fn generated_matcap_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (16u32, 8u32);
        let normal = normal_map(&device, w, h);

        let uniforms = MatcapUniforms {
            color_y_low: [0.08, 0.05, 0.22, 1.0],
            color_y_high: [0.55, 0.75, 0.95, 1.0],
            color_x_low: [0.25, 0.10, 0.45, 1.0],
            color_x_high: [0.15, 0.55, 0.60, 1.0],
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let hand_wgsl = include_str!("shaders/matcap_two_tone.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "matcap-hand");
        let hand_out = dispatch_matcap(&device, &hand_pipeline, &normal, w, h, bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatcapTwoTone>()
            .expect("node.matcap_two_tone standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "matcap-generated",
        );
        let gen_out = dispatch_matcap(&device, &gen_pipeline, &normal, w, h, bytes);

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
