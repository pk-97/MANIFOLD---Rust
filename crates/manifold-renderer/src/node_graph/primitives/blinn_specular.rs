//! `node.shininess` — Blinn-Phong specular from a tangent-space
//! normal + light + view. Pair with `node.matcap_two_tone` (base) +
//! `node.rim_light` (rim) for full stylised PBR layering.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// WGSL alignment: each vec3 occupies 16 bytes (12 data + 4 pad). Total 48.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
// Field order MUST match the PARAMS declaration order below — the runtime
// pipeline is the codegen-generated kernel (`standalone_for_spec`), whose
// uniform layout is derived from PARAMS in order. BUG (found P7, 2026-07-18):
// this struct had `power` 4th while PARAMS declare view_x/y/z before `power`,
// so the standalone dispatch fed the kernel view=(48,0,0), power=1.0 — the
// production "weird tints" specular. The gpu_tests parity oracle packs its
// own bytes per kernel, so it never exercised THIS struct.
struct BlinnUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    view_x: f32,
    view_y: f32,
    view_z: f32,
    power: f32,
    // A Color param expands to four CONSECUTIVE f32 fields in the generated
    // layout (no vec4 alignment), so color sits directly after `power`; the
    // trailing pad rounds the buffer to naga's 16-byte uniform size multiple.
    color: [f32; 4],
    _pad0: f32,
}

crate::primitive! {
    name: BlinnSpecular,
    type_id: "node.shininess",
    purpose: "Blinn-Phong specular from a tangent-space normal map + directional light + view: `h = normalize(light + view); spec = pow(max(dot(n, h), 0), power)`. ADDITIVE — sum with a base shading via `node.compose` mode=Add. Defaults match oily-fluid PBR (light=(0.35,0.55,0.75), view=(0,0,1), power=48, near-white tint). Wire a `node.light` into `light` to drive direction + colour from one source instead of scattered scalars; the wired light's colour multiplies the `color` tint param.",
    inputs: {
        normal: Texture2D required,
        light: Light optional,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
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
            name: Cow::Borrowed("light_x"),
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.35),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_y"),
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_z"),
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
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
            default: ParamValue::Float(48.0),
            range: Some((1.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 0.95, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    // depth_rule: reads a normal map (not color) and a Light; pointwise same-texel shading calc with no UV remap, classified structurally like the other lighting atoms (basic_light, rim_light, matcap_two_tone)
    depth_rule: Inherit,
    composition_notes: "Both light and view are normalised in-shader. Color alpha is ignored; output alpha = spec weight. For audio-reactive sparkle, wire a beat-driven LFO into `power` (lower power = broader highlight; higher = pinpoint). When `light` is wired, the light's direction overrides `light_x/y/z`; the light's pre-multiplied colour multiplies into the `color` tint (so a yellow light through a magenta surface gives a yellow×magenta highlight).",
    examples: [],
    picker: { label: "Shininess (Blinn)", category: Atom },
    summary: "Adds a tight highlight where the surface catches the light, set by a shininess amount. The glossy hotspot on top of basic lighting.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["specular", "shininess", "blinn specular", "highlight", "blinn"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/blinn_specular_body.wgsl"),
}

impl Primitive for BlinnSpecular {
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
        let mut light_x = read("light_x", 0.35);
        let mut light_y = read("light_y", 0.55);
        let mut light_z = read("light_z", 0.75);
        let view_x = read("view_x", 0.0);
        let view_y = read("view_y", 0.0);
        let view_z = read("view_z", 1.0);
        let power = read("power", 48.0);
        let mut color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [1.0, 0.95, 1.0, 1.0],
        };

        // Wired `node.light` overrides direction (negate `light.dir`
        // to match the existing scalar convention) and multiplies into
        // the colour tint. No world_pos mode on blinn — point lights
        // collapse to their forward direction at every pixel.
        if let Some(light) = ctx.inputs.light("light") {
            light_x = -light.dir[0];
            light_y = -light.dir[1];
            light_z = -light.dir[2];
            color = [
                color[0] * light.color[0],
                color[1] * light.color[1],
                color[2] * light.color[2],
                color[3],
            ];
        }

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
            // blinn_specular.wgsl` is retained only as the gpu_tests parity
            // oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.shininess standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.shininess",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = BlinnUniforms {
            light_x,
            light_y,
            light_z,
            power,
            view_x,
            view_y,
            view_z,
            _pad0: 0.0,
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
            "node.shininess",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (`docs/ADDING_PRIMITIVES.md` "The codegen
    //! path is mandatory") — the standalone kernel `run()` actually dispatches
    //! (built via `standalone_for_spec::<BlinnSpecular>()`) must reproduce
    //! `shaders/blinn_specular.wgsl` (the hand oracle) texel-for-texel on a
    //! synthetic non-uniform tangent-space normal map. Also the first
    //! standalone-path exercise of a `ParamType::Color` param end to end.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{BlinnSpecular, BlinnUniforms};
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

    /// Non-uniform tangent-space normal map: a hemisphere-ish spread of
    /// unit normals varying across x/y so the dot(n, h) term is exercised at
    /// many angles, not just a flat +Z fill.
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
        upload_rgba16f(device, w, h, "blinn-normal-map", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("blinn-readback");
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

    /// Dispatch a blinn_specular-shaped kernel (uniform(0), normal(1,
    /// sampler-read), sampler(2), dst(3)) and read back the full RGBA output.
    fn dispatch_blinn(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        normal: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "blinn-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("blinn-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: normal },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "blinn-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// Pack a flat f32 word list into a 16-byte-aligned little-endian buffer
    /// (zero-padded to the next multiple of 4 words) — matches the
    /// generated `Params` struct's layout (`freeze/codegen.rs`'s
    /// PARAMS-declaration-order emission), which is NOT the same field
    /// order as the hand shader's own bespoke `Uniforms` struct (the hand
    /// shader groups `light`/`power`/`view` for readability; PARAMS
    /// declares `light_x/y/z, view_x/y/z, power, color` in a different
    /// order) — so hand and generated dispatches need their OWN correctly
    /// laid-out byte buffers, same pattern as `displace_mesh.rs`'s
    /// hand-vs-generated buffer-layout split.
    fn pack_params(words: &[f32]) -> Vec<u8> {
        let mut v: Vec<f32> = words.to_vec();
        while !v.len().is_multiple_of(4) {
            v.push(0.0);
        }
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[test]
    fn generated_blinn_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (16u32, 8u32);
        let normal = normal_map(&device, w, h);

        let (light_x, light_y, light_z) = (0.35_f32, 0.55, 0.75);
        let (view_x, view_y, view_z) = (0.1_f32, -0.2, 1.0);
        let power = 24.0_f32;
        let color = [0.9_f32, 0.4, 0.7, 1.0];

        // Hand-oracle layout (vec3 light + power, vec3 view + pad, vec4 color)
        // — NOT `BlinnUniforms`, which follows the generated PARAMS-order
        // layout that `run()` dispatches (the P7 fix).
        let hand_bytes = pack_params(&[
            light_x, light_y, light_z, power, view_x, view_y, view_z, 0.0, color[0], color[1],
            color[2], color[3],
        ]);

        let gen_bytes = pack_params(&[
            light_x, light_y, light_z, view_x, view_y, view_z, power, color[0], color[1],
            color[2], color[3],
        ]);

        let hand_wgsl = include_str!("shaders/blinn_specular.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "blinn-hand");
        let hand_out = dispatch_blinn(&device, &hand_pipeline, &normal, w, h, &hand_bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<BlinnSpecular>()
            .expect("node.shininess standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "blinn-generated",
        );
        let gen_out = dispatch_blinn(&device, &gen_pipeline, &normal, w, h, &gen_bytes);

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
