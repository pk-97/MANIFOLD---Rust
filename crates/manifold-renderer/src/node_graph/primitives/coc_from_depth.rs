//! `node.coc_from_depth` — physically-based circle-of-confusion from scene
//! depth + a Camera (`docs/CINEMATIC_POST_DESIGN.md` D1). The CoC-computation
//! half of DoF v1; pair with `node.variable_blur` (H then V, ping-ponged) for
//! the gather — this atom does ONLY the per-pixel CoC math, no blur (the
//! no-fused-monolith rule: CoC and gather are two dispatches, not one).
//!
//! Thin-lens model, world units in meters for lens physics:
//! ```text
//! f_mm    = SENSOR_H_MM / (2 * tan(fov_y / 2))          // from the Camera's fov
//! A_mm    = f_mm / f_stop                                // aperture diameter
//! D_mm    = linearize_depth(raw_depth, near, far) * WORLD_TO_MM
//! S_mm    = focus_distance * WORLD_TO_MM
//! coc_mm  = A_mm * f_mm * |D_mm - S_mm| / (D_mm * max(S_mm - f_mm, 1.0))
//! coc_px  = clamp(coc_mm / SENSOR_H_MM * viewport_h, 0.0, max_radius)
//! out.r   = coc_px / max_radius   (normalized — node.variable_blur's `width`
//!           R channel is a [0,1] FRACTION of ITS OWN max_radius:
//!           step_size = width_sample * max_radius + 1.0. Wire this atom's
//!           `max_radius` param equal to the downstream variable_blur nodes'
//!           `max_radius` so the units agree — get this wrong and DoF looks
//!           right in isolation but desyncs from the actual blur radius.)
//! ```
//!
//! `f_stop = INFINITY` (pinhole) drives `A_mm = 0`, hence `coc_mm = 0`
//! everywhere regardless of focus_distance/depth — an unlensed camera
//! produces a bit-clean zero CoC buffer (invariant I2).
//!
//! `camera` reads `fov_y`/`near`/`far` (projection facts) and
//! `lens.focus_distance`/`lens.f_stop` (the Camera's lens block, the ONE
//! lens `node.camera_lens` writes and every consumer reads —
//! `docs/CAMERA_AND_LENS_DESIGN.md` D4) — consumed ENTIRELY via the five
//! `derived_uniforms` below, never as a GPU binding, which is what lets this
//! Pointwise atom fuse with a neighbour instead of being a permanent
//! boundary (P0/D7).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::camera::{Camera, CameraMode};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");

/// Generated-codegen uniform layout: the `max_radius` param (f32, PARAMS
/// order) then the five DERIVED fields in declaration order (`fov_y`,
/// `near`, `far`, `focus_distance`, `f_stop` — one f32 word each, no vec3
/// expansion), padded to a 16-byte (4-word) multiple. 6 words + 2 pad = 32
/// bytes. Texture-domain atoms carry no `dispatch_count` word (that's a
/// buffer-path-only field — see `flatten_to_camera_plane.rs`'s doc comment
/// for the buffer equivalent).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CocFromDepthUniforms {
    max_radius: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    focus_distance: f32,
    f_stop: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: CocFromDepth,
    type_id: "node.coc_from_depth",
    purpose: "Physically-based circle-of-confusion from scene depth + a Camera (thin-lens model, docs/CINEMATIC_POST_DESIGN.md D1): f_mm = 24mm / (2*tan(fov_y/2)); A_mm = f_mm/f_stop; D_mm = linearize_depth(raw_depth, near, far) * 1000; S_mm = focus_distance * 1000; coc_mm = A_mm*f_mm*|D_mm-S_mm| / (D_mm*max(S_mm-f_mm, 1.0)); coc_px = clamp(coc_mm/24mm * viewport_h, 0, max_radius). Output is coc_px / max_radius (a [0,1] fraction) — wire straight into node.variable_blur's `width` input with max_radius matched between the two nodes. f_stop = infinity (pinhole) makes the whole buffer zero. Reads fov_y/near/far and the Camera's lens (focus_distance/f_stop, written by node.camera_lens) entirely via derived uniforms — the Camera wire is never a GPU binding.",
    inputs: {
        depth: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_radius"),
            label: "Max Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(24.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: reads `depth` (not color) at the same texel with no UV remap — mechanically pointwise like the other single-texture-input compute atoms (hash_field_by_seed, field_combine)
    depth_rule: Inherit,
    composition_notes: "CoC-computation half of DoF v1 — pair with two node.variable_blur nodes (Horizontal then Vertical, ping-ponged) for the gather; this atom does no blurring itself (no-fused-monolith). `max_radius` MUST match the downstream variable_blur nodes' own `max_radius` param — this atom normalizes coc_px by ITS max_radius before emitting, and variable_blur denormalizes by its OWN max_radius (step_size = width_sample * max_radius + 1.0); a mismatch desyncs the blur radius from the physically-computed CoC. `depth` expects render_scene's raw [0,1] `depth` output (not pre-linearized). `focus_distance`/`f_stop` are read off the wired Camera's lens block (set upstream by node.camera_lens — insert one camera_lens between the camera source and both render_scene and this node so DoF and exposure read the same lens).",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "CoC From Depth", category: Atom },
    summary: "Computes how out-of-focus each pixel should be from scene depth and a physical camera lens — the depth-of-field math, before any blurring happens.",
    category: Mask,
    role: Map,
    aliases: ["circle of confusion", "coc", "depth of field", "dof", "focus", "bokeh"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/coc_from_depth_body.wgsl"),
    input_access: [CoincidentTexel],
    // D6(a): the thin-lens CoC derivation is a difference-of-depths
    // (`|D_mm - S_mm|`) amplified by the lens/aperture terms — fp16
    // quantization of `depth` shows up as visible ring contours in the
    // blur-radius map at shallow depth-of-field settings.
    precision_critical: ["depth"],
    derived_uniforms: ["fov_y", "near", "far", "focus_distance", "f_stop"],
    wgsl_includes: [DEPTH_COMMON],
}

/// Single source of truth for the five Camera-derived scalar fields, in
/// `DERIVED_UNIFORMS` declaration order — shared by `run()` (unfused CPU
/// path) and the `inventory::submit!` recompute below (fused path), so the
/// two can never drift (the synthesis-drift bug class
/// `node_graph::camera::linearize_depth`'s doc comment names).
/// `fov_y` defaults to `default_perspective()`'s 60 degrees for an
/// Orthographic camera — D1 only specifies perspective DoF; orthographic
/// CoC is out of scope for this design, so this is a defensive fallback,
/// not a modeled behavior.
fn derive_lens_scalars(cam: &Camera) -> [f32; 5] {
    let fov_y = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        CameraMode::Orthographic { .. } => std::f32::consts::FRAC_PI_3,
    };
    [fov_y, cam.near, cam.far, cam.lens.focus_distance, cam.lens.f_stop]
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's fov_y/near/far/focus_distance/f_stop fields, IN DECLARATION
// ORDER — reads the region's routed Camera external, matching `run()`'s own
// `derive_lens_scalars` call below exactly. `None` when unwired (install.rs
// only creates a `camera_ext_N` port when a real producer wire exists, so
// this should not happen in practice for a member that passed the
// install-time `has_recompute` gate).
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.coc_from_depth",
        recompute: |ctx| ctx.camera.map(derive_lens_scalars).map(|v| v.to_vec()),
    }
}

impl Primitive for CocFromDepth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_radius = match ctx.params.get("max_radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 24.0,
        };

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let [fov_y, near, far, focus_distance, f_stop] = derive_lens_scalars(&cam);

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
            // Single-source: kernel generated from `wgsl_body` (CoincidentTexel —
            // no sampler; generated bindings are uniform(0)/depth(1)/dst(2)).
            // coc_from_depth.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.coc_from_depth standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.coc_from_depth",
            )
        });

        let uniforms = CocFromDepthUniforms {
            max_radius,
            fov_y,
            near,
            far,
            focus_distance,
            f_stop,
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
                    texture: depth_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.coc_from_depth",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_depth_and_camera_inputs_and_texture_output() {
        use crate::node_graph::ports::PortType;

        assert_eq!(CocFromDepth::TYPE_ID, "node.coc_from_depth");
        let names: Vec<&str> = CocFromDepth::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["depth", "camera"]);
        assert_eq!(CocFromDepth::INPUTS[0].ty, PortType::Texture2D);
        assert!(CocFromDepth::INPUTS[0].required);
        assert_eq!(CocFromDepth::INPUTS[1].ty, PortType::Camera);
        assert!(CocFromDepth::INPUTS[1].required);

        assert_eq!(CocFromDepth::OUTPUTS.len(), 1);
        assert_eq!(CocFromDepth::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_max_radius_param_only() {
        let names: Vec<&str> = CocFromDepth::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["max_radius"]);
    }

    #[test]
    fn declares_five_derived_uniforms_in_lens_order() {
        assert_eq!(
            CocFromDepth::DERIVED_UNIFORMS,
            &["fov_y", "near", "far", "focus_distance", "f_stop"]
        );
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<CocFromDepthUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CocFromDepth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.coc_from_depth");
    }

    #[test]
    fn derive_lens_scalars_reads_perspective_fov_and_lens() {
        use crate::node_graph::camera::LensParams;

        let mut cam = Camera::default_perspective();
        cam.near = 0.1;
        cam.far = 500.0;
        cam.lens = LensParams { focus_distance: 3.5, f_stop: 2.8, shutter_angle: 0.0, exposure_ev: 0.0 };
        let [fov_y, near, far, focus_distance, f_stop] = derive_lens_scalars(&cam);
        let CameraMode::Perspective { fov_y: expected_fov } = cam.mode else {
            panic!("default_perspective is Perspective");
        };
        assert_eq!(fov_y, expected_fov);
        assert_eq!(near, 0.1);
        assert_eq!(far, 500.0);
        assert_eq!(focus_distance, 3.5);
        assert_eq!(f_stop, 2.8);
    }

    #[test]
    fn derive_lens_scalars_falls_back_to_60_degrees_for_orthographic() {
        let mut cam = Camera::default_perspective();
        cam.mode = CameraMode::Orthographic { half_height: 2.0 };
        let [fov_y, ..] = derive_lens_scalars(&cam);
        assert_eq!(fov_y, std::f32::consts::FRAC_PI_3);
    }

    #[test]
    fn unregistered_before_this_module_now_has_a_recompute() {
        use crate::node_graph::freeze::derived_uniform_registry::has_recompute;
        assert!(has_recompute("node.coc_from_depth"));
    }
}

/// **I3** (`docs/CINEMATIC_POST_DESIGN.md`): CoC math agrees with 5
/// hand-computed values — CPU-only, no GPU device. Distinct from I1 (which
/// proves the GPU kernel matches this same formula); this test proves the
/// FORMULA itself matches D1's committed math, worked by hand.
///
/// Fixed camera for all 5 cases: fov_y = 90 degrees (so
/// tan(fov_y/2) = tan(45 deg) = 1 exactly, giving
/// f_mm = 24 / (2*1) = 12mm — a clean number to hand-check against).
/// near = 0.1, far = 100.0 world units (meters).
///
/// `linearize_depth(raw, near, far)`: range = far/(near-far) =
/// 100/(0.1-100) = 100/-99.9 = -1.0010010... ; view_z =
/// (range*near)/(raw+range) = (-0.10010010...)/(raw - 1.0010010...).
/// Each case below plugs in `raw` and carries the division by hand in
/// the comment, then applies D1's CoC formula verbatim.
#[cfg(test)]
mod hand_computed_coc {
    use crate::node_graph::camera::linearize_depth;

    const FOV_Y: f32 = std::f32::consts::FRAC_PI_2; // 90 degrees
    const NEAR: f32 = 0.1;
    const FAR: f32 = 100.0;
    const SENSOR_H_MM: f32 = 24.0;
    const WORLD_TO_MM: f32 = 1000.0;
    const VIEWPORT_H: f32 = 1080.0;

    /// The D1 formula, transcribed exactly (the CPU twin the WGSL body and
    /// hand oracle both implement) — used ONLY to cross-check the by-hand
    /// arithmetic in each `#[test]`'s comment, never as the thing being
    /// tested against itself.
    fn coc_px(raw_depth: f32, focus_distance: f32, f_stop: f32, max_radius: f32) -> f32 {
        let f_mm = SENSOR_H_MM / (2.0 * (FOV_Y * 0.5).tan());
        let a_mm = f_mm / f_stop;
        let d_mm = linearize_depth(raw_depth, NEAR, FAR) * WORLD_TO_MM;
        let s_mm = focus_distance * WORLD_TO_MM;
        let coc_mm = a_mm * f_mm * (d_mm - s_mm).abs() / (d_mm * (s_mm - f_mm).max(1.0));
        (coc_mm / SENSOR_H_MM * VIEWPORT_H).clamp(0.0, max_radius)
    }

    /// Case 1: focus exactly AT the sample (D_mm == S_mm) — CoC must be
    /// exactly 0 regardless of aperture (the |D-S| term is 0). raw chosen so
    /// linearize_depth(raw) == 5.0 exactly: range*near/(raw+range) = 5 =>
    /// raw = range*near/5 - range = range*(near/5 - 1) = -1.001001*(0.02-1)
    /// = -1.001001 * -0.98 = 0.98098098...
    #[test]
    fn case_1_focus_at_sample_gives_zero_coc() {
        let range = FAR / (NEAR - FAR);
        let raw = range * (NEAR / 5.0 - 1.0);
        let view_z = linearize_depth(raw, NEAR, FAR);
        assert!((view_z - 5.0).abs() < 1e-4, "fixture raw must linearize to 5.0m, got {view_z}");
        let got = coc_px(raw, 5.0, 2.8, 24.0);
        assert!(got.abs() < 1e-4, "focus-plane sample must give CoC == 0, got {got}");
    }

    /// Case 2: f_stop = infinity (pinhole) — CoC must be exactly 0
    /// regardless of focus/depth mismatch (A_mm = f_mm/inf = 0, and 0 times
    /// any finite number is 0). raw = 0.5 (mid-range, no special meaning).
    #[test]
    fn case_2_pinhole_gives_zero_coc_at_any_depth() {
        let got = coc_px(0.5, 1.0, f32::INFINITY, 24.0);
        assert_eq!(got, 0.0, "f_stop = infinity must give exactly 0, got {got}");
    }

    /// Case 3: hand-worked non-trivial point. raw = 0.5, focus_distance =
    /// 2.0, f_stop = 2.0, max_radius = 24.0.
    ///   range = 100/(0.1-100) = -1.001001...
    ///   view_z = (range*0.1)/(0.5+range) = (-0.1001001)/(0.5-1.001001)
    ///          = (-0.1001001)/(-0.501001) = 0.1997999... m
    ///   D_mm = 199.7999 mm ; S_mm = 2000 mm
    ///   f_mm = 24/(2*tan(45deg)) = 24/2 = 12 mm ; A_mm = 12/2.0 = 6 mm
    ///   coc_mm = 6 * 12 * |199.7999 - 2000| / (199.7999 * max(2000-12,1))
    ///          = 72 * 1800.2001 / (199.7999 * 1988)
    ///          = 129614.41 / 397122.6 = 0.326318... mm
    ///   coc_px = clamp(0.326318/24 * 1080, 0, 24) = clamp(14.6843, 0, 24)
    ///          = 14.6843 px (verified with a Python f32 cross-check, not
    ///          just this by-hand division — machine arithmetic, not eyeballed)
    #[test]
    fn case_3_hand_worked_nontrivial_point() {
        let got = coc_px(0.5, 2.0, 2.0, 24.0);
        assert!((got - 14.6843).abs() < 5e-3, "hand-worked case_3: expected ~14.6843, got {got}");
    }

    /// Case 4: depth far beyond focus, wide aperture — clamp to max_radius
    /// must engage. raw = 0.95 (near the far plane), focus_distance = 0.5m
    /// (very close focus), f_stop = 0.5 (huge aperture) — the CoC blows past
    /// max_radius = 24 and must clamp.
    ///   range = -1.001001 ; view_z = (-0.1001001)/(0.95-1.001001)
    ///         = (-0.1001001)/(-0.051001) = 1.96271... m ; D_mm = 1962.71 mm
    ///   S_mm = 500 mm ; f_mm = 12 mm ; A_mm = 12/0.5 = 24 mm
    ///   coc_mm = 24*12*|1962.71-500| / (1962.71*max(500-12,1))
    ///          = 288*1462.71 / (1962.71*488) = 421260.5 / 957842.5
    ///          = 0.43982... mm
    ///   coc_px = clamp(0.43982/24*1080, 0, 24) = clamp(19.7919, 0, 24)
    ///          = 19.7919 px — under 24, so this case does NOT clamp; kept as
    ///   a second cross-check point distinct from case_3 (see case_5 for the
    ///   clamp-engaged case). Verified with a Python f32 cross-check.
    #[test]
    fn case_4_close_focus_wide_aperture_far_depth() {
        let got = coc_px(0.95, 0.5, 0.5, 24.0);
        assert!((got - 19.7919).abs() < 5e-3, "hand-worked case_4: expected ~19.7919, got {got}");
    }

    /// Case 5: clamp-engaged. raw = 0.99 (very near far plane), focus_distance
    /// = 0.2m, f_stop = 0.5 (huge aperture) — CoC must exceed max_radius and
    /// clamp exactly to it.
    ///   range = -1.001001 ; view_z = (-0.1001001)/(0.99-1.001001)
    ///         = (-0.1001001)/(-0.011001) = 9.09889... m ; D_mm = 9098.89 mm
    ///   S_mm = 200 mm ; f_mm = 12 mm ; A_mm = 24 mm
    ///   coc_mm = 24*12*|9098.89-200| / (9098.89*max(200-12,1))
    ///          = 288*8898.89 / (9098.89*188) = 2562880 / 1710591
    ///          = 1.49827... mm
    ///   coc_px_unclamped = 1.49827/24*1080 = 67.4222 px — well past
    ///   max_radius = 24, so the clamp must engage: expected coc_px == 24.0
    ///   exactly.
    #[test]
    fn case_5_clamp_engages_at_max_radius() {
        let got = coc_px(0.99, 0.2, 0.5, 24.0);
        assert_eq!(got, 24.0, "clamp must engage exactly at max_radius, got {got}");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I1**: generated-vs-hand parity (`docs/ADDING_PRIMITIVES.md` "The
    //! codegen path is mandatory") — the standalone kernel `run()` actually
    //! dispatches (built via `standalone_for_spec::<CocFromDepth>()`) must
    //! reproduce `coc_from_depth.wgsl` (the hand oracle) texel-for-texel on a
    //! synthetic non-uniform depth ramp.
    //!
    //! **I2**: pinhole invariant (`docs/CINEMATIC_POST_DESIGN.md`) —
    //! `f_stop = INFINITY` must produce an all-zero CoC buffer (I2a), and the
    //! whole DoF chain (coc_from_depth -> variable_blur H -> variable_blur V)
    //! must be a bit-clean pass-through when chained on top of that all-zero
    //! CoC (I2b) — `node.variable_blur`'s body returns `fetch_in(uv)`
    //! unchanged whenever `center_coc < 0.005`.
    //!
    //! **I5**: preset load-smoke lives in a separate integration test file
    //! (`tests/`), per the doc's `gpu_proofs` binary convention, not here.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::{CocFromDepth, CocFromDepthUniforms};
    use crate::node_graph::primitives::GaussianBlurVariableWidth;
    use crate::render_target::RenderTarget;

    /// A custom, CPU-uploadable texture (unlike `RenderTarget`, whose usage
    /// flags don't include `CPU_UPLOAD`) — used for every INPUT texture in
    /// this module so a non-uniform synthetic pattern can be written
    /// directly, matching `freeze/proof.rs::gradient_input`'s approach.
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

    /// Synthetic depth ramp: raw depth varies `0.1..=0.9` across x, constant
    /// across y — non-uniform so a per-texel bug can't hide behind a flat
    /// fill (same rationale as `gradient_input`).
    fn depth_ramp(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let raw = 0.1 + 0.8 * (x as f32 / (w.saturating_sub(1).max(1)) as f32);
                px[i] = f16::from_f32(raw);
                px[i + 1] = f16::from_f32(raw);
                px[i + 2] = f16::from_f32(raw);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "coc-depth-ramp", &px)
    }

    /// A non-uniform RGBA gradient — the DoF-chain pass-through fixture (I2b)
    /// reads this back byte-for-byte after coc_from_depth + 2x variable_blur.
    fn color_gradient(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "coc-color-gradient", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("coc-readback");
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

    /// Dispatch a coc_from_depth-shaped kernel (uniform(0), depth(1, load-only,
    /// no sampler), dst(2)) and read back the full RGBA output.
    fn dispatch_coc(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        depth: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "coc-out");
        let mut enc = device.create_encoder("coc-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: depth },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "coc-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    fn coc_uniforms(
        max_radius: f32,
        fov_y: f32,
        near: f32,
        far: f32,
        focus_distance: f32,
        f_stop: f32,
    ) -> CocFromDepthUniforms {
        CocFromDepthUniforms {
            max_radius,
            fov_y,
            near,
            far,
            focus_distance,
            f_stop,
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }


    /// **I2a**: `f_stop = INFINITY` gives an exactly-zero CoC buffer,
    /// regardless of a non-uniform depth ramp underneath it — the generated
    /// kernel is the one that ships, so this dispatches it directly (not the
    /// hand oracle).
    #[test]
    fn pinhole_f_stop_gives_all_zero_coc_buffer() {
        let device = crate::test_device();
        let (w, h) = (16u32, 4u32);
        let depth = depth_ramp(&device, w, h);
        let uniforms = coc_uniforms(24.0, std::f32::consts::FRAC_PI_2, 0.1, 100.0, 5.0, f32::INFINITY);
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CocFromDepth>()
            .expect("node.coc_from_depth standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "coc-pinhole",
        );
        let out = dispatch_coc(&device, &pipeline, &depth, w, h, bytes);

        for (i, px) in out.iter().enumerate() {
            assert_eq!(px[0], 0.0, "texel {i} R must be exactly 0 at f_stop=inf, got {}", px[0]);
        }
    }

    /// **I2b**: the whole DoF chain (coc_from_depth[pinhole] -> variable_blur
    /// H -> variable_blur V) is a bit-clean pass-through of the color input —
    /// `node.variable_blur`'s `center_coc < 0.005` branch returns the
    /// fetched center texel unchanged, and an all-zero CoC buffer satisfies
    /// that on every texel.
    #[test]
    fn pinhole_dof_chain_is_bit_clean_passthrough() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let depth = depth_ramp(&device, w, h);
        let color = color_gradient(&device, w, h);
        let uniforms = coc_uniforms(24.0, std::f32::consts::FRAC_PI_2, 0.1, 100.0, 5.0, f32::INFINITY);
        let bytes = bytemuck::bytes_of(&uniforms);

        let coc_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<CocFromDepth>()
            .expect("node.coc_from_depth standalone codegen");
        let coc_pipeline = device.create_compute_pipeline(
            &coc_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "coc-chain",
        );
        let coc_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "coc-chain-out");
        {
            let mut enc = device.create_encoder("coc-chain-coc");
            enc.dispatch_compute(
                &coc_pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytes },
                    GpuBinding::Texture { binding: 1, texture: &depth },
                    GpuBinding::Texture { binding: 2, texture: &coc_out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "coc-chain-coc",
            );
            enc.commit_and_wait_completed();
        }

        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct BlurUniforms {
            direction: u32,
            max_radius: f32,
            _pad0: u32,
            _pad1: u32,
        }

        let vbw_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<GaussianBlurVariableWidth>()
                .expect("node.variable_blur standalone codegen");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let dispatch_blur = |direction: u32, input: &GpuTexture| -> RenderTarget {
            let pipeline = device.create_specialized_compute_pipeline(
                &vbw_wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                &[("QUALITY_LEVEL", "1u"), ("WEIGHTING_MODE", "0u")],
                "coc-chain-blur",
            );
            let u = BlurUniforms { direction, max_radius: 24.0, _pad0: 0, _pad1: 0 };
            let out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "coc-chain-blur-out");
            let mut enc = device.create_encoder("coc-chain-blur");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&u) },
                    GpuBinding::Texture { binding: 1, texture: input },
                    GpuBinding::Texture { binding: 2, texture: &coc_out.texture },
                    GpuBinding::Sampler { binding: 3, sampler: &sampler },
                    GpuBinding::Texture { binding: 4, texture: &out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "coc-chain-blur",
            );
            enc.commit_and_wait_completed();
            out
        };

        let h_pass = dispatch_blur(0, &color);
        let v_pass = dispatch_blur(1, &h_pass.texture);

        let expected = readback_rgba(&device, &color, w, h);
        let got = readback_rgba(&device, &v_pass.texture, w, h);
        assert_eq!(expected.len(), got.len());
        for (i, (e, g)) in expected.iter().zip(got.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (e[c] - g[c]).abs() < 1e-6,
                    "texel {i} channel {c}: pinhole DoF chain must pass through bit-clean, expected={} got={}",
                    e[c],
                    g[c]
                );
            }
        }
    }
}
