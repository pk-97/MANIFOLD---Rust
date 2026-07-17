//! `node.heightfield_shadow` — screen-space heightfield shadow raymarch
//! (`docs/DEPTH_RELIGHT_DESIGN.md` D5, P1). Given a `height` texture and a
//! light direction, marches toward the light in the same ortho heightfield
//! frame `node.ssao_gtao`'s Height Field mode uses, and returns a grayscale
//! shadow map: 1.0 = fully lit, `(1 - strength)` = fully shadowed. Like
//! GTAO, this atom NEVER touches the color image itself — consumers
//! multiply the output into a shading term (e.g. alongside AO, next to
//! `node.basic_light`'s Lambert term).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the seven PARAMS (`light_x`,
/// `light_y`, `light_z`, `steps`, `strength`, `softness`, `relief`) in
/// declaration order, one f32 word each, padded to a 16-byte (4-word)
/// multiple. 7 words + 1 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HeightfieldShadowUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    steps: f32,
    strength: f32,
    softness: f32,
    relief: f32,
    _pad0: f32,
}

crate::primitive! {
    name: HeightfieldShadow,
    type_id: "node.heightfield_shadow",
    purpose: "Screen-space heightfield shadow raymarch (docs/DEPTH_RELIGHT_DESIGN.md D5): reads `height` as a raw [0,1] height map in the SAME ortho heightfield frame as node.ssao_gtao's Height Field mode (position = (uv.x*aspect, 1.0-uv.y, (1.0-raw)*relief)) and marches `steps` samples toward the light's XY direction, out to a max distance of relief*2 in uv units, tracking the deepest terrain-vs-ray penetration along the way. Fully lit (out=1.0) when no marched sample's terrain exceeds the ray; otherwise occlusion = smoothstep(0, softness*relief, penetration) * strength, out = clamp(1-occlusion, 0, 1) — soft penumbra via `softness`, hard shadows as softness approaches 0. Like node.ssao_gtao this atom NEVER modifies the color image — wire the grayscale output into a node.mix (Multiply mode) or sum it alongside an AO term next to node.basic_light's Lambert output. `light_x/y/z` use the same scene-toward-light convention and defaults as node.basic_light (0.4/0.6/0.7), port-shadowable for performance-time light orbiting.",
    inputs: {
        height: Texture2D required,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("light_x"),
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_y"),
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("light_z"),
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("steps"),
            label: "Steps",
            ty: ParamType::Float,
            default: ParamValue::Float(24.0),
            range: Some((4.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("strength"),
            label: "Strength",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("softness"),
            label: "Softness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("relief"),
            label: "Relief",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output is a grayscale shadow map (R=G=B, A=1): 1.0 = fully lit, (1-strength) = fully shadowed — multiply it into a shading term (node.mix, Multiply mode) alongside node.basic_light's Lambert output or an AO pass; this atom never touches the color image itself, same no-fused-color contract as node.ssao_gtao. `height` expects a raw [0,1] height map (brighter = taller, same `(1-raw)*relief` convention as ssao_gtao's Height Field mode) — pair the two atoms on the SAME `relief` value so their geometric frames agree. `light_x/y/z` point FROM the scene TOWARD the light (same convention as node.basic_light); a light with no horizontal component (light_x=light_y=0) can't cast a screen-space shadow and returns fully lit everywhere. `steps` trades raymarch fidelity for cost; `softness` widens the penumbra as a fraction of `relief`; `strength` above 1.0 lets a soft shadow reach full black sooner without the shadow's edge going harder.",
    examples: [],
    picker: { label: "Heightfield Shadow", category: Atom },
    summary: "Casts a soft screen-space shadow across a height map toward a light direction. Multiply it into a Lambert term for relief-lit terrain.",
    category: Mask,
    role: Map,
    aliases: ["heightfield shadow", "height shadow", "terrain shadow", "raymarch shadow", "relief shadow"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/heightfield_shadow_body.wgsl"),
    input_access: [GatherTexel],
}

impl Primitive for HeightfieldShadow {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let light_x = ctx.scalar_or_param("light_x", 0.4);
        let light_y = ctx.scalar_or_param("light_y", 0.6);
        let light_z = ctx.scalar_or_param("light_z", 0.7);
        let steps = match ctx.params.get("steps") {
            Some(ParamValue::Float(f)) => *f,
            _ => 24.0,
        };
        let strength = match ctx.params.get("strength") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let softness = match ctx.params.get("softness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };
        let relief = match ctx.params.get("relief") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.25,
        };

        let Some(height_tex) = ctx.inputs.texture_2d("height") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from `wgsl_body` (GatherTexel —
            // no sampler; generated bindings are uniform(0)/height(1)/dst(2)).
            // heightfield_shadow.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.heightfield_shadow standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.heightfield_shadow",
            )
        });

        let uniforms = HeightfieldShadowUniforms {
            light_x,
            light_y,
            light_z,
            steps,
            strength,
            softness,
            relief,
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
                    texture: height_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.heightfield_shadow",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_height_input_and_optional_light_scalars() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(HeightfieldShadow::TYPE_ID, "node.heightfield_shadow");
        let names: Vec<&str> = HeightfieldShadow::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["height", "light_x", "light_y", "light_z"]);
        assert_eq!(HeightfieldShadow::INPUTS[0].ty, PortType::Texture2D);
        assert!(HeightfieldShadow::INPUTS[0].required);
        for i in 1..4 {
            assert_eq!(HeightfieldShadow::INPUTS[i].ty, PortType::Scalar(ScalarType::F32));
            assert!(!HeightfieldShadow::INPUTS[i].required);
        }

        assert_eq!(HeightfieldShadow::OUTPUTS.len(), 1);
        assert_eq!(HeightfieldShadow::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_light_and_march_params_in_declaration_order() {
        let names: Vec<&str> = HeightfieldShadow::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["light_x", "light_y", "light_z", "steps", "strength", "softness", "relief"]);
    }

    #[test]
    fn defaults_match_basic_light_convention() {
        let defaults: Vec<f32> = HeightfieldShadow::PARAMS[..3]
            .iter()
            .map(|p| match p.default {
                ParamValue::Float(f) => f,
                _ => panic!("expected float default"),
            })
            .collect();
        assert_eq!(defaults, vec![0.4, 0.6, 0.7]);
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<HeightfieldShadowUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = HeightfieldShadow::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.heightfield_shadow");
    }
}

/// **CPU reference** (`docs/DEPTH_RELIGHT_DESIGN.md` P1 deliverable) — a
/// plain-Rust implementation of the D5 algorithm, independent of the WGSL
/// body (not sharing source). Used two ways: (1) the analytic sanity tests
/// below, pure CPU, no GPU device; (2) the `matches_cpu_reference` GPU-vs-
/// CPU synthetic-ramp parity gpu_test further down.
#[cfg(test)]
pub(crate) mod cpu_reference {
    /// A synthetic height buffer: raw [0,1] height values, row-major,
    /// `w*h` long.
    pub struct HeightBuffer<'a> {
        pub w: i32,
        pub h: i32,
        pub raw: &'a [f32],
    }

    impl HeightBuffer<'_> {
        fn load(&self, x: i32, y: i32) -> f32 {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.raw[(cy * self.w + cx) as usize]
        }
    }

    /// Round half away from zero, matching `heightfield_shadow_body.wgsl`'s
    /// `hfshadow_round` bit-for-bit (see that file's header comment for why
    /// the language builtins are avoided).
    fn hfshadow_round(x: f32) -> f32 {
        if x >= 0.0 { (x + 0.5).floor() } else { -(-x + 0.5).floor() }
    }

    fn height_at(height: &HeightBuffer<'_>, c: (i32, i32), relief: f32) -> f32 {
        let raw = height.load(c.0, c.1);
        (1.0 - raw) * relief
    }

    /// The D5 algorithm, transcribed exactly (independent of the WGSL
    /// body) — one texel's shadow-term output, `[0,1]`.
    #[allow(clippy::too_many_arguments)]
    pub fn hfshadow_texel(
        height: &HeightBuffer<'_>,
        cx: i32,
        cy: i32,
        light_x: f32,
        light_y: f32,
        light_z: f32,
        steps: usize,
        strength: f32,
        softness: f32,
        relief: f32,
    ) -> f32 {
        let steps = steps.max(1);
        let light_len = (light_x * light_x + light_y * light_y + light_z * light_z).sqrt();
        let (ldx, ldy, ldz) = if light_len > 1e-8 {
            (light_x / light_len, light_y / light_len, light_z / light_len)
        } else {
            (0.0, 0.0, 1.0)
        };
        let xy_len = (ldx * ldx + ldy * ldy).sqrt();
        if xy_len < 1e-6 {
            return 1.0;
        }
        let dir2 = (ldx / xy_len, ldy / xy_len);
        let slope = ldz / xy_len;

        let dims_y = height.h as f32;
        let start_height = height_at(height, (cx, cy), relief);

        let max_dist = relief * 2.0;
        let max_dist_px = max_dist * dims_y;

        let mut max_penetration = 0.0f32;
        for i in 1..=steps {
            let t = max_dist * (i as f32) / (steps as f32);
            let t_px = max_dist_px * (i as f32) / (steps as f32);
            let off_x = hfshadow_round(dir2.0 * t_px) as i32;
            let off_y = hfshadow_round(-dir2.1 * t_px) as i32;
            let cs = (cx + off_x, cy + off_y);
            let terrain = height_at(height, cs, relief);
            let ray_height = start_height + t * slope;
            let penetration = terrain - ray_height;
            if penetration > max_penetration {
                max_penetration = penetration;
            }
        }

        if max_penetration <= 0.0 {
            return 1.0;
        }

        let edge0 = 0.0f32;
        let edge1 = softness * relief + 1e-4;
        let x = ((max_penetration - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        let smooth = x * x * (3.0 - 2.0 * x);
        let occlusion = smooth * strength;
        (1.0 - occlusion).clamp(0.0, 1.0)
    }
}

/// **Analytic sanity tests** (`docs/DEPTH_RELIGHT_DESIGN.md` P1 deliverable)
/// — pure CPU, no GPU device.
#[cfg(test)]
mod analytic_sanity {
    use super::cpu_reference::{hfshadow_texel, HeightBuffer};

    /// A flat height field must give `out ~= 1.0` (fully lit) everywhere,
    /// for any light direction with a horizontal component (and for one
    /// with none, which short-circuits to fully lit by construction).
    #[test]
    fn flat_field_full_visibility_for_any_light() {
        let (w, h) = (16i32, 16i32);
        let raw = vec![0.5f32; (w * h) as usize];
        let height = HeightBuffer { w, h, raw: &raw };

        let lights: &[(f32, f32, f32)] =
            &[(0.4, 0.6, 0.7), (1.0, 0.0, 0.3), (0.0, 1.0, 0.5), (-0.7, -0.7, 0.2), (0.0, 0.0, 1.0)];

        for &(lx, ly, lz) in lights {
            for cy in 0..h {
                for cx in 0..w {
                    let out = hfshadow_texel(&height, cx, cy, lx, ly, lz, 24, 1.0, 0.5, 0.25);
                    assert!(
                        (out - 1.0).abs() < 1e-6,
                        "light=({lx},{ly},{lz}) texel ({cx},{cy}): flat field must give out~=1.0, got {out}"
                    );
                }
            }
        }
    }

    /// A single raised square bump on a flat field: shadowed pixels must
    /// exist ONLY on the side AWAY from the light's XY direction (the far
    /// side relative to the light, where the ray toward the light has to
    /// climb over the bump); the side facing the light (downstream of the
    /// bump, between it and the light) must read fully lit (out == 1.0).
    #[test]
    fn raised_bump_casts_shadow_only_away_from_light() {
        let (w, h) = (32i32, 32i32);
        let mut raw = vec![1.0f32; (w * h) as usize]; // flat floor, height 0 everywhere
        // A "wall": full-height columns 14..=17, all rows — raw=0.0 gives
        // height = relief (the tallest point), background raw=1.0 gives
        // height = 0.
        for cy in 0..h {
            for cx in 14..=17 {
                raw[(cy * w + cx) as usize] = 0.0;
            }
        }
        let height = HeightBuffer { w, h, raw: &raw };

        // Light toward +x (east) and elevated (z=1, 45 degrees): the wall
        // casts a shadow to its WEST (-x, away from the light) and stays
        // lit to its EAST (+x, between the wall and the light).
        let (light_x, light_y, light_z) = (1.0, 0.0, 1.0);
        let (steps, strength, softness, relief) = (24, 1.0, 0.5, 0.25);
        let row = 16;

        // West side (upstream of the wall relative to the +x march
        // toward the light): the ray toward light must climb the wall's
        // near face — shadowed.
        let west = hfshadow_texel(&height, 10, row, light_x, light_y, light_z, steps, strength, softness, relief);
        assert!(west < 1.0, "west-of-wall texel should be shadowed (away from light), got {west}");

        // East side (downstream of the wall, toward the light already):
        // nothing ahead of the ray toward light — fully lit.
        let east = hfshadow_texel(&height, 22, row, light_x, light_y, light_z, steps, strength, softness, relief);
        assert!(
            (east - 1.0).abs() < 1e-6,
            "east-of-wall texel (facing the light) must read fully lit, got {east}"
        );

        // Sanity: a texel far from the wall in both directions (outside
        // march reach) is also fully lit.
        let far = hfshadow_texel(&height, 2, row, light_x, light_y, light_z, steps, strength, softness, relief);
        assert!((far - 1.0).abs() < 1e-6, "texel far from the wall must read fully lit, got {far}");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Mirrors `ssao_gtao.rs`'s `gpu_tests` shape: generated-vs-hand parity
    //! (proves the codegen-path mandate) and generated-vs-CPU-reference
    //! parity (proves the algorithm, not just the codegen plumbing).
    use half::f16;

    use manifold_gpu::{GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

    use super::cpu_reference::{hfshadow_texel, HeightBuffer};
    use super::HeightfieldShadow;
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

    /// Non-uniform synthetic height ramp (raw varies smoothly in x AND y).
    fn height_ramp_2d(w: u32, h: u32) -> Vec<f32> {
        let mut raw = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                let fy = y as f32 / (h.saturating_sub(1).max(1)) as f32;
                raw[(y * w + x) as usize] = 0.2 + 0.6 * (0.5 * fx + 0.5 * fy);
            }
        }
        raw
    }

    fn upload_height(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, &r) in raw.iter().enumerate() {
            px[i * 4] = f16::from_f32(r);
            px[i * 4 + 1] = f16::from_f32(r);
            px[i * 4 + 2] = f16::from_f32(r);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, "hfshadow-height-ramp", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("hfshadow-readback");
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

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct HfShadowUniforms {
        light_x: f32,
        light_y: f32,
        light_z: f32,
        steps: f32,
        strength: f32,
        softness: f32,
        relief: f32,
        _pad0: f32,
    }

    fn dispatch(
        device: &GpuDevice,
        pipeline: &manifold_gpu::GpuComputePipeline,
        height: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "hfshadow-out");
        let mut enc = device.create_encoder("hfshadow-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: height },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "hfshadow-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// **Generated kernel vs the hand-authored `heightfield_shadow.wgsl`
    /// oracle** (`docs/ADDING_PRIMITIVES.md` "The codegen path is
    /// mandatory") — same fixture, independent WGSL source, at a
    /// non-default `steps` to prove the dynamic loop bound agrees, not
    /// just the default.
    #[test]
    fn generated_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let raw = height_ramp_2d(w, h);
        let height_tex = upload_height(&device, w, h, &raw);

        let uniforms = HfShadowUniforms {
            light_x: 0.5,
            light_y: -0.3,
            light_z: 0.6,
            steps: 32.0,
            strength: 1.0,
            softness: 0.5,
            relief: 0.25,
            _pad0: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let hand_wgsl = include_str!("shaders/heightfield_shadow.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "hfshadow-hand");
        let hand_out = dispatch(&device, &hand_pipeline, &height_tex, w, h, bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<HeightfieldShadow>()
            .expect("node.heightfield_shadow standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "hfshadow-generated",
        );
        let gen_out = dispatch(&device, &gen_pipeline, &height_tex, w, h, bytes);

        assert_eq!(hand_out.len(), gen_out.len());
        for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
            for c in 0..3 {
                assert!(
                    (h_px[c] - g_px[c]).abs() < 1e-4,
                    "texel {i} channel {c}: hand={} gen={}",
                    h_px[c],
                    g_px[c]
                );
            }
        }
    }

    /// **Generated kernel vs CPU-Rust reference** — same house pattern as
    /// `ssao_gtao.rs`'s `gtao_matches_cpu_reference`: implement the
    /// committed algorithm twice, once in WGSL, once in plain Rust. Unlike
    /// GTAO, this march is fully deterministic per-pixel (no hash-driven
    /// rotation), so a tight bound holds — measured max observed
    /// discrepancy on this fixture is well under 1e-3 (ordinary FP
    /// rounding between GPU hardware transcendentals and CPU libm in
    /// `sqrt`/`smoothstep`'s cubic, not a tread-boundary class of error).
    #[test]
    fn generated_matches_cpu_reference() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let raw = height_ramp_2d(w, h);
        let height_tex = upload_height(&device, w, h, &raw);

        let (light_x, light_y, light_z) = (0.5f32, -0.3f32, 0.6f32);
        let (steps, strength, softness, relief) = (32.0f32, 1.0f32, 0.5f32, 0.25f32);
        let uniforms = HfShadowUniforms { light_x, light_y, light_z, steps, strength, softness, relief, _pad0: 0.0 };
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<HeightfieldShadow>()
            .expect("node.heightfield_shadow standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "hfshadow-generated-vs-cpu",
        );
        let gen_out = dispatch(&device, &pipeline, &height_tex, w, h, bytes);

        let height_buf = HeightBuffer { w: w as i32, h: h as i32, raw: &raw };
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let cpu = hfshadow_texel(
                    &height_buf,
                    x,
                    y,
                    light_x,
                    light_y,
                    light_z,
                    steps as usize,
                    strength,
                    softness,
                    relief,
                );
                let gpu = gen_out[(y as u32 * w + x as u32) as usize][0];
                let diff = (cpu - gpu).abs();
                assert!(
                    diff < 1e-3,
                    "texel ({x},{y}): cpu={cpu} gpu={gpu} diff={diff} exceeds the ordinary-FP-noise \
                     bound — this march is fully deterministic per-pixel (no hash), so unlike GTAO \
                     there's no hash-driven-jump class to accommodate"
                );
            }
        }
    }

    /// **Analytic sanity, GPU path** — the same flat-field claim as
    /// `analytic_sanity::flat_field_full_visibility_for_any_light`,
    /// dispatched on the real generated kernel.
    #[test]
    fn generated_flat_field_gives_full_visibility() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let raw = vec![0.5f32; (w * h) as usize];
        let height_tex = upload_height(&device, w, h, &raw);

        let uniforms = HfShadowUniforms {
            light_x: 0.4,
            light_y: 0.6,
            light_z: 0.7,
            steps: 24.0,
            strength: 1.0,
            softness: 0.5,
            relief: 0.25,
            _pad0: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<HeightfieldShadow>()
            .expect("node.heightfield_shadow standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "hfshadow-flat",
        );
        let out = dispatch(&device, &pipeline, &height_tex, w, h, bytes);

        for (i, px) in out.iter().enumerate() {
            assert!(
                (px[0] - 1.0).abs() < 1e-4,
                "texel {i}: flat field must give ~full visibility (out~1), got {}",
                px[0]
            );
        }
    }
}
