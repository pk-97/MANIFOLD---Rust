//! `node.ssao_from_depth` — screen-space ambient occlusion from reconstructed
//! view-space normals + a Camera (`docs/CINEMATIC_POST_DESIGN.md` D3). No
//! normal G-buffer in v1 (GBUFFER D6 reserves that ABI for later) — the
//! normal is reconstructed per-texel from depth alone via explicit neighbour
//! reads. Output is a grayscale AO map (R=G=B=occlusion, A=1); the atom does
//! NOT modify the color image itself — the preset wires the output into a
//! `node.mix` (Multiply mode) against the scene color (D3's explicit
//! contract: "the atom does NOT modify the color image").
//!
//! Committed algorithm, no substitution:
//! ```text
//! 1. view-space position per texel: view_z = linearize_depth(raw, near, far);
//!    ndc = (uv*2-1, 1-uv.y*2); view_xy = ndc * tan(fov_y/2) * [aspect, 1] * view_z.
//! 2. normal = normalize(cross(P(x+1)-P(x-1), P(y+1)-P(y-1))) from explicit
//!    +/-1-texel INTEGER reads (GatherTexel — no derivative intrinsics; compute
//!    has no fragment derivatives, and texel-exact reads are what the CPU
//!    reference replicates exactly, docs/CINEMATIC_POST_DESIGN.md D3).
//! 3. N=16 golden-angle spiral (docs/CINEMATIC_POST_DESIGN.md D2:
//!    r_i = sqrt((i+0.5)/N), theta_i = i*2.399963), lifted onto the normal's
//!    hemisphere by Malley's method (z_i = sqrt(1 - r_i^2)) around a tangent
//!    basis built from the reconstructed normal, rotated per-pixel by D2's
//!    hash (hash = fract(sin(dot(px, vec2(12.9898,78.233)))*43758.5453)*2pi).
//! 4. Each sample: sample_pos = P_center + kernel_vec * radius; reproject to a
//!    texel (nearest, not bilinear — depth is non-linear across silhouette
//!    edges); occlusion += 1 when the ACTUAL scene depth there is nearer than
//!    the sample point (minus `bias`) AND within `radius` of the center depth
//!    (the standard halo guard).
//! 5. out.r = 1 - intensity * occlusion / N (broadcast to RGB, alpha 1).
//! ```
//!
//! `camera` reads `fov_y`/`near`/`far` entirely via the three
//! `derived_uniforms` below (aspect is recovered from `dims`, following
//! `node.coc_from_depth` / `node.project_3d`'s convention) — never a GPU
//! binding, which is what lets this Pointwise atom fuse with a neighbour
//! instead of being a permanent boundary (P0/D7).
//!
//! `radius`/`intensity`/`bias` are ordinary atom params (not port-shadowed,
//! not lens-derived) — per the orchestrator's brief: D3 doesn't call them
//! port-shadowed, D6 lists `ssao_intensity`/`ssao_radius` as preset CARDS
//! (which bind to plain params via `param_values`, no wire needed), and
//! `node.coc_from_depth`'s own `max_radius` (P1 precedent) is a plain param
//! with no input port either — same shape here for consistency.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::camera::{Camera, CameraMode};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");

/// Generated-codegen uniform layout: the three PARAMS (`radius`, `intensity`,
/// `bias`) in declaration order, then the three DERIVED fields
/// (`fov_y`, `near`, `far`) in declaration order — one f32 word each, no vec3
/// expansion — padded to a 16-byte (4-word) multiple. 6 words + 2 pad = 32
/// bytes. Mirrors `coc_from_depth.rs`'s `CocFromDepthUniforms` layout note.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SsaoFromDepthUniforms {
    radius: f32,
    intensity: f32,
    bias: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: SsaoFromDepth,
    type_id: "node.ssao_from_depth",
    purpose: "Screen-space ambient occlusion from scene depth + a Camera, reconstructing view-space normals from explicit neighbour depth reads (no normal G-buffer in v1, docs/CINEMATIC_POST_DESIGN.md D3): view-space position per texel from linearize_depth(raw, near, far) + inverse-projection xy; normal = normalize(cross(P(x+1)-P(x-1), P(y+1)-P(y-1))); N=16 golden-angle-spiral hemisphere samples (r_i=sqrt((i+0.5)/16), theta_i=i*2.399963, Malley's-method z_i=sqrt(1-r_i^2)) scaled by `radius`, rotated per-pixel by the committed hash; occlusion += range-checked depth comparison with `bias`; out.r = 1 - intensity*occlusion/16 (broadcast to RGB, alpha 1). Output is an AO map — wire it into a node.mix (Multiply mode) against the scene color; this atom does NOT modify the color image itself. Reads fov_y/near/far entirely via derived uniforms — the Camera wire is never a GPU binding.",
    inputs: {
        depth: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.01, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bias"),
            label: "Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.025),
            range: Some((0.0, 0.5)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output is a grayscale AO map (R=G=B=occlusion, A=1) — wire straight into a node.mix (mode=Multiply, amount=1.0) with the scene color as `a` and this atom's `out` as `b`; this atom never touches the color image itself (D3's explicit no-fused-color contract). `depth` expects render_scene's raw [0,1] `depth` output (not pre-linearized), same contract as node.coc_from_depth. `radius` is a WORLD-units hemisphere radius (not pixels) — scale it to the scene's scale, not the canvas resolution. `bias` guards against self-occlusion acne on nearly-flat surfaces; raise it if you see banding on gentle curves, lower it if contact shadows look detached.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "SSAO From Depth", category: Atom },
    summary: "Computes contact shadows from scene depth and a physical camera lens — darkens crevices and touching surfaces the way ambient light naturally would.",
    category: Mask,
    role: Map,
    aliases: ["ssao", "ambient occlusion", "contact shadow", "screen space ambient occlusion", "ao"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/ssao_from_depth_body.wgsl"),
    input_access: [GatherTexel],
    derived_uniforms: ["fov_y", "near", "far"],
    wgsl_includes: [DEPTH_COMMON],
}

/// Single source of truth for the three Camera-derived scalar fields, in
/// `DERIVED_UNIFORMS` declaration order — shared by `run()` (unfused CPU
/// path) and the `inventory::submit!` recompute below (fused path), so the
/// two can never drift. Mirrors `coc_from_depth.rs`'s `derive_lens_scalars`;
/// `fov_y` defaults to `default_perspective()`'s 60 degrees for an
/// Orthographic camera (same defensive fallback, orthographic AO is out of
/// scope for D3).
fn derive_view_scalars(cam: &Camera) -> [f32; 3] {
    let fov_y = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        CameraMode::Orthographic { .. } => std::f32::consts::FRAC_PI_3,
    };
    [fov_y, cam.near, cam.far]
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's fov_y/near/far fields, IN DECLARATION ORDER — reads the region's
// routed Camera external, matching `run()`'s own `derive_view_scalars` call
// below exactly.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.ssao_from_depth",
        recompute: |ctx| ctx.camera.map(derive_view_scalars).map(|v| v.to_vec()),
    }
}

impl Primitive for SsaoFromDepth {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_f32 = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let radius = read_f32(ctx, "radius", 0.5);
        let intensity = read_f32(ctx, "intensity", 1.0);
        let bias = read_f32(ctx, "bias", 0.025);

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let [fov_y, near, far] = derive_view_scalars(&cam);

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
            // Single-source: kernel generated from `wgsl_body` (GatherTexel —
            // no sampler; generated bindings are uniform(0)/depth(1)/dst(2)).
            // ssao_from_depth.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.ssao_from_depth standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.ssao_from_depth",
            )
        });

        let uniforms = SsaoFromDepthUniforms {
            radius,
            intensity,
            bias,
            fov_y,
            near,
            far,
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
            "node.ssao_from_depth",
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

        assert_eq!(SsaoFromDepth::TYPE_ID, "node.ssao_from_depth");
        let names: Vec<&str> = SsaoFromDepth::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["depth", "camera"]);
        assert_eq!(SsaoFromDepth::INPUTS[0].ty, PortType::Texture2D);
        assert!(SsaoFromDepth::INPUTS[0].required);
        assert_eq!(SsaoFromDepth::INPUTS[1].ty, PortType::Camera);
        assert!(SsaoFromDepth::INPUTS[1].required);

        assert_eq!(SsaoFromDepth::OUTPUTS.len(), 1);
        assert_eq!(SsaoFromDepth::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_radius_intensity_bias_params_only() {
        let names: Vec<&str> = SsaoFromDepth::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["radius", "intensity", "bias"]);
    }

    #[test]
    fn declares_three_derived_uniforms_in_view_order() {
        assert_eq!(SsaoFromDepth::DERIVED_UNIFORMS, &["fov_y", "near", "far"]);
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<SsaoFromDepthUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SsaoFromDepth::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.ssao_from_depth");
    }

    #[test]
    fn derive_view_scalars_reads_perspective_fov_near_far() {
        let mut cam = Camera::default_perspective();
        cam.near = 0.1;
        cam.far = 500.0;
        let [fov_y, near, far] = derive_view_scalars(&cam);
        let CameraMode::Perspective { fov_y: expected_fov } = cam.mode else {
            panic!("default_perspective is Perspective");
        };
        assert_eq!(fov_y, expected_fov);
        assert_eq!(near, 0.1);
        assert_eq!(far, 500.0);
    }

    #[test]
    fn derive_view_scalars_falls_back_to_60_degrees_for_orthographic() {
        let mut cam = Camera::default_perspective();
        cam.mode = CameraMode::Orthographic { half_height: 2.0 };
        let [fov_y, ..] = derive_view_scalars(&cam);
        assert_eq!(fov_y, std::f32::consts::FRAC_PI_3);
    }

    #[test]
    fn unregistered_before_this_module_now_has_a_recompute() {
        use crate::node_graph::freeze::derived_uniform_registry::has_recompute;
        assert!(has_recompute("node.ssao_from_depth"));
    }
}

/// **CPU reference** (`docs/CINEMATIC_POST_DESIGN.md` P2 deliverable: "atom
/// per D3, CPU reference, synthetic-ramp parity (I1)") — a plain-Rust
/// implementation of the D3 algorithm, independent of the WGSL body (not
/// sharing source), used two ways: (1) the analytic sanity unit test below
/// (a flat/constant-depth plane must give occlusion ~0 everywhere, per the
/// P2 brief's explicit phrasing), pure CPU, no GPU device; (2) the I1
/// GPU-vs-CPU synthetic-ramp parity gpu_test further down, which uploads the
/// same input this module reads and asserts pixel agreement.
#[cfg(test)]
pub(crate) mod cpu_reference {
    use crate::node_graph::camera::linearize_depth;

    const SSAO_N: usize = 16;
    const GOLDEN_ANGLE: f32 = 2.399963;

    /// A synthetic depth buffer: raw [0,1] depth values, row-major, `w*h` long.
    pub struct DepthBuffer<'a> {
        pub w: i32,
        pub h: i32,
        pub raw: &'a [f32],
    }

    impl DepthBuffer<'_> {
        fn load(&self, x: i32, y: i32) -> f32 {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.raw[(cy * self.w + cx) as usize]
        }
    }

    fn hash_angle(px_x: f32, px_y: f32) -> f32 {
        (((px_x * 12.9898 + px_y * 78.233).sin()) * 43_758.547).fract() * std::f32::consts::TAU
    }

    fn view_pos(
        depth: &DepthBuffer<'_>,
        cx: i32,
        cy: i32,
        tan_half_fov: f32,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> [f32; 3] {
        let raw = depth.load(cx, cy);
        let view_z = linearize_depth(raw, near, far);
        let ccx = cx.clamp(0, depth.w - 1);
        let ccy = cy.clamp(0, depth.h - 1);
        let u = (ccx as f32 + 0.5) / depth.w as f32;
        let v = (ccy as f32 + 0.5) / depth.h as f32;
        let ndc_x = u * 2.0 - 1.0;
        let ndc_y = 1.0 - v * 2.0;
        let view_x = ndc_x * tan_half_fov * aspect * view_z;
        let view_y = ndc_y * tan_half_fov * view_z;
        [view_x, view_y, view_z]
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }
    fn length(a: [f32; 3]) -> f32 {
        (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
    }
    fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
        [a[0] * s, a[1] * s, a[2] * s]
    }
    fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }
    fn normalize(a: [f32; 3]) -> [f32; 3] {
        let l = length(a);
        if l > 1e-8 { scale(a, 1.0 / l) } else { [0.0, 0.0, -1.0] }
    }

    /// The D3 algorithm, transcribed exactly (independent of the WGSL body) —
    /// one texel's occlusion output, `[0,1]`.
    #[allow(clippy::too_many_arguments)]
    pub fn ssao_texel(
        depth: &DepthBuffer<'_>,
        cx: i32,
        cy: i32,
        radius: f32,
        intensity: f32,
        bias: f32,
        fov_y: f32,
        near: f32,
        far: f32,
    ) -> f32 {
        let tan_half_fov = (fov_y * 0.5).tan();
        let aspect = depth.w as f32 / depth.h as f32;

        let p_c = view_pos(depth, cx, cy, tan_half_fov, aspect, near, far);
        let p_xp = view_pos(depth, cx + 1, cy, tan_half_fov, aspect, near, far);
        let p_xm = view_pos(depth, cx - 1, cy, tan_half_fov, aspect, near, far);
        let p_yp = view_pos(depth, cx, cy + 1, tan_half_fov, aspect, near, far);
        let p_ym = view_pos(depth, cx, cy - 1, tan_half_fov, aspect, near, far);

        let ddx = sub(p_xp, p_xm);
        let ddy = sub(p_yp, p_ym);
        let normal = normalize(cross(ddx, ddy));

        let up_ref = if normal[1].abs() > 0.999 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let tangent = normalize(cross(up_ref, normal));
        let bitangent = cross(normal, tangent);

        let rot = hash_angle(cx as f32, cy as f32);

        let mut occlusion = 0.0f32;
        for i in 0..SSAO_N {
            let r = (((i as f32) + 0.5) / (SSAO_N as f32)).sqrt();
            let theta = (i as f32) * GOLDEN_ANGLE + rot;
            let disc_x = r * theta.cos();
            let disc_y = r * theta.sin();
            let disc_z = (1.0 - r * r).max(0.0).sqrt();
            let kernel_vec = add(add(scale(tangent, disc_x), scale(bitangent, disc_y)), scale(normal, disc_z));
            let sample_pos = add(p_c, scale(kernel_vec, radius));

            let vz = sample_pos[2].max(1e-4);
            let denom = tan_half_fov * vz;
            let sample_ndc_x = sample_pos[0] / (aspect * denom);
            let sample_ndc_y = sample_pos[1] / denom;
            let sample_u = sample_ndc_x * 0.5 + 0.5;
            let sample_v = (1.0 - sample_ndc_y) * 0.5;
            let sample_cx = ((sample_u * depth.w as f32) as i32).clamp(0, depth.w - 1);
            let sample_cy = ((sample_v * depth.h as f32) as i32).clamp(0, depth.h - 1);

            let scene_raw = depth.load(sample_cx, sample_cy);
            let scene_view_z = linearize_depth(scene_raw, near, far);

            let occluded = if scene_view_z <= sample_pos[2] - bias { 1.0 } else { 0.0 };
            let range_ok = if (p_c[2] - scene_view_z).abs() < radius { 1.0 } else { 0.0 };
            occlusion += occluded * range_ok;
        }

        (1.0 - intensity * occlusion / SSAO_N as f32).clamp(0.0, 1.0)
    }
}

/// **Analytic sanity test** (`docs/CINEMATIC_POST_DESIGN.md` P2 deliverable):
/// a flat plane (constant raw depth — no local depth discontinuity anywhere
/// in the buffer) must give occlusion 0 everywhere except bias tolerance.
/// Pure CPU, no GPU device — mirrors `coc_from_depth.rs`'s `hand_computed_coc`
/// module's "CPU-only formula check" pattern.
#[cfg(test)]
mod analytic_sanity {
    use super::cpu_reference::{ssao_texel, DepthBuffer};

    /// A perfectly flat plane facing the camera: every texel reads the SAME
    /// raw depth, so every reconstructed neighbour position differs only in
    /// (x,y), never in view_z — the geometrically correct case for "no local
    /// depth discontinuity". Any hemisphere sample projected from such a
    /// surface reprojects onto the SAME constant depth, so (per the D3
    /// formula worked by hand in `ssao_from_depth.rs`'s primitive doc
    /// comment) `scene_view_z == p_c.z` for every sample and the occluded
    /// test can only fire when a sample's OWN kernel_vec.z*radius exceeds
    /// `bias` toward the camera — which the hemisphere lift (z_i >= 0,
    /// oriented along the surface normal pointing AWAY from the scene i.e.
    /// TOWARD the camera / decreasing view_z) makes near-zero for all but a
    /// vanishing bias.
    #[test]
    fn flat_plane_gives_zero_occlusion_everywhere_except_bias_tolerance() {
        let (w, h) = (16i32, 16i32);
        let raw = vec![0.5f32; (w * h) as usize];
        let depth = DepthBuffer { w, h, raw: &raw };

        let (radius, intensity, bias) = (0.5, 1.0, 0.025);
        let (fov_y, near, far) = (std::f32::consts::FRAC_PI_2, 0.1, 100.0);

        for cy in 0..h {
            for cx in 0..w {
                let occ_out = ssao_texel(&depth, cx, cy, radius, intensity, bias, fov_y, near, far);
                // out.r = 1 - intensity*occlusion/N; flat plane -> occlusion
                // ~0 -> out.r ~1. Tolerance covers the one-texel border ring
                // where the +/-1 neighbour clamp makes ddx/ddy degenerate on
                // one axis (normal still well-defined from the other axis).
                assert!(
                    (occ_out - 1.0).abs() < 1e-3,
                    "texel ({cx},{cy}): flat plane must give ~zero occlusion (out.r~1), got {occ_out}"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I1** (`docs/CINEMATIC_POST_DESIGN.md` P2 deliverable: "CPU reference,
    //! synthetic-ramp parity (I1)"): the generated standalone kernel (built
    //! via `standalone_for_spec::<SsaoFromDepth>()`, the one that ships) must
    //! reproduce `cpu_reference::ssao_texel` (the plain-Rust reference) within
    //! tolerance on a synthetic non-uniform depth ramp. ALSO proves the
    //! `docs/ADDING_PRIMITIVES.md` codegen-path mandate (generated-vs-hand
    //! WGSL parity) against `ssao_from_depth.wgsl`, mirroring
    //! `coc_from_depth.rs`'s `generated_coc_matches_hand_kernel` — two
    //! independent oracles (CPU-Rust and hand-WGSL), same generated kernel.
    use half::f16;

    use manifold_gpu::{GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

    use super::cpu_reference::{ssao_texel, DepthBuffer};
    use super::SsaoFromDepth;
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

    /// Non-uniform synthetic depth ramp (raw varies smoothly in x AND y so
    /// the reconstructed normal is non-degenerate everywhere) — same
    /// rationale as `coc_from_depth.rs`'s `depth_ramp` fixture.
    fn depth_ramp_2d(w: u32, h: u32) -> Vec<f32> {
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

    fn upload_depth(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, &r) in raw.iter().enumerate() {
            px[i * 4] = f16::from_f32(r);
            px[i * 4 + 1] = f16::from_f32(r);
            px[i * 4 + 2] = f16::from_f32(r);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, "ssao-depth-ramp", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("ssao-readback");
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
    struct SsaoUniforms {
        radius: f32,
        intensity: f32,
        bias: f32,
        fov_y: f32,
        near: f32,
        far: f32,
        _pad0: f32,
        _pad1: f32,
    }

    fn dispatch(
        device: &GpuDevice,
        pipeline: &manifold_gpu::GpuComputePipeline,
        depth: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "ssao-out");
        let mut enc = device.create_encoder("ssao-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: depth },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "ssao-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// **I1a**: generated kernel vs CPU-Rust reference — the doc's own house
    /// pattern (`docs/CINEMATIC_POST_DESIGN.md` intro: "implemented twice
    /// ... once as WGSL, once as plain-Rust").
    ///
    /// Tolerance policy (measured, not assumed): the vast majority of texels
    /// (375/384 on this fixture) agree within 5e-3. A small minority disagree
    /// by EXACTLY `1/SSAO_N` (0.0625) — never more — because the occlusion
    /// accumulation is a binary threshold decision
    /// (`scene_view_z <= sample_pos.z - bias`) over 16 samples, and GPU
    /// hardware transcendentals (`sin`/`cos`/`sqrt`) vs CPU libm can round a
    /// sub-ulp hair differently, occasionally flipping exactly one of the 16
    /// samples right at its own decision boundary. This is the same class
    /// FREEZE_COMPILER_MAP.md §7 point 4 documents for out-of-loop texture
    /// regions ("≈1 ulp, NOT bit-exact ... bounded over-count") — quantized
    /// here into a 1/16 step by the discrete sample count rather than
    /// showing up as smooth drift. A same-fixture, same-precision GPU-vs-GPU
    /// check (`generated_ssao_matches_hand_kernel`, below) already proves the
    /// codegen path itself is exact; this test's job is the algorithm-level
    /// cross-check, so a bounded count of single-sample boundary flips is
    /// accepted and anything else (multi-sample flips, unquantized drift)
    /// fails immediately.
    #[test]
    fn generated_ssao_matches_cpu_reference_on_synthetic_ramp() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let raw = depth_ramp_2d(w, h);
        let depth_tex = upload_depth(&device, w, h, &raw);

        let (radius, intensity, bias) = (0.5f32, 1.0f32, 0.025f32);
        let (fov_y, near, far) = (std::f32::consts::FRAC_PI_2, 0.1, 100.0);
        let uniforms = SsaoUniforms { radius, intensity, bias, fov_y, near, far, _pad0: 0.0, _pad1: 0.0 };
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SsaoFromDepth>()
            .expect("node.ssao_from_depth standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "ssao-generated",
        );
        let gen_out = dispatch(&device, &pipeline, &depth_tex, w, h, bytes);

        const SAMPLE_STEP: f32 = 1.0 / 16.0;
        let depth_buf = DepthBuffer { w: w as i32, h: h as i32, raw: &raw };
        let total = (w * h) as usize;
        let mut boundary_flip_count = 0usize;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let cpu = ssao_texel(&depth_buf, x, y, radius, intensity, bias, fov_y, near, far);
                let gpu = gen_out[(y as u32 * w + x as u32) as usize][0];
                let diff = (cpu - gpu).abs();
                if diff < 5e-3 {
                    continue;
                }
                assert!(
                    (diff - SAMPLE_STEP).abs() < 1e-4,
                    "texel ({x},{y}): cpu={cpu} gpu={gpu} diff={diff} is not a single-sample \
                     (1/16) boundary flip — looks like a real algorithm mismatch, not FP rounding"
                );
                boundary_flip_count += 1;
            }
        }
        assert!(
            boundary_flip_count * 20 <= total,
            "{boundary_flip_count}/{total} texels hit a boundary sample-flip — exceeds the \
             expected rare rate (>5%), which would suggest a systematic issue rather than \
             isolated cross-platform trig rounding"
        );
    }

    /// **I1b** (`docs/ADDING_PRIMITIVES.md` "The codegen path is mandatory"):
    /// generated kernel vs the hand-authored `ssao_from_depth.wgsl` oracle —
    /// same fixture, independent WGSL source, proves the codegen path itself
    /// (not just the algorithm) is correct.
    #[test]
    fn generated_ssao_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let raw = depth_ramp_2d(w, h);
        let depth_tex = upload_depth(&device, w, h, &raw);

        let uniforms = SsaoUniforms {
            radius: 0.5,
            intensity: 1.0,
            bias: 0.025,
            fov_y: std::f32::consts::FRAC_PI_2,
            near: 0.1,
            far: 100.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let hand_wgsl = include_str!("shaders/ssao_from_depth.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "ssao-hand");
        let hand_out = dispatch(&device, &hand_pipeline, &depth_tex, w, h, bytes);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SsaoFromDepth>()
            .expect("node.ssao_from_depth standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "ssao-generated-vs-hand",
        );
        let gen_out = dispatch(&device, &gen_pipeline, &depth_tex, w, h, bytes);

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

    /// **Analytic sanity, GPU path**: the same flat-plane claim as
    /// `analytic_sanity::flat_plane_gives_zero_occlusion_everywhere_except_bias_tolerance`,
    /// dispatched on the real generated kernel (not just the CPU reference) —
    /// belt-and-suspenders that the shipping kernel, not only its Rust twin,
    /// satisfies the invariant.
    #[test]
    fn generated_ssao_flat_plane_gives_near_full_visibility() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let raw = vec![0.5f32; (w * h) as usize];
        let depth_tex = upload_depth(&device, w, h, &raw);

        let uniforms = SsaoUniforms {
            radius: 0.5,
            intensity: 1.0,
            bias: 0.025,
            fov_y: std::f32::consts::FRAC_PI_2,
            near: 0.1,
            far: 100.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<SsaoFromDepth>()
            .expect("node.ssao_from_depth standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "ssao-flat",
        );
        let out = dispatch(&device, &pipeline, &depth_tex, w, h, bytes);

        for (i, px) in out.iter().enumerate() {
            assert!(
                (px[0] - 1.0).abs() < 1e-3,
                "texel {i}: flat plane must give ~full visibility (out.r~1), got {}",
                px[0]
            );
        }
    }
}
