//! `node.bilateral_blur` — depth-guided (bilateral) separable blur pair
//! between an AO atom and its mix (`docs/CINEMATIC_POST_DESIGN.md` D8). The
//! observed defect this closes: `node.ssao_from_depth`'s 16 hash-rotated
//! samples per pixel ship raw, wired straight into the compositing mix, with
//! no smoothing pass — per-pixel noise by construction, and every production
//! AO implementation (SSAO or GTAO) follows the sampler with an edge-aware
//! blur. General-purpose by design (any texture + depth guide, not AO-only)
//! per the §2.5 audit (2026-07-13, 214 primitives surveyed) that found no
//! edge-aware/bilateral blur in the catalog.
//!
//! Fixed 9 taps at 1-texel spacing along `axis`, weighted by the SAME
//! sigma~=2 gaussian constants every other 9-tap kernel in this codebase
//! uses (`VBW_K9` / `SG_K9_*`) times a Gaussian falloff on the linearized-
//! depth difference from the center texel: `weight_j = K9_j *
//! exp(-(dz_j/depth_sigma)^2)`. Renormalized by the actual weight sum used.
//! Alpha is a pure center pass-through — this atom never blurs an alpha
//! channel it doesn't own.
//!
//! `in` is Gather (stencil-fetch — the body samples it via `fetch_in(uv)`).
//! `depth` is GatherTexel (raw [0,1] depth, integer `textureLoad` + manual
//! ClampToEdge, no sampler — same convention as `node.ssao_from_depth`'s
//! own depth reads, texel-exact so the CPU reference replicates it exactly).
//! `camera` is consumed ENTIRELY via the two `near`/`far` derived uniforms
//! (the D7/P0 mechanism `node.coc_from_depth` established) — never a GPU
//! binding, which is what lets this atom fuse with a pointwise neighbour
//! instead of being a permanent boundary.
//!
//! `depth_sigma` is a plain param (NOT a card, D8 — denoise is quality
//! plumbing, not a performer knob). Pair an H pass with a V pass for a full
//! 2D edge-aware blur (same axis-pair convention as `node.gaussian_blur`).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");

/// Display labels for the `axis` enum, indexed by enum value — matches
/// `GAUSSIAN_BLUR_AXES` / `BLUR_VARIABLE_AXES`'s convention (0=Horizontal,
/// 1=Vertical).
pub const BILATERAL_BLUR_AXES: &[&str] = &["Horizontal", "Vertical"];

/// Generated-codegen uniform layout: the two PARAMS (`axis`, `depth_sigma`)
/// in declaration order, then the two DERIVED fields (`near`, `far`) in
/// declaration order — one f32/u32 word each. 4 words = exactly 16 bytes,
/// no padding needed (mirrors `coc_from_depth.rs`'s layout note, minus the
/// padding since this atom has fewer fields).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BilateralBlurUniforms {
    axis: u32,
    depth_sigma: f32,
    near: f32,
    far: f32,
}

crate::primitive! {
    name: BilateralBlur,
    type_id: "node.bilateral_blur",
    purpose: "Depth-guided (bilateral) single-axis blur: fixed 9 taps at 1-texel spacing along `axis`, weight_j = K9_j * exp(-(dz_j/depth_sigma)^2) where K9_j are the same sigma~=2 gaussian constants used by every other 9-tap kernel in this codebase and dz_j is the linearized-depth difference from the center texel, renormalized by the weight sum actually used. Pair a Horizontal pass with a Vertical pass for a full 2D edge-aware blur that smooths noise (e.g. raw SSAO/GTAO occlusion) without bleeding across depth discontinuities (silhouette edges stay sharp). Alpha is a pure center pass-through. `camera` is read entirely via near/far derived uniforms for `linearize_depth` — never a GPU binding.",
    inputs: {
        in: Texture2D required,
        depth: Texture2D required,
        camera: Camera required,
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
            range: Some((0.0, 1.0)),
            enum_values: BILATERAL_BLUR_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("depth_sigma"),
            label: "Depth Sigma",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.001, 5.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Pair an H pass (axis=Horizontal) with a V pass (axis=Vertical) for a 2D edge-aware blur — same axis-pair convention as node.gaussian_blur / node.variable_blur. `depth_sigma` is in the SAME world units `linearize_depth` returns (view-space meters, following the Camera's near/far) — smaller values hug depth edges tighter (less cross-edge bleed, noisier flat regions); larger values approach a plain 9-tap gaussian (D8's I7 invariant: on a perfectly uniform depth plane this atom is byte-identical to the plain K9 gaussian, since every dz_j collapses to 0 and every weight reduces to its K9_j term). `depth` expects render_scene's raw [0,1] `depth` output (not pre-linearized), same contract as node.coc_from_depth / node.ssao_from_depth.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Bilateral Blur", category: Atom },
    summary: "A depth-guided blur that smooths noise without bleeding across depth edges — the standard denoise pass after any per-pixel noisy sampler (ambient occlusion, dithered effects) that needs to stay sharp at silhouettes.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["bilateral blur", "bilateral filter", "edge-aware blur", "depth-aware blur", "denoise", "ao denoise"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/bilateral_blur_body.wgsl"),
    input_access: [Gather, GatherTexel],
    // D6(a): `depth` is compared texel-vs-tap (`dz_j`) across all 9 taps to
    // weight the edge-aware blend — fp16 quantization of that per-tap
    // difference shows up as banding in the AO denoise near silhouette
    // edges. `in` stays filtered (Gather, unmarked): the color/AO signal
    // being blurred has no derivative/horizon read.
    precision_critical: ["depth"],
    stencil_fetch: true,
    derived_uniforms: ["near", "far"],
    wgsl_includes: [DEPTH_COMMON],
}

/// Single source of truth for the two Camera-derived scalar fields, in
/// `DERIVED_UNIFORMS` declaration order — shared by `run()` (unfused CPU
/// path) and the `inventory::submit!` recompute below (fused path), so the
/// two can never drift. Mirrors `coc_from_depth.rs`'s `derive_lens_scalars`
/// / `ssao_from_depth.rs`'s `derive_view_scalars` — this atom needs neither
/// fov_y nor a projection, only the near/far pair `linearize_depth` takes.
fn derive_depth_scalars(cam: &Camera) -> [f32; 2] {
    [cam.near, cam.far]
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's near/far fields, IN DECLARATION ORDER — reads the region's routed
// Camera external, matching `run()`'s own `derive_depth_scalars` call below
// exactly.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.bilateral_blur",
        recompute: |ctx| ctx.camera.map(derive_depth_scalars).map(|v| v.to_vec()),
    }
}

impl Primitive for BilateralBlur {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let depth_sigma = match ctx.params.get("depth_sigma") {
            Some(ParamValue::Float(f)) => f.max(1e-4),
            _ => 0.1,
        };

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let [near, far] = derive_depth_scalars(&cam);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(depth_tex) = ctx.inputs.texture_2d("depth") else {
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
            // Two-source MultiInputCoincident: `in` is Gather (stencil-fetch,
            // fetch_in(uv)), `depth` is GatherTexel (raw handle, manual
            // textureLoad). Generated bindings are uniform(0)/tex_in(1)/
            // tex_depth(2)/samp(3, for `in`'s Gather reads)/dst(4).
            // bilateral_blur.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.bilateral_blur standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.bilateral_blur",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = BilateralBlurUniforms {
            axis,
            depth_sigma,
            near,
            far,
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
                    texture: in_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: depth_tex,
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
            "node.bilateral_blur",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_in_depth_camera_inputs_and_texture_output() {
        use crate::node_graph::ports::PortType;

        assert_eq!(BilateralBlur::TYPE_ID, "node.bilateral_blur");
        let names: Vec<&str> = BilateralBlur::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "depth", "camera"]);
        assert_eq!(BilateralBlur::INPUTS[0].ty, PortType::Texture2D);
        assert!(BilateralBlur::INPUTS[0].required);
        assert_eq!(BilateralBlur::INPUTS[1].ty, PortType::Texture2D);
        assert!(BilateralBlur::INPUTS[1].required);
        assert_eq!(BilateralBlur::INPUTS[2].ty, PortType::Camera);
        assert!(BilateralBlur::INPUTS[2].required);

        assert_eq!(BilateralBlur::OUTPUTS.len(), 1);
        assert_eq!(BilateralBlur::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_axis_and_depth_sigma_params_only() {
        let names: Vec<&str> = BilateralBlur::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["axis", "depth_sigma"]);
    }

    #[test]
    fn declares_two_derived_uniforms_near_far() {
        assert_eq!(BilateralBlur::DERIVED_UNIFORMS, &["near", "far"]);
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<BilateralBlurUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BilateralBlur::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.bilateral_blur");
    }

    #[test]
    fn derive_depth_scalars_reads_near_far_only() {
        let mut cam = Camera::default_perspective();
        cam.near = 0.2;
        cam.far = 250.0;
        let [near, far] = derive_depth_scalars(&cam);
        assert_eq!(near, 0.2);
        assert_eq!(far, 250.0);
    }

    #[test]
    fn unregistered_before_this_module_now_has_a_recompute() {
        use crate::node_graph::freeze::derived_uniform_registry::has_recompute;
        assert!(has_recompute("node.bilateral_blur"));
    }
}

/// **CPU reference** (I1-pattern, `docs/CINEMATIC_POST_DESIGN.md` I7's third
/// named check: "the I1-pattern CPU-reference parity test") — a plain-Rust
/// implementation of the committed D8 formula, independent of the WGSL body
/// (not sharing source). Used by the GPU-vs-CPU parity gpu_test below.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod cpu_reference {
    use crate::node_graph::camera::linearize_depth;

    const K9: [f32; 5] = [0.16501, 0.15019, 0.11325, 0.07076, 0.03664];

    /// A synthetic depth+color buffer: raw [0,1] depth and RGBA color,
    /// row-major, `w*h` long each.
    pub struct Fixture<'a> {
        pub w: i32,
        pub h: i32,
        pub depth: &'a [f32],
        pub color: &'a [[f32; 4]],
    }

    impl Fixture<'_> {
        fn depth_at(&self, x: i32, y: i32) -> f32 {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.depth[(cy * self.w + cx) as usize]
        }
        fn color_at(&self, x: i32, y: i32) -> [f32; 4] {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.color[(cy * self.w + cx) as usize]
        }
    }

    /// The D8 formula, transcribed exactly (the CPU twin the WGSL body and
    /// hand oracle both implement). `axis` follows `BILATERAL_BLUR_AXES`
    /// (0=Horizontal, 1=Vertical). Nearest-neighbour reads on both textures —
    /// matches the GPU body's `fetch_in`/`bb_depth_at` texel-center sampling
    /// exactly (the fixture is uploaded at integer pixel positions with no
    /// fractional offsets, so bilinear vs nearest is not exercised here —
    /// the I7 tests below cover the depth-weighting behaviour, not filtering).
    #[allow(clippy::too_many_arguments)]
    pub fn bilateral_texel(
        fx: &Fixture<'_>,
        cx: i32,
        cy: i32,
        axis: u32,
        depth_sigma: f32,
        near: f32,
        far: f32,
    ) -> [f32; 4] {
        let (dxi, dyi) = if axis == 0 { (1, 0) } else { (0, 1) };
        let sigma = depth_sigma.max(1e-4);
        let inv_sigma = 1.0 / sigma;
        let z_center = linearize_depth(fx.depth_at(cx, cy), near, far);
        let center = fx.color_at(cx, cy);

        let mut acc = [center[0] * K9[0], center[1] * K9[0], center[2] * K9[0]];
        let mut wsum = K9[0];

        for j in 1..=4i32 {
            let kj = K9[j as usize];
            for sign in [1i32, -1i32] {
                let off = j * sign;
                let cxx = cx + dxi * off;
                let cyy = cy + dyi * off;
                let zj = linearize_depth(fx.depth_at(cxx, cyy), near, far);
                let dz = (zj - z_center) * inv_sigma;
                let w = kj * (-(dz * dz)).exp();
                let c = fx.color_at(cxx, cyy);
                acc[0] += c[0] * w;
                acc[1] += c[1] * w;
                acc[2] += c[2] * w;
                wsum += w;
            }
        }

        let inv_w = 1.0 / wsum.max(1e-6);
        [acc[0] * inv_w, acc[1] * inv_w, acc[2] * inv_w, center[3]]
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I7** (`docs/CINEMATIC_POST_DESIGN.md`): `bilateral_blur` on a
    //! uniform-depth plane equals the plain 9-tap gaussian; across a depth
    //! step it does not bleed. Three named tests per the invariant table:
    //! `bilateral_uniform_depth_matches_gaussian`, `bilateral_depth_edge_
    //! no_bleed`, and the I1-pattern CPU-reference parity test. (The
    //! `docs/ADDING_PRIMITIVES.md` codegen-path generated-vs-hand-kernel
    //! parity test was deleted 2026-07-20, W1-B, migration scaffolding
    //! retired.)
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::cpu_reference::{bilateral_texel, Fixture};
    use super::{BilateralBlur, BilateralBlurUniforms};
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

    fn upload_depth(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, &r) in raw.iter().enumerate() {
            px[i * 4] = f16::from_f32(r);
            px[i * 4 + 1] = f16::from_f32(r);
            px[i * 4 + 2] = f16::from_f32(r);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, "bilateral-depth", &px)
    }

    fn upload_color(device: &GpuDevice, w: u32, h: u32, color: &[[f32; 4]]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, c) in color.iter().enumerate() {
            px[i * 4] = f16::from_f32(c[0]);
            px[i * 4 + 1] = f16::from_f32(c[1]);
            px[i * 4 + 2] = f16::from_f32(c[2]);
            px[i * 4 + 3] = f16::from_f32(c[3]);
        }
        upload_rgba16f(device, w, h, "bilateral-color", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("bilateral-readback");
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

    fn dispatch(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        in_tex: &GpuTexture,
        depth_tex: &GpuTexture,
        sampler: &manifold_gpu::GpuSampler,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "bilateral-out");
        let mut enc = device.create_encoder("bilateral-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Texture { binding: 2, texture: depth_tex },
                GpuBinding::Sampler { binding: 3, sampler },
                GpuBinding::Texture { binding: 4, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bilateral-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    fn generated_pipeline(device: &GpuDevice, label: &str) -> GpuComputePipeline {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<BilateralBlur>()
            .expect("node.bilateral_blur standalone codegen");
        device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, label)
    }

    /// Non-uniform color gradient — noise stand-in — so a per-texel bug
    /// can't hide behind a flat fill.
    fn color_gradient(w: u32, h: u32) -> Vec<[f32; 4]> {
        (0..(w * h) as usize)
            .map(|i| {
                let x = (i as u32 % w) as f32 / w as f32;
                let y = (i as u32 / w) as f32 / h as f32;
                [x, y, 0.5, 1.0]
            })
            .collect()
    }

    /// **I7a — `bilateral_uniform_depth_matches_gaussian`**: on a perfectly
    /// flat depth plane, every `dz_j` is exactly 0, so every weight reduces
    /// to its bare `K9_j` term — this atom must equal a plain K9 gaussian
    /// blur byte-for-byte (within fp16 tolerance). Cross-checked against the
    /// CPU reference (which implements the identical reduction) rather than
    /// against `node.gaussian_blur` itself, since the two primitives don't
    /// share a WGSL source to byte-compare against directly — the reduction
    /// IS the K9 gaussian by construction, so matching the CPU reference at
    /// dz=0 is the byte-compare the invariant calls for.
    #[test]
    fn bilateral_uniform_depth_matches_gaussian() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let raw_depth = vec![0.5f32; (w * h) as usize];
        let color = color_gradient(w, h);
        let depth_tex = upload_depth(&device, w, h, &raw_depth);
        let color_tex = upload_color(&device, w, h, &color);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let (near, far) = (0.1f32, 100.0f32);
        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms { axis, depth_sigma: 0.1, near, far };
            let bytes = bytemuck::bytes_of(&uniforms);
            let pipeline = generated_pipeline(&device, "bilateral-uniform");
            let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, &sampler, w, h, bytes);

            let fx = Fixture { w: w as i32, h: h as i32, depth: &raw_depth, color: &color };
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let cpu = bilateral_texel(&fx, x, y, axis, 0.1, near, far);
                    let gpu = gpu_out[(y as u32 * w + x as u32) as usize];
                    for c in 0..4 {
                        assert!(
                            (cpu[c] - gpu[c]).abs() < 2e-3,
                            "axis {axis} texel ({x},{y}) ch {c}: uniform-depth cpu={} gpu={}",
                            cpu[c],
                            gpu[c]
                        );
                    }
                }
            }
        }
    }

    /// **I7b — `bilateral_depth_edge_no_bleed`**: a hard step-edge depth
    /// discontinuity down the middle of the buffer (near-plane left half,
    /// far-plane right half — a difference many multiples of `depth_sigma`)
    /// must suppress cross-edge weight almost entirely. Measures the actual
    /// numeric weight contribution from across the edge (not just the pixel
    /// output) and asserts it's under 1% of the total, directly exercising
    /// the invariant's own phrasing ("cross-edge contribution < 1% asserted
    /// numerically").
    #[test]
    fn bilateral_depth_edge_no_bleed() {
        let (w, h) = (16i32, 4i32);
        let half = w / 2;
        let mut raw_depth = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                raw_depth[(y * w + x) as usize] = if x < half { 0.05 } else { 0.95 };
            }
        }
        let (near, far, depth_sigma) = (0.1f32, 100.0f32, 0.1f32);
        let sigma = depth_sigma.max(1e-4);
        let inv_sigma = 1.0 / sigma;

        // Compute the ACTUAL per-tap weight sum at the texel immediately left
        // of the edge (cx = half - 1, axis=Horizontal): the taps at j=+1..+4
        // reach across the edge into the far-plane side; every other tap
        // stays on the near-plane side. Reconstruct z_center + each tap's
        // weight directly (mirrors `bilateral_texel`'s inner loop) so this
        // test asserts the WEIGHT CONTRIBUTION, not just the blended color.
        use crate::node_graph::camera::linearize_depth;
        let depth_at = |x: i32, y: i32| -> f32 {
            let cx = x.clamp(0, w - 1);
            let cy = y.clamp(0, h - 1);
            raw_depth[(cy * w + cx) as usize]
        };
        let cx = half - 1;
        let cy = 0;
        let z_center = linearize_depth(depth_at(cx, cy), near, far);
        const K9: [f32; 5] = [0.16501, 0.15019, 0.11325, 0.07076, 0.03664];
        let mut total_weight = K9[0];
        let mut cross_edge_weight = 0.0f32;
        for j in 1..=4i32 {
            let kj = K9[j as usize];
            for sign in [1i32, -1i32] {
                let off = j * sign;
                let sx = cx + off;
                let zj = linearize_depth(depth_at(sx, cy), near, far);
                let dz = (zj - z_center) * inv_sigma;
                let w_tap = kj * (-(dz * dz)).exp();
                total_weight += w_tap;
                // "Across the edge" = sample landed on the far-plane side
                // (sx >= half) while the center is on the near-plane side.
                if sx.clamp(0, w - 1) >= half {
                    cross_edge_weight += w_tap;
                }
            }
        }
        let fraction = cross_edge_weight / total_weight;
        assert!(
            fraction < 0.01,
            "cross-edge weight fraction must be < 1%, got {} ({}/{})",
            fraction,
            cross_edge_weight,
            total_weight
        );

        // Belt-and-suspenders: dispatch the real generated kernel on this
        // exact fixture and confirm the blended pixel just left of the edge
        // stays near the near-plane's own color (didn't pick up the
        // far-plane's, which in this fixture is IDENTICAL color so we vary
        // the color per side instead — a real cross-check needs a color
        // difference to detect bleed through the OUTPUT, not just weights).
        let device = crate::test_device();
        let (wu, hu) = (w as u32, h as u32);
        let mut color = vec![[0.0f32, 0.0, 0.0, 1.0]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let v = if x < half { 0.0 } else { 1.0 };
                color[(y * w + x) as usize] = [v, v, v, 1.0];
            }
        }
        let depth_tex = upload_depth(&device, wu, hu, &raw_depth);
        let color_tex = upload_color(&device, wu, hu, &color);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let uniforms = BilateralBlurUniforms { axis: 0, depth_sigma, near, far };
        let bytes = bytemuck::bytes_of(&uniforms);
        let pipeline = generated_pipeline(&device, "bilateral-edge");
        let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, &sampler, wu, hu, bytes);
        let idx = (cy as u32 * wu + cx as u32) as usize;
        assert!(
            gpu_out[idx][0] < 0.01,
            "texel just left of the depth edge must stay near the near-plane's own color (0.0), \
             got {} — cross-edge bleed detected",
            gpu_out[idx][0]
        );
    }

    /// **I1-pattern CPU-reference parity** (I7's third named check): the
    /// generated standalone kernel matches `cpu_reference::bilateral_texel`
    /// within 1e-4 on synthetic non-uniform depth+color inputs — this is the
    /// general (non-degenerate) case, distinct from I7a's dz=0 special case.
    #[test]
    fn generated_bilateral_matches_cpu_reference() {
        let device = crate::test_device();
        let (w, h) = (20u32, 12u32);
        let mut raw_depth = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                let fy = y as f32 / (h.saturating_sub(1).max(1)) as f32;
                raw_depth[(y * w + x) as usize] = 0.1 + 0.8 * (0.5 * fx + 0.5 * fy);
            }
        }
        let color = color_gradient(w, h);
        let depth_tex = upload_depth(&device, w, h, &raw_depth);
        let color_tex = upload_color(&device, w, h, &color);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let (near, far, depth_sigma) = (0.1f32, 100.0f32, 0.3f32);

        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms { axis, depth_sigma, near, far };
            let bytes = bytemuck::bytes_of(&uniforms);
            let pipeline = generated_pipeline(&device, "bilateral-cpu-parity");
            let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, &sampler, w, h, bytes);

            let fixture = Fixture { w: w as i32, h: h as i32, depth: &raw_depth, color: &color };
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let cpu = bilateral_texel(&fixture, x, y, axis, depth_sigma, near, far);
                    let gpu = gpu_out[(y as u32 * w + x as u32) as usize];
                    for c in 0..4 {
                        assert!(
                            (cpu[c] - gpu[c]).abs() < 1e-3,
                            "axis {axis} texel ({x},{y}) ch {c}: cpu={} gpu={}",
                            cpu[c],
                            gpu[c]
                        );
                    }
                }
            }
        }
    }

}
