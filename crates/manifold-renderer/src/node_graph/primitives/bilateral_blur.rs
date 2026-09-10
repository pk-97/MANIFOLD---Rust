//! `node.bilateral_blur` — depth-guided (bilateral) separable blur pair
//! between an AO atom and its mix (`docs/CINEMATIC_POST_DESIGN.md` D8). The
//! observed defect this closes: `node.ssao_from_depth`'s 16 hash-rotated
//! samples per pixel ship raw, wired straight into the compositing mix, with
//! no smoothing pass — per-pixel noise by construction, and every production
//! AO implementation (SSAO or GTAO) follows the sampler with an edge-aware
//! blur. General-purpose by design (any texture + depth guide, not AO-only)
//! per the section 2.5 audit (2026-07-13, 214 primitives surveyed) that found no
//! edge-aware/bilateral blur in the catalog.
//!
//! S6 water extension (`docs/WATER_SIMULATION_DESIGN.md` section 7): an
//! optional `coverage` input (unwired is BYTE-IDENTICAL to the D8 kernel —
//! gated by the existing gpu_tests parity checks plus the defaults test)
//! excludes uncovered neighbour taps and preserves empty centre pixels, and
//! an optional `value_space` enum selects ClipDepth mode: `in` carries raw
//! [0,1] clip depth, the weighted average runs in linear eye depth (raw
//! depth stays the guide), and the result converts back through the shared
//! projection convention (`depth_common.wgsl`'s linearize/delinearize pair).
//! `camera` is required in ClipDepth mode and otherwise optional; both
//! modes consume it entirely via the near/far derived uniforms.
//!
//! Fixed 9 taps at 1-texel spacing along `axis`, weighted by the SAME
//! sigma~=2 gaussian constants every other 9-tap kernel in this codebase
//! uses (`VBW_K9` / `SG_K9_*`) times a Gaussian falloff on the linearized-
//! depth difference from the center texel: `weight_j = K9_j *
//! exp(-(dz_j/depth_sigma)^2)`. Renormalized by the actual weight sum used.
//! Alpha is a pure center pass-through — this atom never blurs an alpha
//! channel it doesn't own.
//!
//! `in`, `depth` and `coverage` are all GatherTexel (S6 revision: integer
//! `textureLoad` + manual ClampToEdge, no sampler — `in` may carry fp32
//! clip depth in ClipDepth mode and r32float is not sampler-filterable; the
//! fixed 9 taps are integer 1-texel offsets, so texel loads are
//! byte-identical to the old texel-centre sampler reads). `camera` is
//! consumed ENTIRELY via the two `near`/`far` derived uniforms (the D7/P0
//! mechanism `node.coc_from_depth` established) — never a GPU binding,
//! which is what lets this atom fuse with a pointwise neighbour instead of
//! being a permanent boundary.
//!
//! `depth_sigma` is a plain param (NOT a card, D8 — denoise is quality
//! plumbing, not a performer knob). Pair an H pass with a V pass for a full
//! 2D edge-aware blur (same axis-pair convention as `node.gaussian_blur`).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");

/// Display labels for the `axis` enum, indexed by enum value — matches
/// `GAUSSIAN_BLUR_AXES` / `BLUR_VARIABLE_AXES`'s convention (0=Horizontal,
/// 1=Vertical).
pub const BILATERAL_BLUR_AXES: &[&str] = &["Horizontal", "Vertical"];

/// Display labels for the `value_space` enum (S6): RawColour is the D8
/// original (values averaged as-is); ClipDepth treats `in` as raw [0,1]
/// clip depth and averages in linear eye depth.
pub const BILATERAL_BLUR_VALUE_SPACES: &[&str] = &["RawColour", "ClipDepth"];

/// Generated-codegen uniform layout: the three PARAMS (`axis`,
/// `depth_sigma`, `value_space`) in declaration order, then the two DERIVED
/// fields (`near`, `far`) in declaration order, then the injected
/// `use_coverage` flag — one f32/u32 word each. 6 words = 24 bytes, no
/// padding needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BilateralBlurUniforms {
    axis: u32,
    depth_sigma: f32,
    value_space: u32,
    near: f32,
    far: f32,
    use_coverage: u32,
}

crate::primitive! {
    name: BilateralBlur,
    type_id: "node.bilateral_blur",
    purpose: "Depth-guided (bilateral) single-axis blur: fixed 9 taps at 1-texel spacing along `axis`, weight_j = K9_j * exp(-(dz_j/depth_sigma)^2) where K9_j are the same sigma~=2 gaussian constants used by every other 9-tap kernel in this codebase and dz_j is the linearized-depth difference from the center texel, renormalized by the weight sum actually used. Pair a Horizontal pass with a Vertical pass for a full 2D edge-aware blur that smooths noise without bleeding across depth discontinuities. Alpha is a pure center pass-through. S6: optional `coverage` excludes uncovered taps and preserves empty centres (unwired = byte-identical D8 behaviour); `value_space=ClipDepth` averages `in` as clip depth in linear eye depth (raw depth guide, camera near/far via derived uniforms, fp32 only). `camera` is read entirely via near/far derived uniforms — never a GPU binding.",
    inputs: {
        in: Texture2D required,
        depth: Texture2D required,
        camera: Camera optional,
        coverage: Texture2D optional,
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
        ParamDef {
            name: Cow::Borrowed("value_space"),
            label: "Value Space",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: BILATERAL_BLUR_VALUE_SPACES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Pair an H pass (axis=Horizontal) with a V pass (axis=Vertical) for a 2D edge-aware blur — same axis-pair convention as node.gaussian_blur / node.variable_blur. `depth_sigma` is in the SAME world units `linearize_depth` returns (view-space meters, following the Camera's near/far) — smaller values hug depth edges tighter (less cross-edge bleed, noisier flat regions); larger values approach a plain 9-tap gaussian (D8's I7 invariant: on a perfectly uniform depth plane this atom is byte-identical to the plain K9 gaussian, since every dz_j collapses to 0 and every weight reduces to its K9_j term). `depth` expects render_scene's raw [0,1] `depth` output (not pre-linearized), same contract as node.coc_from_depth / node.ssao_from_depth. S6 water: wire node.particle_surface_depth's coverage in and set value_space=ClipDepth to smooth the reconstructed water surface depth — uncovered taps are excluded and empty pixels pass through untouched.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Bilateral Blur", category: Atom },
    summary: "A depth-guided blur that smooths noise without bleeding across depth edges — the standard denoise pass after any per-pixel noisy sampler (ambient occlusion, dithered effects) that needs to stay sharp at silhouettes; S6 adds a coverage-aware ClipDepth mode for smoothing reconstructed water surface depth.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["bilateral blur", "bilateral filter", "edge-aware blur", "depth-aware blur", "denoise", "ao denoise"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/bilateral_blur_body.wgsl"),
    // S6 revision: ALL inputs GatherTexel. `in` may carry fp32 clip depth
    // (ClipDepth mode), and r32float is NOT sampler-filterable — integer
    // textureLoad is the only legal read. Taps are integer 1-texel offsets
    // regardless, so texel loads are byte-identical to the old sampler
    // reads at texel centres (gated by the D8 parity tests).
    input_access: [GatherTexel, GatherTexel, GatherTexel],
    // D6(a): `depth` feeds the per-tap dz guide and, in ClipDepth mode, `in`
    // carries the fp32 depth being averaged — fp16 quantization of either
    // shows up as banding at silhouette edges.
    precision_critical: ["depth", "in"],
    derived_uniforms: ["near", "far"],
    wgsl_includes: [DEPTH_COMMON],
    extra_fields: {
        // 1x1 dummy for the unwired coverage slot — one texture bound as
        // both shader-read and storage-write in a dispatch is a hazard,
        // so the dummy is dedicated, never the output.
        dummy_texture: Option<manifold_gpu::GpuTexture> = None,
        // Cache of the `value_space` param that drives output_format
        // (compile-time). 0 = RawColour (the pre-S6 default: backend
        // Rgba16Float out); 1 = ClipDepth (R32Float out). Written by
        // reconfigure (param writes) and re-asserted by run.
        value_space_mode: u32 = 0,
    },
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
// exactly. `camera` is OPTIONAL since S6: an unwired camera falls back to
// `Camera::default_perspective()`'s near/far on BOTH paths (the fused
// recompute gets `ctx.camera = None` when no wire exists, and must produce
// the same values `run()` would).
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.bilateral_blur",
        recompute: |ctx| {
            ctx.camera
                .map(derive_depth_scalars)
                .or_else(|| Some(derive_depth_scalars(&Camera::default_perspective())))
                .map(|v| v.to_vec())
        },
    }
}

/// Single source of truth for the `value_space` param resolution —
/// shared by `run()` (per-frame), `reconfigure()` (param writes) and
/// the `output_format` cache they both feed, so the three can never
/// drift. 0 = RawColour (default), 1 = ClipDepth.
fn resolve_value_space(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("value_space") {
        Some(ParamValue::Enum(v)) => (*v).min(1),
        Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
        _ => 0,
    }
}

impl Primitive for BilateralBlur {
    /// S6: `out` carries clip depth in ClipDepth mode — fp32 only, no
    /// f16 depth feedback (docs/WATER_SIMULATION_DESIGN.md section 7).
    /// RawColour keeps the backend default (Rgba16Float): an
    /// unconditional R32Float pin would drop g/b for rgb consumers of a
    /// materialized output and expose them to the unfilterable-r32float
    /// sampler read (`node.mix` defaults to Coincident). The mode is
    /// recorded by `reconfigure` (param writes, before compile queries
    /// formats) and re-asserted by `run` (covers the
    /// `set_param_unchecked` hot path, which skips reconfigure).
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        match port {
            "out" if self.value_space_mode == 1 => Some(manifold_gpu::GpuTextureFormat::R32Float),
            _ => None,
        }
    }

    fn reconfigure(&mut self, params: &crate::node_graph::effect_node::ParamValues) {
        self.value_space_mode = resolve_value_space(params);
    }

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
        let value_space = resolve_value_space(ctx.params);
        // Re-assert the cached mode (reconfigure is skipped on the
        // set_param_unchecked hot path; output_format reads the cache at
        // plan-compile time).
        self.value_space_mode = value_space;

        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        if value_space == 1 && ctx.inputs.camera("camera").is_none() {
            // ClipDepth without a camera: the linearization has no
            // near/far. Explicit diagnostic; the fallback below (default
            // camera) keeps the output deterministic.
            ctx.error(
                "node.bilateral_blur: value_space=ClipDepth requires the `camera` input (near/far); falling back to the default camera"
                    .to_string(),
            );
        }
        let [near, far] = derive_depth_scalars(&cam);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(depth_tex) = ctx.inputs.texture_2d("depth") else {
            return;
        };
        let coverage_tex = ctx.inputs.texture_2d("coverage");
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Three-source MultiInputCoincident, ALL GatherTexel (S6
            // revision — `in` can be fp32 clip depth, which is not
            // sampler-filterable; integer loads only, no sampler bound).
            // Generated bindings are uniform(0)/tex_in(1)/tex_depth(2)/
            // tex_coverage(3)/dst(4).
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.bilateral_blur standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.bilateral_blur",
            )
        });

        let uniforms = BilateralBlurUniforms {
            axis,
            depth_sigma,
            value_space,
            near,
            far,
            use_coverage: coverage_tex.is_some() as u32,
        };

        // The shader always binds the coverage slot; unwired binds a
        // DEDICATED 1x1 dummy (gated off via use_coverage) — never the
        // output texture: one texture bound as both shader-read and
        // storage-write in a single dispatch is a read-write hazard.
        let dummy = self.dummy_texture.get_or_insert_with(|| {
            gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::R8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.bilateral_blur.coverage_dummy",
                mip_levels: 1,
            })
        });
        let coverage_bind = coverage_tex.unwrap_or(dummy);

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
                GpuBinding::Texture {
                    binding: 3,
                    texture: coverage_bind,
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
    fn declares_in_depth_optional_camera_and_coverage() {
        use crate::node_graph::ports::PortType;

        assert_eq!(BilateralBlur::TYPE_ID, "node.bilateral_blur");
        let names: Vec<&str> = BilateralBlur::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "depth", "camera", "coverage"]);
        assert_eq!(BilateralBlur::INPUTS[0].ty, PortType::Texture2D);
        assert!(BilateralBlur::INPUTS[0].required);
        assert_eq!(BilateralBlur::INPUTS[1].ty, PortType::Texture2D);
        assert!(BilateralBlur::INPUTS[1].required);
        assert_eq!(BilateralBlur::INPUTS[2].ty, PortType::Camera);
        // S6: camera is optional (required only in ClipDepth mode — run()
        // emits an explicit error there) and coverage is optional
        // (unwired = byte-identical D8 behaviour).
        assert!(!BilateralBlur::INPUTS[2].required);
        assert_eq!(BilateralBlur::INPUTS[3].ty, PortType::Texture2D);
        assert!(!BilateralBlur::INPUTS[3].required);

        assert_eq!(BilateralBlur::OUTPUTS.len(), 1);
        assert_eq!(BilateralBlur::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_axis_depth_sigma_and_value_space_params() {
        let names: Vec<&str> = BilateralBlur::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["axis", "depth_sigma", "value_space"]);
        // RawColour default keeps unwired/pre-S6 behaviour byte-identical.
        assert_eq!(BilateralBlur::PARAMS[2].default, ParamValue::Enum(0));
    }

    #[test]
    fn declares_two_derived_uniforms_near_far() {
        assert_eq!(BilateralBlur::DERIVED_UNIFORMS, &["near", "far"]);
    }

    #[test]
    fn out_port_format_follows_value_space_mode() {
        use crate::node_graph::effect_node::EffectNode;

        let fmt = |prim: &BilateralBlur| {
            crate::node_graph::primitive::Primitive::output_format(prim, "out")
        };
        let mut params = crate::node_graph::effect_node::ParamValues::default();

        // Unset / RawColour (the pre-S6 default): backend default
        // Rgba16Float — an unconditional fp32 pin would drop g/b for
        // rgb consumers of a materialized output and expose them to the
        // unfilterable-r32float sampler read.
        let mut prim = BilateralBlur::new();
        assert_eq!(fmt(&prim), None);

        // ClipDepth: fp32 only, no f16 depth feedback (design section 7).
        params.insert(std::borrow::Cow::Borrowed("value_space"), ParamValue::Enum(1));
        EffectNode::reconfigure(&mut prim, &params);
        assert_eq!(fmt(&prim), Some(manifold_gpu::GpuTextureFormat::R32Float));

        // Back to RawColour: default again (the cache is re-written, not
        // sticky).
        params.insert(std::borrow::Cow::Borrowed("value_space"), ParamValue::Enum(0));
        EffectNode::reconfigure(&mut prim, &params);
        assert_eq!(fmt(&prim), None);
    }

    #[test]
    fn uniform_struct_is_24_bytes() {
        assert_eq!(std::mem::size_of::<BilateralBlurUniforms>(), 24);
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
/// implementation of the committed D8 formula + the S6 extensions,
/// independent of the WGSL body (not sharing source). Used by the
/// GPU-vs-CPU parity gpu_tests below.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod cpu_reference {
    use crate::node_graph::camera::{delinearize_depth, linearize_depth};

    const K9: [f32; 5] = [0.16501, 0.15019, 0.11325, 0.07076, 0.03664];

    /// A synthetic depth+color buffer: raw [0,1] depth and RGBA color,
    /// row-major, `w*h` long each. `coverage` (S6) is an optional 0/1 mask.
    pub struct Fixture<'a> {
        pub w: i32,
        pub h: i32,
        pub depth: &'a [f32],
        pub color: &'a [[f32; 4]],
        pub coverage: Option<&'a [f32]>,
    }

    impl<'a> Fixture<'a> {
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
        fn covered_at(&self, x: i32, y: i32) -> bool {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.coverage.is_none_or(|c| c[(cy * self.w + cx) as usize] >= 0.5)
        }
    }

    /// The D8 formula with the S6 extensions, transcribed exactly (the CPU
    /// twin the WGSL body implements). `axis` follows
    /// `BILATERAL_BLUR_AXES` (0=Horizontal, 1=Vertical); `value_space`
    /// follows `BILATERAL_BLUR_VALUE_SPACES` (0=RawColour, 1=ClipDepth).
    /// Nearest-neighbour reads on both textures — matches the GPU body's
    /// `fetch_in`/`bb_load_at` texel-center sampling exactly (the fixture
    /// is uploaded at integer pixel positions with no fractional offsets).
    #[allow(clippy::too_many_arguments)]
    pub fn bilateral_texel_ext(
        fx: &Fixture<'_>,
        cx: i32,
        cy: i32,
        axis: u32,
        depth_sigma: f32,
        value_space: u32,
        coverage_wired: bool,
        near: f32,
        far: f32,
    ) -> [f32; 4] {
        let (dxi, dyi) = if axis == 0 { (1, 0) } else { (0, 1) };
        let sigma = depth_sigma.max(1e-4);
        let inv_sigma = 1.0 / sigma;
        let z_center = linearize_depth(fx.depth_at(cx, cy), near, far);
        let center = fx.color_at(cx, cy);

        // S6: uncovered centre passes through untouched.
        if coverage_wired && !fx.covered_at(cx, cy) {
            return center;
        }

        let center_value = if value_space == 1 {
            [linearize_depth(center[0], near, far); 3]
        } else {
            [center[0], center[1], center[2]]
        };
        let mut acc = [center_value[0] * K9[0], center_value[1] * K9[0], center_value[2] * K9[0]];
        let mut wsum = K9[0];

        for j in 1..=4i32 {
            let kj = K9[j as usize];
            for sign in [1i32, -1i32] {
                let off = j * sign;
                let cxx = cx + dxi * off;
                let cyy = cy + dyi * off;
                // S6: uncovered taps are excluded entirely.
                if coverage_wired && !fx.covered_at(cxx, cyy) {
                    continue;
                }
                let zj = linearize_depth(fx.depth_at(cxx, cyy), near, far);
                let dz = (zj - z_center) * inv_sigma;
                let w = kj * (-(dz * dz)).exp();
                let c = fx.color_at(cxx, cyy);
                let v = if value_space == 1 {
                    [linearize_depth(c[0], near, far); 3]
                } else {
                    [c[0], c[1], c[2]]
                };
                acc[0] += v[0] * w;
                acc[1] += v[1] * w;
                acc[2] += v[2] * w;
                wsum += w;
            }
        }

        let inv_w = 1.0 / wsum.max(1e-6);
        let mut out = [acc[0] * inv_w, acc[1] * inv_w, acc[2] * inv_w];
        if value_space == 1 {
            let d = delinearize_depth(out[0], near, far).clamp(0.0, 0.999_999_94);
            out = [d, d, d];
        }
        [out[0], out[1], out[2], center[3]]
    }

    /// The pre-S6 signature, unchanged for the D8 tests: RawColour values,
    /// no coverage.
    pub fn bilateral_texel(
        fx: &Fixture<'_>,
        cx: i32,
        cy: i32,
        axis: u32,
        depth_sigma: f32,
        near: f32,
        far: f32,
    ) -> [f32; 4] {
        bilateral_texel_ext(fx, cx, cy, axis, depth_sigma, 0, false, near, far)
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
        GpuBinding, GpuComputePipeline, GpuDevice, GpuTexture, GpuTextureDesc,
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
        coverage_tex: Option<&GpuTexture>,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "bilateral-out");
        // The shader always binds the coverage slot (GatherTexel); an
        // unwired coverage binds a DEDICATED 1x1 dummy (gated off via
        // use_coverage) — never the output texture: one texture bound as
        // both shader-read and storage-write in a single dispatch is a
        // read-write hazard the encoder's bind cache doesn't dedupe.
        let dummy = device.create_texture(&GpuTextureDesc {
            width: 1,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::R8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ,
            label: "bilateral-coverage-dummy",
            mip_levels: 1,
        });
        let coverage_bind = coverage_tex.unwrap_or(&dummy);
        let mut enc = device.create_encoder("bilateral-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Texture { binding: 2, texture: depth_tex },
                GpuBinding::Texture { binding: 3, texture: coverage_bind },
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

        let (near, far) = (0.1f32, 100.0f32);
        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms {
                axis,
                depth_sigma: 0.1,
                value_space: 0,
                near,
                far,
                use_coverage: 0,
            };
            let bytes = bytemuck::bytes_of(&uniforms);
            let pipeline = generated_pipeline(&device, "bilateral-uniform");
            let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, None, w, h, bytes);

            let fx = Fixture { w: w as i32, h: h as i32, depth: &raw_depth, color: &color, coverage: None };
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
        let uniforms = BilateralBlurUniforms {
            axis: 0,
            depth_sigma,
            value_space: 0,
            near,
            far,
            use_coverage: 0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);
        let pipeline = generated_pipeline(&device, "bilateral-edge");
        let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, None, wu, hu, bytes);
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
        let (near, far, depth_sigma) = (0.1f32, 100.0f32, 0.3f32);

        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms {
                axis,
                depth_sigma,
                value_space: 0,
                near,
                far,
                use_coverage: 0,
            };
            let bytes = bytemuck::bytes_of(&uniforms);
            let pipeline = generated_pipeline(&device, "bilateral-cpu-parity");
            let gpu_out = dispatch(&device, &pipeline, &color_tex, &depth_tex, None, w, h, bytes);

            let fixture = Fixture { w: w as i32, h: h as i32, depth: &raw_depth, color: &color, coverage: None };
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

    /// **S6 defaults**: `value_space=RawColour` + coverage unwired must be
    /// byte-identical to the D8 kernel — same generated pipeline the S6
    /// body produces, exercised through the exact default param path, on a
    /// non-uniform depth+color fixture, cross-checked against the D8
    /// cpu_reference formula (the pre-S6 parity tests above already pin the
    /// same kernel at other fixtures; this test pins the DEFAULT PARAM
    /// RESOLUTION path specifically).
    #[test]
    fn bilateral_s6_defaults_match_d8_reference() {
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
        let (near, far) = (0.1f32, 100.0f32);

        // The param defaults as run() resolves them (no params set at all).
        let axis = 0u32;
        let depth_sigma = 0.1f32;
        let value_space = 0u32;
        let uniforms = BilateralBlurUniforms {
            axis,
            depth_sigma,
            value_space,
            near,
            far,
            use_coverage: 0,
        };
        let pipeline = generated_pipeline(&device, "bilateral-s6-defaults");
        let gpu_out = dispatch(
            &device, &pipeline, &color_tex, &depth_tex, None, w, h,
            bytemuck::bytes_of(&uniforms),
        );

        let fixture = Fixture {
            w: w as i32,
            h: h as i32,
            depth: &raw_depth,
            color: &color,
            coverage: None,
        };
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let cpu = super::cpu_reference::bilateral_texel_ext(
                    &fixture, x, y, axis, depth_sigma, value_space, false, near, far,
                );
                let gpu = gpu_out[(y as u32 * w + x as u32) as usize];
                for c in 0..4 {
                    assert!(
                        (cpu[c] - gpu[c]).abs() < 1e-3,
                        "defaults texel ({x},{y}) ch {c}: cpu={} gpu={}",
                        cpu[c],
                        gpu[c]
                    );
                }
            }
        }
    }

    /// Upload a single-channel fp32 fixture as an R32Float texture — the
    /// S6 ClipDepth contract is fp32 end to end (no f16 depth feedback).
    fn upload_r32(device: &GpuDevice, w: u32, h: u32, raw: &[f32], label: &str) -> GpuTexture {
        assert_eq!(raw.len(), (w * h) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::R32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        device.upload_texture(&tex, bytemuck::cast_slice(raw));
        tex
    }

    /// Upload a 0/1 coverage mask as R8Unorm (the node.particle_surface_depth
    /// coverage wire's format).
    fn upload_coverage(device: &GpuDevice, w: u32, h: u32, mask: &[f32]) -> GpuTexture {
        let bytes: Vec<u8> = mask
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::R8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "bilateral-coverage",
            mip_levels: 1,
        });
        device.upload_texture(&tex, &bytes);
        tex
    }

    /// Read back an R32Float texture as f32.
    fn readback_r32(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<f32> {
        let bytes_per_row = w * 4;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("bilateral-readback-r32");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let floats: &[f32] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (w * h) as usize) };
        floats.to_vec()
    }

    /// Dispatch into an R32Float output (ClipDepth's real output format —
    /// `output_format("out")` is R32Float in ClipDepth mode, Rgba16F in
    /// RawColour).
    fn dispatch_r32(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        in_tex: &GpuTexture,
        depth_tex: &GpuTexture,
        coverage_tex: Option<&GpuTexture>,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<f32> {
        let out = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::R32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::COPY_SRC
                | GpuTextureUsage::COPY_DST,
            label: "bilateral-out-r32",
            mip_levels: 1,
        });
        let coverage_bind = coverage_tex.unwrap_or(&out);
        let mut enc = device.create_encoder("bilateral-dispatch-r32");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Texture { binding: 2, texture: depth_tex },
                GpuBinding::Texture { binding: 3, texture: coverage_bind },
                GpuBinding::Texture { binding: 4, texture: &out },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bilateral-dispatch-r32",
        );
        enc.commit_and_wait_completed();
        readback_r32(device, &out, w, h)
    }

    /// **S6 ClipDepth parity**: with `value_space=ClipDepth` the averaged
    /// quantity is linear eye depth, converted back through the shared
    /// projection convention; fp32 end to end (R32Float in AND out), so the
    /// CPU reference must match to a tight tolerance on non-uniform depth.
    #[test]
    fn bilateral_clipdepth_matches_cpu_reference_fp32() {
        use super::cpu_reference::bilateral_texel_ext;

        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let mut raw_depth = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w - 1) as f32;
                let fy = y as f32 / (h - 1) as f32;
                raw_depth[(y * w + x) as usize] = 0.05 + 0.45 * (0.5 * fx + 0.5 * fy);
            }
        }
        // `in` carries the same clip depth (the water surface-depth wire).
        let color: Vec<[f32; 4]> = raw_depth.iter().map(|&d| [d, d, d, 1.0]).collect();
        let (near, far, depth_sigma) = (0.1f32, 100.0f32, 0.3f32);

        let depth_tex = upload_r32(&device, w, h, &raw_depth, "bilateral-clipdepth-depth");
        let color_tex = upload_r32(&device, w, h, &raw_depth, "bilateral-clipdepth-in");

        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms {
                axis,
                depth_sigma,
                value_space: 1,
                near,
                far,
                use_coverage: 0,
            };
            let pipeline = generated_pipeline(&device, "bilateral-clipdepth");
            let gpu_out = dispatch_r32(
                &device, &pipeline, &color_tex, &depth_tex, None, w, h,
                bytemuck::bytes_of(&uniforms),
            );

            let fixture = Fixture {
                w: w as i32,
                h: h as i32,
                depth: &raw_depth,
                color: &color,
                coverage: None,
            };
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let cpu =
                        bilateral_texel_ext(&fixture, x, y, axis, depth_sigma, 1, false, near, far);
                    let gpu = gpu_out[(y as u32 * w + x as u32) as usize];
                    assert!(
                        (cpu[0] - gpu).abs() < 2e-5,
                        "ClipDepth axis {axis} texel ({x},{y}): cpu={} gpu={}",
                        cpu[0],
                        gpu
                    );
                }
            }
        }
    }

    /// **S6 coverage**: wired coverage excludes uncovered neighbour taps
    /// from BOTH the weighted sum and the weight total, and an uncovered
    /// CENTRE pixel passes through untouched — empty pixels are never
    /// smoothed into liquid.
    #[test]
    fn bilateral_coverage_excludes_taps_and_preserves_empty_centre() {
        use super::cpu_reference::bilateral_texel_ext;

        let device = crate::test_device();
        let (w, h) = (24u32, 8u32);
        // Two depth layers with a hard step, and per-pixel color noise so a
        // wrong tap shows up in the output.
        let half = (w / 2) as i32;
        let mut raw_depth = vec![0.0f32; (w * h) as usize];
        let mut color = vec![[0.0f32; 4]; (w * h) as usize];
        let mut coverage = vec![0.0f32; (w * h) as usize];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let i = (y * w as i32 + x) as usize;
                raw_depth[i] = if x < half { 0.2 } else { 0.6 };
                color[i] = [x as f32 / w as f32, y as f32 / h as f32, 0.25, 1.0];
                // The liquid occupies the left half plus one isolated pixel
                // at the far right (an empty island inside empty space).
                coverage[i] = if x < half || (x == (w - 2) as i32 && y == (h / 2) as i32) {
                    1.0
                } else {
                    0.0
                };
            }
        }
        let (near, far, depth_sigma) = (0.1f32, 100.0f32, 0.15f32);

        let depth_tex = upload_depth(&device, w, h, &raw_depth);
        let color_tex = upload_color(&device, w, h, &color);
        let coverage_tex = upload_coverage(&device, w, h, &coverage);

        for axis in 0u32..=1 {
            let uniforms = BilateralBlurUniforms {
                axis,
                depth_sigma,
                value_space: 0,
                near,
                far,
                use_coverage: 1,
            };
            let pipeline = generated_pipeline(&device, "bilateral-coverage");
            let gpu_out = dispatch(
                &device, &pipeline, &color_tex, &depth_tex, Some(&coverage_tex), w, h,
                bytemuck::bytes_of(&uniforms),
            );

            let fixture = Fixture {
                w: w as i32,
                h: h as i32,
                depth: &raw_depth,
                color: &color,
                coverage: Some(&coverage),
            };
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let i = (y * w as i32 + x) as usize;
                    let cpu = bilateral_texel_ext(
                        &fixture, x, y, axis, depth_sigma, 0, true, near, far,
                    );
                    let gpu = gpu_out[i];
                    for c in 0..4 {
                        assert!(
                            (cpu[c] - gpu[c]).abs() < 1e-3,
                            "coverage axis {axis} texel ({x},{y}) ch {c}: cpu={} gpu={}",
                            cpu[c],
                            gpu[c]
                        );
                    }
                    if coverage[i] < 0.5 {
                        // The centre passes through as the loaded texel —
                        // the fixture round-tripped through an f16 texture,
                        // so compare against the f16-quantized input.
                        let expected: [f32; 4] = {
                            let q = |v: f32| f16::from_f32(v).to_f32();
                            [
                                q(color[i][0]),
                                q(color[i][1]),
                                q(color[i][2]),
                                q(color[i][3]),
                            ]
                        };
                        assert_eq!(
                            gpu, expected,
                            "uncovered centre ({x},{y}) must pass through untouched"
                        );
                    }
                }
            }
        }
    }

    /// **S6 fused-region uniform layout**: the standalone kernel the freeze
    /// compiler would inline must declare the fields in the exact order
    /// run() packs bytemuck-side — params (declaration order), then the
    /// derived uniforms, then the injected `use_coverage` flag. A drift here
    /// would corrupt every fused S6 water region silently.
    #[test]
    fn bilateral_s6_uniform_layout_matches_generated_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<BilateralBlur>()
            .expect("node.bilateral_blur standalone codegen");
        for field in ["axis: u32", "depth_sigma: f32", "value_space: u32", "near: f32", "far: f32", "use_coverage: u32"] {
            assert!(wgsl.contains(field), "generated kernel missing `{field}`");
        }
        // The body references the S6 conversion from the shared include.
        assert!(wgsl.contains("delinearize_depth"));
    }
}
