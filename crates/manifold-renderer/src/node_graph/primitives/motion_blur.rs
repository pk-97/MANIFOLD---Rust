//! `node.motion_blur` — velocity-directed gather motion blur from a scene's
//! velocity buffer + a Camera's shutter angle (`docs/CINEMATIC_POST_DESIGN.md`
//! D4). The tail of DoF v1's `CinematicScene` chain — this atom does ONLY
//! the per-pixel smear gather, no CoC/SSAO composition (the no-fused-
//! monolith rule: motion blur is one dispatch, not folded into the DoF or
//! SSAO chains it sits after).
//!
//! Exact formula, no substitution:
//! ```text
//! smear_px = velocity_ndc * 0.5 * viewport * (shutter_angle / 360)   // clamped to +/- max_blur_px
//! out      = average of 8 equal-weight taps of `in`, evenly spaced
//!            (inclusive) from uv - smear_uv/2 to uv + smear_uv/2
//! ```
//! `shutter_angle = 0` (pinhole default) collapses every tap onto the same
//! texel, so the average equals that texel unchanged — an unlensed shutter
//! produces a bit-clean pass-through (invariant I2, the D4 twin of D1's
//! pinhole CoC invariant).
//!
//! `velocity` is GBUFFER's `node.render_scene` `velocity` output — Rg16Float
//! NDC-space `(dx, dy) = (ndc_now - ndc_prev)`, camera + rigid-object motion
//! only (`GBUFFER_DESIGN.md` §2 D5's documented v1 limitation: a re-scattered
//! instance array contributes no per-instance velocity). Read as
//! `CoincidentTexel` (own-texel, exact — a directional vector must never be
//! blended with a neighbour's). `in` (the color to smear) is read as
//! `Gather`: the tap coordinate is body-computed from the velocity-derived
//! smear vector, so the codegen can't pre-sample it into a register — the
//! body samples it itself via the stencil-fetch `fetch_in(uv)` free function
//! (same mechanism `node.variable_blur` uses for its own Gather inputs),
//! which additionally lets a fusable upstream pointwise producer (e.g. the
//! DoF/SSAO composited color immediately upstream in `CinematicScene`)
//! virtually chain into this dispatch instead of materializing first.
//!
//! `camera` reads ONLY `lens.shutter_angle` (the Camera's lens block,
//! written upstream by `node.camera_lens` — "one lens, every consumer
//! reads it", `docs/CAMERA_AND_LENS_DESIGN.md` D4) entirely via the one
//! `derived_uniforms` field below, never as a GPU binding (P0/D7). This
//! atom declares NO port-shadowed scalar of its own for `shutter_angle` —
//! `node.camera_lens` already exposes `shutter_angle` as a port-shadowed
//! param (its own composition_notes name exactly this: "a drop macro into
//! shutter_angle for a motion-blur smear"), and combining a port-shadow
//! override WITH a derived-uniforms Camera-fallback on the SAME field in
//! ONE atom is a composition that doesn't exist anywhere in this codebase
//! (verified: `node.coc_from_depth`'s `focus_distance`/`f_stop` and
//! `node.ssao_from_depth`'s `radius`/`intensity`/`bias` both read/declare
//! their P1/P2-era Camera-derived or plain-param fields with zero such
//! combination, and their landing reports both name the missing "wire
//! overrides camera-derived fallback" composition explicitly as unbuilt
//! compiler machinery). Building that composition here would be new
//! compiler work inside an atom-authoring phase — forbidden by this
//! phase's brief; the zero-new-mechanism, already-shipped resolution is
//! this atom reading the lens purely via derived_uniforms and the preset's
//! `shutter_angle` card binding directly to `node.camera_lens`, exactly
//! mirroring P1/P2's `focus_distance`/`f_stop`/`exposure_ev` cards. Flagged
//! here for Peter, same as P1/P2's equivalent calls.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `max_blur_px` param (f32, PARAMS
/// order) then the one DERIVED field (`shutter_angle`), padded to a
/// 16-byte (4-word) multiple — 2 real words + 2 pad = 16 bytes. Mirrors
/// `node.variable_blur`'s `BlurUniforms` (2 real + 2 pad) and
/// `coc_from_depth.rs`/`ssao_from_depth.rs`'s layout-note convention.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MotionBlurUniforms {
    max_blur_px: f32,
    shutter_angle: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: MotionBlur,
    type_id: "node.motion_blur",
    purpose: "Velocity-directed gather motion blur (thin-shutter model, docs/CINEMATIC_POST_DESIGN.md D4): smear_px = velocity_ndc * 0.5 * viewport * (shutter_angle/360), clamped to +/- max_blur_px; output is the average of 8 equal-weight taps of `in`, evenly spaced from uv - smear_uv/2 to uv + smear_uv/2. shutter_angle = 0 (pinhole default) collapses every tap onto the same texel, an exact pass-through. `velocity` expects render_scene's Rg16Float NDC-delta `velocity` output (own-texel read, never filtered). `camera` reads only the wired Camera's lens.shutter_angle (written by node.camera_lens) entirely via derived uniforms — the Camera wire is never a GPU binding, and this atom declares no port-shadowed scalar of its own (wire an LFO/beat envelope into node.camera_lens's own shutter_angle port for live control).",
    inputs: {
        in: Texture2D required,
        velocity: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_blur_px"),
            label: "Max Blur (px)",
            ty: ParamType::Float,
            default: ParamValue::Float(32.0),
            range: Some((0.0, 128.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Wire render_scene's `velocity` output straight into `velocity` (requires GBUFFER's emit_velocity path — lazy, costs nothing unless wired) and the Camera used for `node.camera_lens` into `camera` so the shutter reads the same lens exposure/DoF use. `samples` is a fixed WGSL const (8), not a runtime param — D4 commits the tap count. `max_blur_px` is a safety clamp on the smear distance in pixels, independent of `node.variable_blur`'s own `max_radius` (no shared-units contract with the DoF chain the way coc_from_depth has with variable_blur — motion blur and DoF are independent smears in this preset). Sits at the END of the CinematicScene chain, after the DoF+SSAO composited color, so its Gather `in` read can virtually stencil-fetch-chain that upstream pointwise work instead of materializing an extra intermediate.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Motion Blur", category: Atom },
    summary: "Smears each pixel along its own screen-space motion, scaled by the camera's shutter angle — the classic filmic motion-blur look, driven by real per-object movement instead of a post-process approximation.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["motion blur", "shutter blur", "velocity blur", "smear"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/motion_blur_body.wgsl"),
    input_access: [Gather, CoincidentTexel],
    stencil_fetch: true,
    derived_uniforms: ["shutter_angle"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `shutter_angle` field — reads the region's routed Camera
// external, matching `run()`'s own `cam.lens.shutter_angle` read below
// exactly (both are the single source of truth; there is no shared helper
// fn to factor out, unlike coc_from_depth/ssao_from_depth's multi-field
// `derive_*_scalars`, because this atom reads exactly one field).
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.motion_blur",
        recompute: |ctx| ctx.camera.map(|c| vec![c.lens.shutter_angle]),
    }
}

impl Primitive for MotionBlur {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_blur_px = match ctx.params.get("max_blur_px") {
            Some(ParamValue::Float(f)) => *f,
            _ => 32.0,
        };

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let shutter_angle = cam.lens.shutter_angle;

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(velocity_tex) = ctx.inputs.texture_2d("velocity") else {
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
            // Single-source: kernel generated from `wgsl_body` (`in` Gather
            // stencil-fetch, `velocity` CoincidentTexel; generated bindings
            // are uniform(0)/tex_in(1)/tex_velocity(2)/samp(3)/dst(4)).
            // motion_blur.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.motion_blur standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.motion_blur",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MotionBlurUniforms {
            max_blur_px,
            shutter_angle,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: velocity_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.motion_blur",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_in_velocity_camera_inputs_and_texture_output() {
        use crate::node_graph::ports::PortType;

        assert_eq!(MotionBlur::TYPE_ID, "node.motion_blur");
        let names: Vec<&str> = MotionBlur::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "velocity", "camera"]);
        assert_eq!(MotionBlur::INPUTS[0].ty, PortType::Texture2D);
        assert!(MotionBlur::INPUTS[0].required);
        assert_eq!(MotionBlur::INPUTS[1].ty, PortType::Texture2D);
        assert!(MotionBlur::INPUTS[1].required);
        assert_eq!(MotionBlur::INPUTS[2].ty, PortType::Camera);
        assert!(MotionBlur::INPUTS[2].required);

        assert_eq!(MotionBlur::OUTPUTS.len(), 1);
        assert_eq!(MotionBlur::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_max_blur_px_param_only() {
        let names: Vec<&str> = MotionBlur::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["max_blur_px"]);
    }

    #[test]
    fn declares_shutter_angle_as_sole_derived_uniform() {
        assert_eq!(MotionBlur::DERIVED_UNIFORMS, &["shutter_angle"]);
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<MotionBlurUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MotionBlur::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.motion_blur");
    }

    #[test]
    fn unregistered_before_this_module_now_has_a_recompute() {
        use crate::node_graph::freeze::derived_uniform_registry::has_recompute;
        assert!(has_recompute("node.motion_blur"));
    }
}

/// **CPU reference** (`docs/CINEMATIC_POST_DESIGN.md` P3 deliverable: "CPU
/// reference on a synthetic velocity ramp (I1)") — a plain-Rust
/// implementation of the D4 algorithm, independent of the WGSL body, used
/// by the I1 GPU-vs-CPU synthetic-ramp parity gpu_test further down.
#[cfg(test)]
pub(crate) mod cpu_reference {
    const MOTION_BLUR_SAMPLES: usize = 8;

    /// A synthetic RGBA color buffer, bilinear-sampled with CLAMP-TO-EDGE
    /// addressing (matching `textureSampleLevel`'s default sampler mode,
    /// `GpuSamplerDesc::default()`).
    pub struct ColorBuffer<'a> {
        pub w: i32,
        pub h: i32,
        pub rgba: &'a [[f32; 4]],
    }

    impl ColorBuffer<'_> {
        fn texel(&self, x: i32, y: i32) -> [f32; 4] {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.rgba[(cy * self.w + cx) as usize]
        }

        /// Bilinear sample at a UV coordinate, clamp-to-edge.
        fn sample(&self, u: f32, v: f32) -> [f32; 4] {
            let px = u * self.w as f32 - 0.5;
            let py = v * self.h as f32 - 0.5;
            let x0 = px.floor();
            let y0 = py.floor();
            let fx = px - x0;
            let fy = py - y0;
            let x0i = x0 as i32;
            let y0i = y0 as i32;
            let c00 = self.texel(x0i, y0i);
            let c10 = self.texel(x0i + 1, y0i);
            let c01 = self.texel(x0i, y0i + 1);
            let c11 = self.texel(x0i + 1, y0i + 1);
            let mut out = [0.0f32; 4];
            for c in 0..4 {
                let top = c00[c] * (1.0 - fx) + c10[c] * fx;
                let bot = c01[c] * (1.0 - fx) + c11[c] * fx;
                out[c] = top * (1.0 - fy) + bot * fy;
            }
            out
        }
    }

    /// The D4 algorithm, transcribed exactly (independent of the WGSL
    /// body) — one texel's motion-blurred output, `[r,g,b,a]`.
    #[allow(clippy::too_many_arguments)]
    pub fn motion_blur_texel(
        color: &ColorBuffer<'_>,
        velocity_ndc: [f32; 2],
        cx: i32,
        cy: i32,
        max_blur_px: f32,
        shutter_angle: f32,
    ) -> [f32; 4] {
        let dims = [color.w as f32, color.h as f32];
        let uv = [
            (cx as f32 + 0.5) / dims[0],
            (cy as f32 + 0.5) / dims[1],
        ];

        let smear_px_raw = [
            velocity_ndc[0] * 0.5 * dims[0] * (shutter_angle / 360.0),
            velocity_ndc[1] * 0.5 * dims[1] * (shutter_angle / 360.0),
        ];
        let smear_px = [
            smear_px_raw[0].clamp(-max_blur_px, max_blur_px),
            smear_px_raw[1].clamp(-max_blur_px, max_blur_px),
        ];
        let smear_uv = [smear_px[0] / dims[0], smear_px[1] / dims[1]];

        let mut acc = [0.0f32; 4];
        for i in 0..MOTION_BLUR_SAMPLES {
            let t = i as f32 / (MOTION_BLUR_SAMPLES - 1) as f32 - 0.5;
            let tap_u = uv[0] + smear_uv[0] * t;
            let tap_v = uv[1] + smear_uv[1] * t;
            let s = color.sample(tap_u, tap_v);
            for c in 0..4 {
                acc[c] += s[c];
            }
        }
        for c in acc.iter_mut() {
            *c /= MOTION_BLUR_SAMPLES as f32;
        }
        acc
    }
}

/// **Analytic sanity** (pure CPU, no GPU device — mirrors
/// `coc_from_depth.rs`'s `hand_computed_coc` / `ssao_from_depth.rs`'s
/// `analytic_sanity` "CPU-only formula check" pattern): `shutter_angle = 0`
/// must collapse every one of the 8 taps onto the exact same UV, so
/// `cpu_reference::motion_blur_texel`'s output equals that texel's own
/// color unchanged — the CPU-side proof of I2, independent of the GPU
/// kernel (`gpu_tests::zero_shutter_angle_is_bit_clean_passthrough` proves
/// the same invariant on the shipping kernel).
#[cfg(test)]
mod analytic_sanity {
    use super::cpu_reference::{motion_blur_texel, ColorBuffer};

    #[test]
    fn zero_shutter_angle_collapses_every_tap_to_the_center_texel() {
        let (w, h) = (8i32, 8i32);
        let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                rgba[i] = [x as f32 / w as f32, y as f32 / h as f32, 0.5, 1.0];
            }
        }
        let color = ColorBuffer { w, h, rgba: &rgba };

        for y in 0..h {
            for x in 0..w {
                let expected = rgba[(y * w + x) as usize];
                // A large, non-degenerate velocity — if shutter_angle=0
                // didn't zero the smear term, this would visibly blur.
                let got = motion_blur_texel(&color, [0.3, -0.25], x, y, 32.0, 0.0);
                for c in 0..4 {
                    assert!(
                        (got[c] - expected[c]).abs() < 1e-5,
                        "texel ({x},{y}) channel {c}: shutter=0 must reproduce the center texel exactly, expected={} got={}",
                        expected[c],
                        got[c]
                    );
                }
            }
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I1** (`docs/ADDING_PRIMITIVES.md` "The codegen path is mandatory" +
    //! `docs/CINEMATIC_POST_DESIGN.md` P3 deliverable): the generated
    //! standalone kernel (built via `standalone_for_spec::<MotionBlur>()`,
    //! the one that ships) must reproduce BOTH `cpu_reference::motion_blur_texel`
    //! (the plain-Rust reference, I1) and the hand-authored `motion_blur.wgsl`
    //! oracle (the codegen-path proof) texel-for-texel on a synthetic
    //! non-uniform velocity ramp.
    //!
    //! **I2**: `shutter_angle = 0` must be an exact pass-through of `in`
    //! (I2a, direct on the generated kernel) — mirrors
    //! `coc_from_depth.rs`'s `pinhole_dof_chain_is_bit_clean_passthrough`
    //! pattern, one dispatch instead of a 3-node chain since motion_blur has
    //! no downstream stage to chain through.
    //!
    //! **I5**: preset load-smoke lives in a separate integration test file
    //! (`tests/`), per the doc's `gpu_proofs` binary convention, not here.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::cpu_reference::{motion_blur_texel, ColorBuffer};
    use super::{MotionBlur, MotionBlurUniforms};
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

    /// Synthetic velocity ramp: NDC delta varies smoothly in x AND y (both
    /// components non-zero and non-uniform, up to +/-0.2 NDC units — a
    /// visible but not extreme per-frame motion) so a per-texel or
    /// per-axis bug can't hide behind a flat/degenerate fixture.
    fn velocity_ramp(w: u32, h: u32) -> Vec<[f32; 2]> {
        let mut v = vec![[0.0f32, 0.0f32]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                let fy = y as f32 / (h.saturating_sub(1).max(1)) as f32;
                v[(y * w + x) as usize] = [(fx - 0.5) * 0.4, (fy - 0.5) * 0.4];
            }
        }
        v
    }

    fn upload_velocity(device: &GpuDevice, w: u32, h: u32, v: &[[f32; 2]]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, vv) in v.iter().enumerate() {
            px[i * 4] = f16::from_f32(vv[0]);
            px[i * 4 + 1] = f16::from_f32(vv[1]);
            px[i * 4 + 2] = f16::from_f32(0.0);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, "motion-blur-velocity-ramp", &px)
    }

    /// A non-uniform RGBA gradient — the color input every test dispatches
    /// motion_blur against.
    fn color_gradient(device: &GpuDevice, w: u32, h: u32) -> (GpuTexture, Vec<[f32; 4]>) {
        let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let r = x as f32 / w as f32;
                let g = y as f32 / h as f32;
                let b = 0.5;
                let a = 1.0;
                rgba[i] = [r, g, b, a];
                px[i * 4] = f16::from_f32(r);
                px[i * 4 + 1] = f16::from_f32(g);
                px[i * 4 + 2] = f16::from_f32(b);
                px[i * 4 + 3] = f16::from_f32(a);
            }
        }
        (upload_rgba16f(device, w, h, "motion-blur-color-gradient", &px), rgba)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("motion-blur-readback");
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

    fn mb_uniforms(max_blur_px: f32, shutter_angle: f32) -> MotionBlurUniforms {
        MotionBlurUniforms { max_blur_px, shutter_angle, _pad0: 0.0, _pad1: 0.0 }
    }

    fn dispatch(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        sampler: &manifold_gpu::GpuSampler,
        color: &GpuTexture,
        velocity: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "motion-blur-out");
        let mut enc = device.create_encoder("motion-blur-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: color },
                GpuBinding::Texture { binding: 2, texture: velocity },
                GpuBinding::Sampler { binding: 3, sampler },
                GpuBinding::Texture { binding: 4, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "motion-blur-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// **I1a**: generated kernel vs CPU-Rust reference on the synthetic
    /// velocity ramp — the doc's house pattern (implemented twice from the
    /// same committed spec, compared pixel-for-pixel).
    #[test]
    fn generated_motion_blur_matches_cpu_reference_on_synthetic_ramp() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let velocity = velocity_ramp(w, h);
        let velocity_tex = upload_velocity(&device, w, h, &velocity);
        let (color_tex, color_rgba) = color_gradient(&device, w, h);

        let (max_blur_px, shutter_angle) = (32.0f32, 180.0f32);
        let uniforms = mb_uniforms(max_blur_px, shutter_angle);
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MotionBlur>()
            .expect("node.motion_blur standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "motion-blur-generated",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let gen_out = dispatch(&device, &pipeline, &sampler, &color_tex, &velocity_tex, w, h, bytes);

        let color_buf = ColorBuffer { w: w as i32, h: h as i32, rgba: &color_rgba };
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let idx = (y as u32 * w + x as u32) as usize;
                let vel = velocity[idx];
                let cpu = motion_blur_texel(&color_buf, vel, x, y, max_blur_px, shutter_angle);
                let gpu = gen_out[idx];
                for c in 0..4 {
                    assert!(
                        (cpu[c] - gpu[c]).abs() < 2e-3,
                        "texel ({x},{y}) channel {c}: cpu={} gpu={}",
                        cpu[c],
                        gpu[c]
                    );
                }
            }
        }
    }

    /// **I1b** (`docs/ADDING_PRIMITIVES.md` "The codegen path is
    /// mandatory"): generated kernel vs the hand-authored `motion_blur.wgsl`
    /// oracle — same fixture, independent WGSL source, proves the codegen
    /// path itself (not just the algorithm) is correct.
    #[test]
    fn generated_motion_blur_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let velocity = velocity_ramp(w, h);
        let velocity_tex = upload_velocity(&device, w, h, &velocity);
        let (color_tex, _color_rgba) = color_gradient(&device, w, h);

        let uniforms = mb_uniforms(32.0, 180.0);
        let bytes = bytemuck::bytes_of(&uniforms);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_wgsl = include_str!("shaders/motion_blur.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "motion-blur-hand");
        let hand_out = dispatch(&device, &hand_pipeline, &sampler, &color_tex, &velocity_tex, w, h, bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MotionBlur>()
            .expect("node.motion_blur standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "motion-blur-generated-vs-hand",
        );
        let gen_out = dispatch(&device, &gen_pipeline, &sampler, &color_tex, &velocity_tex, w, h, bytes);

        assert_eq!(hand_out.len(), gen_out.len());
        for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (h_px[c] - g_px[c]).abs() < 1e-4,
                    "texel {i} channel {c}: hand={} gen={}",
                    h_px[c],
                    g_px[c]
                );
            }
        }
    }

    /// **I2**: `shutter_angle = 0` is an exact pass-through of `in` — every
    /// tap collapses onto the center texel regardless of the velocity
    /// buffer underneath it (a non-uniform velocity ramp, so this isn't
    /// hiding behind an all-zero fixture).
    #[test]
    fn zero_shutter_angle_is_bit_clean_passthrough() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let velocity = velocity_ramp(w, h);
        let velocity_tex = upload_velocity(&device, w, h, &velocity);
        let (color_tex, _color_rgba) = color_gradient(&device, w, h);

        let uniforms = mb_uniforms(32.0, 0.0);
        let bytes = bytemuck::bytes_of(&uniforms);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MotionBlur>()
            .expect("node.motion_blur standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "motion-blur-zero-shutter",
        );
        let got = dispatch(&device, &pipeline, &sampler, &color_tex, &velocity_tex, w, h, bytes);
        let expected = readback_rgba(&device, &color_tex, w, h);

        assert_eq!(expected.len(), got.len());
        for (i, (e, g)) in expected.iter().zip(got.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (e[c] - g[c]).abs() < 1e-3,
                    "texel {i} channel {c}: shutter=0 must pass through bit-clean, expected={} got={}",
                    e[c],
                    g[c]
                );
            }
        }
    }
}
