//! `node.ssao_gtao` — Ground Truth Ambient Occlusion (Jimenez et al. 2016)
//! from scene depth and a Camera, sampled as Intel's XeGTAO does. Output is a grayscale AO map
//! (R=G=B=occlusion, A=1) that the graph multiplies onto the colour with a
//! `node.mix`; the atom never touches the colour itself.
//!
//! Per pixel: reconstruct the view-space position and the normal (XeGTAO's
//! edge-aware reconstruction, `view_normal_common.wgsl`, so a pixel beside a
//! silhouette keeps its own surface's normal); for each of `slices` screen directions, march
//! `steps` taps per side out to 1.457 x `radius` (XeGTAO's radius
//! multiplier; world units, projected at the pixel's depth), keep each
//! side's highest horizon, and integrate the
//! visible arc against the normal. Visibility divides by the fully-open arc
//! of each slice (`full_ref`), not by the slice count, so flat ground stays
//! white at grazing angles (BUG-y5w7).
//!
//! What keeps it free of lines: a tap's horizon fades to the open horizon
//! over the outer 61.5% of that reach instead of being cut at it, so
//! occlusion is about half faded at `radius`;
//! each pixel offsets its taps by its own fraction of a step; and slice angle
//! and step offset come from XeGTAO's spatio-temporal noise (Hilbert index
//! walked by the R2 sequence). With `noise_per_frame` on the pattern moves
//! one step on each new frame, for `node.temporal_accumulate` to average;
//! off, it holds still. A new frame is a new render pass of the host (one per
//! content tick, so the pattern keeps moving while the transport is stopped
//! and the history never settles on one pattern's grain) or a new host frame
//! count; rendering the same frame again draws the same pattern, so a frame
//! pumped until it holds still comes out byte for byte the same.
//! `clear_state` restarts the count, and export clears state before its
//! first frame, so two exports draw the same noise.
//!
//! The Hilbert index, the R2 noise, the falloff and the normal are ported
//! from XeGTAO (Intel, MIT license; see THIRD_PARTY_NOTICES.md).
//!
//! The run count is state that crosses frames, so this atom is a fusion
//! boundary: a fused kernel could not advance it. Its kernel is still
//! generated from `wgsl_body`.

use std::borrow::Cow;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::dispatch_standalone_2d;
use manifold_node_engine::scene::camera::{Camera, CameraMode};

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");
const VIEW_NORMAL_COMMON: &str = include_str!("shaders/view_normal_common.wgsl");

/// Display labels for the `projection` enum, indexed by enum value.
pub const GTAO_PROJECTIONS: &[&str] = &["Scene Depth", "Height Field"];

/// XeGTAO's temporal cycle: the pattern repeats after this many steps.
const NOISE_CYCLE: u32 = 64;
/// XeGTAO's Hilbert-index offset between consecutive frames.
const NOISE_FRAME_STRIDE: u32 = 288;

/// The generated uniform layout: the params in declaration order, then the
/// derived fields in declaration order, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SsaoGtaoUniforms {
    radius: f32,
    intensity: f32,
    slices: f32,
    steps: f32,
    projection: u32,
    relief: f32,
    noise_per_frame: u32,
    fov_y: f32,
    near: f32,
    far: f32,
    noise_frame: u32,
    _pad0: f32,
}

manifold_node_engine::primitive! {
    name: SsaoGtao,
    type_id: "node.ssao_gtao",
    purpose: "Ground Truth Ambient Occlusion from scene depth and a Camera, sampled as XeGTAO does. Per pixel: view-space position from depth, normal from XeGTAO's edge-aware reconstruction (each corner of the four neighbours weighted by how well they follow the local depth slope, so silhouette pixels keep their own surface's normal); `slices` screen directions, `steps` taps per side out to 1.457 x `radius` (XeGTAO's radius multiplier; world units projected at the pixel's depth); each tap's horizon fades to the open horizon over the outer 61.5% of that reach (no hard cut), so occlusion is about half faded at `radius`; per-pixel step offsets and slice angles from XeGTAO's Hilbert/R2 spatio-temporal noise; the visible arc integrated against the normal and divided by the slice's fully-open arc. out.r = clamp(1 - intensity*(1-visibility), 0, 1), broadcast to RGB, alpha 1. `noise_per_frame` moves the noise one step on each new frame (a new host render pass, which moves while the transport is stopped, or a new frame count), for a temporal filter; the same frame rendered again keeps its noise; clear_state restarts the sequence; off, the pattern holds still. The camera is read only through derived uniforms.",
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
        // Quality: taps = slices * 2 * steps (16 by default).
        ParamDef {
            name: Cow::Borrowed("slices"),
            label: "Slices",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("steps"),
            label: "Steps",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((1.0, 16.0)),
            enum_values: &[],
        },
        // `Height Field` reads `depth` as a raw height map in an orthographic
        // frame: position = (uv.x*aspect, uv.y, raw*relief), radius in uv units.
        ParamDef {
            name: Cow::Borrowed("projection"),
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: GTAO_PROJECTIONS,
        },
        ParamDef {
            name: Cow::Borrowed("relief"),
            label: "Relief",
            ty: ParamType::Float,
            default: ParamValue::Float(0.2),
            range: Some((0.01, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("noise_per_frame"),
            label: "Noise Per Frame",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    // A wide gather over `depth`, but the output stays on the input's pixel grid.
    depth_rule: Inherit,
    composition_notes: "Output is a grayscale AO map (R=G=B=occlusion, A=1): wire it into a node.mix (mode=Multiply, amount=1.0) with the scene colour as `a`. `depth` is render_scene's raw [0,1] `depth` output. `radius` is in world units, where occlusion has faded to about half (it ends by 1.46x `radius`): scale it to the scene, not the canvas. The noise is per pixel; follow it with node.bilateral_blur (H then V). For a still image leave `noise_per_frame` off. With node.temporal_accumulate between this atom and the blur, turn `noise_per_frame` on: the pattern then moves every frame and the history averages it away.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "SSAO (GTAO)", category: Atom },
    summary: "Computes contact shadows from scene depth and a physical camera lens using a horizon-angle integral (GTAO): darkens crevices and touching surfaces the way ambient light naturally would.",
    category: Mask,
    role: Map,
    aliases: ["gtao", "ssao", "ground truth ambient occlusion", "ambient occlusion", "contact shadow", "screen space ambient occlusion", "ao"],
    boundary_reason: CrossFrameState,
    wgsl_body: include_str!("shaders/ssao_gtao_body.wgsl"),
    input_access: [GatherTexel],
    // `depth` feeds the normal and every tap's range test; fp16 would
    // stair-step both at grazing angles.
    precision_critical: ["depth"],
    derived_uniforms: ["fov_y", "near", "far", "noise_frame:u32"],
    wgsl_includes: [DEPTH_COMMON, VIEW_NORMAL_COMMON],
    extra_fields: {
        // New frames since the last `clear_state`, modulo `NOISE_CYCLE`.
        noise_step: u32 = 0,
        // The frame the step was taken for: the host's render pass and frame count.
        noise_seen: Option<(u64, i64)> = None,
    },
}

/// The Camera-derived fields `run()` passes as derived uniforms.
pub(super) fn derive_view_scalars(cam: &Camera) -> [f32; 3] {
    let fov_y = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        CameraMode::Orthographic { .. } => std::f32::consts::FRAC_PI_3,
    };
    [fov_y, cam.near, cam.far]
}

impl SsaoGtao {
    /// The offset into the noise sequence for `frame` (the host's render pass
    /// and frame count): one step on from the frame before, the same step
    /// again for the same frame.
    fn noise_frame_for(&mut self, frame: (u64, i64)) -> u32 {
        if self.noise_seen.is_some_and(|seen| seen != frame) {
            self.noise_step = (self.noise_step + 1) % NOISE_CYCLE;
        }
        self.noise_seen = Some(frame);
        self.noise_step * NOISE_FRAME_STRIDE
    }
}

impl Primitive for SsaoGtao {
    fn clear_state(&mut self) {
        self.noise_step = 0;
        self.noise_seen = None;
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read_f32 = |ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32| -> f32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => *f,
                _ => default,
            }
        };
        let radius = read_f32(ctx, "radius", 0.5);
        let intensity = read_f32(ctx, "intensity", 1.0);
        let slices = read_f32(ctx, "slices", 2.0);
        let steps = read_f32(ctx, "steps", 4.0);
        let projection = match ctx.params.get("projection") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 0,
        };
        let relief = read_f32(ctx, "relief", 0.2);
        let noise_per_frame = u32::from(matches!(
            ctx.params.get("noise_per_frame"),
            Some(ParamValue::Bool(true))
        ));

        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        let [fov_y, near, far] = derive_view_scalars(&cam);
        let pass = ctx.gpu.as_ref().map_or(0, |gpu| gpu.render_pass);
        let noise_frame = self.noise_frame_for((pass, ctx.time.frame_count));

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
            let wgsl =
                manifold_node_engine::freeze::codegen::standalone_for_boundary_spec::<Self>()
                    .expect("node.ssao_gtao standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                manifold_node_engine::freeze::codegen::ENTRY,
                "node.ssao_gtao",
            )
        });

        let uniforms = SsaoGtaoUniforms {
            radius,
            intensity,
            slices,
            steps,
            projection,
            relief,
            noise_per_frame,
            fov_y,
            near,
            far,
            noise_frame,
            _pad0: 0.0,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[depth_tex],
            None,
            out_tex,
            "node.ssao_gtao",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_view_scalars_reads_perspective_fov_near_far() {
        let mut cam = Camera::default_perspective();
        cam.near = 0.1;
        cam.far = 500.0;
        let [fov_y, near, far] = derive_view_scalars(&cam);
        let CameraMode::Perspective {
            fov_y: expected_fov,
        } = cam.mode
        else {
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

    /// The noise moves on each new render pass whatever the frame time does
    /// (a stopped transport still averages) and on each new host frame
    /// count; the same frame rendered again draws the same pattern. It
    /// cycles, and `clear_state` restarts it, so two exports from a cleared
    /// state draw the same sequence.
    #[test]
    fn noise_moves_once_per_new_frame_and_restarts_on_clear() {
        let mut node = SsaoGtao::new();
        let ticks = |node: &mut SsaoGtao| -> Vec<u32> {
            (0..=u64::from(NOISE_CYCLE))
                .map(|pass| node.noise_frame_for((pass + 1, 0)))
                .collect()
        };
        let first = ticks(&mut node);
        assert_eq!(first[0], 0);
        assert_eq!(first[1], NOISE_FRAME_STRIDE);
        assert!(
            first.windows(2).all(|w| w[0] != w[1]),
            "each content tick moves the pattern"
        );
        assert_eq!(first[NOISE_CYCLE as usize], 0, "the pattern cycles");

        let held = node.noise_frame_for((7, 3));
        assert!(
            (0..5).all(|_| node.noise_frame_for((7, 3)) == held),
            "the same frame rendered again keeps its pattern"
        );
        assert_ne!(
            node.noise_frame_for((7, 4)),
            held,
            "a new host frame count moves it"
        );

        Primitive::clear_state(&mut node);
        assert_eq!(
            first,
            ticks(&mut node),
            "a cleared node repeats the sequence"
        );
    }
}

/// Plain-Rust twin of `shaders/ssao_gtao_body.wgsl`, written separately so
/// the GPU parity test below is a real cross-check.
#[cfg(test)]
pub(crate) mod cpu_reference {
    use manifold_node_engine::scene::camera::linearize_depth;

    const GTAO_HALF_PI: f32 = std::f32::consts::FRAC_PI_2;
    const FALLOFF_RANGE: f32 = 0.615;
    const MIN_TAP_PX: f32 = 1.3;
    const RADIUS_MULTIPLIER: f32 = 1.457;
    const HILBERT_WIDTH: u32 = 64;

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

    fn hilbert_index(mut x: u32, mut y: u32) -> u32 {
        let mut index = 0;
        let mut level = HILBERT_WIDTH / 2;
        while level > 0 {
            let rx = u32::from(x & level > 0);
            let ry = u32::from(y & level > 0);
            index += level * level * ((3 * rx) ^ ry);
            if ry == 0 {
                if rx == 1 {
                    x = HILBERT_WIDTH - 1 - x;
                    y = HILBERT_WIDTH - 1 - y;
                }
                std::mem::swap(&mut x, &mut y);
            }
            level /= 2;
        }
        index
    }

    /// (slice, step) noise in [0,1) for a pixel and frame offset.
    fn noise(cx: i32, cy: i32, frame_offset: u32) -> (f32, f32) {
        let index = (hilbert_index(cx as u32 % HILBERT_WIDTH, cy as u32 % HILBERT_WIDTH)
            + frame_offset) as f32;
        let fract = |v: f32| v - v.floor();
        (
            fract(0.5 + index * 0.754_877_7),
            fract(0.5 + index * 0.569_840_3),
        )
    }

    fn gtao_round(x: f32) -> f32 {
        if x >= 0.0 {
            (x + 0.5).floor()
        } else {
            -(-x + 0.5).floor()
        }
    }

    fn height_pos(depth: &DepthBuffer<'_>, cx: i32, cy: i32, aspect: f32, relief: f32) -> [f32; 3] {
        let raw = depth.load(cx, cy);
        let ccx = cx.clamp(0, depth.w - 1);
        let ccy = cy.clamp(0, depth.h - 1);
        let u = (ccx as f32 + 0.5) / depth.w as f32;
        let v = (ccy as f32 + 0.5) / depth.h as f32;
        [u * aspect, 1.0 - v, (1.0 - raw) * relief]
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
        [
            ndc_x * tan_half_fov * aspect * view_z,
            ndc_y * tan_half_fov * view_z,
            view_z,
        ]
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn length(a: [f32; 3]) -> f32 {
        (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
    }
    fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
        [a[0] * s, a[1] * s, a[2] * s]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn normalize(a: [f32; 3]) -> [f32; 3] {
        let l = length(a);
        if l > 1e-8 {
            scale(a, 1.0 / l)
        } else {
            [0.0, 0.0, -1.0]
        }
    }

    /// `view_normal_common.wgsl`'s edge weights (left, right, top, bottom).
    fn view_normal_edges(center: f32, left: f32, right: f32, top: f32, bottom: f32) -> [f32; 4] {
        let e = [left - center, right - center, top - center, bottom - center];
        let slope_lr = (e[1] - e[0]) * 0.5;
        let slope_tb = (e[3] - e[2]) * 0.5;
        let adjusted = [
            e[0] + slope_lr,
            e[1] - slope_lr,
            e[2] + slope_tb,
            e[3] - slope_tb,
        ];
        let scale = (center * 0.011).max(1e-6);
        std::array::from_fn(|i| (1.25 - e[i].abs().min(adjusted[i].abs()) / scale).clamp(0.0, 1.0))
    }

    /// `view_normal_common.wgsl`'s `view_normal`.
    fn view_normal(
        edges: [f32; 4],
        c: [f32; 3],
        l: [f32; 3],
        r: [f32; 3],
        t: [f32; 3],
        b: [f32; 3],
    ) -> [f32; 3] {
        let dir = |p: [f32; 3]| {
            let d = sub(p, c);
            let len = length(d);
            if len > 1e-8 {
                scale(d, 1.0 / len)
            } else {
                [0.0; 3]
            }
        };
        let (l, r, t, b) = (dir(l), dir(r), dir(t), dir(b));
        let accepted = [
            edges[0] * edges[2],
            edges[2] * edges[1],
            edges[1] * edges[3],
            edges[3] * edges[0],
        ]
        .map(|a| (a + 0.01).clamp(0.0, 1.0));
        let corners = [cross(l, t), cross(t, r), cross(r, b), cross(b, l)];
        let mut n = [0.0f32; 3];
        for (a, corner) in accepted.iter().zip(corners) {
            n = [
                n[0] + a * corner[0],
                n[1] + a * corner[1],
                n[2] + a * corner[2],
            ];
        }
        normalize(n)
    }

    fn integrate_arc(h: f32, n: f32) -> f32 {
        0.25 * (-(2.0 * h - n).cos() + n.cos() + 2.0 * h * n.sin())
    }

    /// `integrate_arc(n-pi/2) + integrate_arc(n+pi/2)`: an unoccluded
    /// slice's arc. Not constant in `n` (BUG-y5w7), hence the division by it.
    fn full_ref(n: f32) -> f32 {
        n.cos() + n * n.sin()
    }

    /// One side's horizon after a tap: faded toward the open horizon by the
    /// falloff weight, then the running maximum.
    fn horizon(hcos: f32, d: [f32; 3], view_vec: [f32; 3], radius: f32, low_cos: f32) -> f32 {
        let len = length(d);
        if len <= 1e-5 {
            return hcos;
        }
        let weight = ((radius - len) / (FALLOFF_RANGE * radius)).clamp(0.0, 1.0);
        let tap = dot(scale(d, 1.0 / len), view_vec);
        hcos.max(low_cos + (tap - low_cos) * weight)
    }

    /// One texel's AO output, `[0,1]`. `noise_frame` is the frame's offset
    /// into the noise sequence; 0 is the still pattern.
    #[allow(clippy::too_many_arguments)]
    pub fn gtao_texel(
        depth: &DepthBuffer<'_>,
        cx: i32,
        cy: i32,
        radius: f32,
        intensity: f32,
        slices: usize,
        steps: usize,
        heightfield: bool,
        relief: f32,
        fov_y: f32,
        near: f32,
        far: f32,
        noise_frame: u32,
    ) -> f32 {
        let radius = radius * RADIUS_MULTIPLIER;
        let slices = slices.max(1);
        let steps = steps.max(1);
        if intensity <= 0.0 {
            return 1.0;
        }
        if !heightfield {
            let raw = depth.load(cx, cy);
            if raw <= 1e-6 {
                return 1.0;
            }
        }
        let tan_half_fov = (fov_y * 0.5).tan();
        let aspect = depth.w as f32 / depth.h as f32;
        let pos = |cx: i32, cy: i32| -> [f32; 3] {
            if heightfield {
                height_pos(depth, cx, cy, aspect, relief)
            } else {
                view_pos(depth, cx, cy, tan_half_fov, aspect, near, far)
            }
        };

        let p_c = pos(cx, cy);
        let (l, r, t, b) = (
            pos(cx - 1, cy),
            pos(cx + 1, cy),
            pos(cx, cy - 1),
            pos(cx, cy + 1),
        );
        let edges = if heightfield {
            [1.0; 4]
        } else {
            view_normal_edges(p_c[2], l[2], r[2], t[2], b[2])
        };
        let normal = view_normal(edges, p_c, l, r, t, b);

        let (view_vec, radius_px) = if heightfield {
            ([0.0, 0.0, -1.0], radius * depth.h as f32)
        } else {
            let center_vz = p_c[2].max(1e-4);
            (
                scale(normalize(p_c), -1.0),
                (radius / (tan_half_fov * center_vz)) * (depth.h as f32 * 0.5),
            )
        };

        let (slice_noise, step_offset) = noise(cx, cy, noise_frame);
        let rot = slice_noise * std::f32::consts::TAU;

        let mut visibility_sum = 0.0f32;
        let mut baseline_sum = 0.0f32;
        for s in 0..slices {
            let phi = rot * 0.5 + (s as f32) * (2.0 * GTAO_HALF_PI / slices as f32);
            let dir2 = [phi.cos(), phi.sin()];
            // The view-space direction of a +dir2 pixel step (Y flips, BUG-y5w7).
            let dir3 = [dir2[0], -dir2[1], 0.0];

            let axis = cross(dir3, view_vec);
            let axis_len = length(axis);
            let mut proj_len = 0.0f32;
            let mut n_signed = 0.0f32;
            if axis_len > 1e-6 {
                let axis_n = scale(axis, 1.0 / axis_len);
                let proj_normal = sub(normal, scale(axis_n, dot(normal, axis_n)));
                proj_len = length(proj_normal);
                if proj_len > 1e-6 {
                    let cos_n = (dot(proj_normal, view_vec) / proj_len).clamp(-1.0, 1.0);
                    let ortho = sub(dir3, scale(view_vec, dot(dir3, view_vec)));
                    let sign_n = if dot(ortho, proj_normal) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    n_signed = sign_n * cos_n.acos();
                }
            }

            let low_plus = (n_signed + GTAO_HALF_PI).cos();
            let low_minus = (n_signed - GTAO_HALF_PI).cos();
            let mut hcos_minus = -1.0f32;
            let mut hcos_plus = -1.0f32;
            for j in 0..steps {
                let r_j = radius_px * (j as f32 + step_offset) / steps as f32 + MIN_TAP_PX;
                let plus = pos(
                    cx + gtao_round(dir2[0] * r_j) as i32,
                    cy + gtao_round(dir2[1] * r_j) as i32,
                );
                hcos_plus = horizon(hcos_plus, sub(plus, p_c), view_vec, radius, low_plus);
                let minus = pos(
                    cx + gtao_round(-dir2[0] * r_j) as i32,
                    cy + gtao_round(-dir2[1] * r_j) as i32,
                );
                hcos_minus = horizon(hcos_minus, sub(minus, p_c), view_vec, radius, low_minus);
            }

            let mut h1 = -hcos_minus.clamp(-1.0, 1.0).acos();
            let mut h2 = hcos_plus.clamp(-1.0, 1.0).acos();
            h1 = n_signed + (h1 - n_signed).max(-GTAO_HALF_PI);
            h2 = n_signed + (h2 - n_signed).min(GTAO_HALF_PI);

            visibility_sum +=
                proj_len * (integrate_arc(h1, n_signed) + integrate_arc(h2, n_signed));
            baseline_sum += proj_len * full_ref(n_signed);
        }

        let visibility = (visibility_sum / baseline_sum.max(1e-4)).clamp(0.0, 1.0);
        (1.0 - intensity * (1.0 - visibility)).clamp(0.0, 1.0)
    }
}

/// A flat plane must read fully visible (out.r ~ 1) at any field of view.
/// `margin` skips the edge ring, where the finite-difference normal has no
/// real neighbour.
#[cfg(test)]
mod analytic_sanity {
    use super::cpu_reference::{DepthBuffer, gtao_texel};

    #[allow(clippy::too_many_arguments)]
    fn assert_flat_plane_visible(
        fov_y: f32,
        near: f32,
        far: f32,
        radius: f32,
        raw: f32,
        margin: i32,
        bound: f32,
    ) {
        let (w, h) = (16i32, 16i32);
        let raw_buf = vec![raw; (w * h) as usize];
        let depth = DepthBuffer {
            w,
            h,
            raw: &raw_buf,
        };
        for noise_frame in [0, super::NOISE_FRAME_STRIDE * 7] {
            for cy in margin..(h - margin) {
                for cx in margin..(w - margin) {
                    let vis = gtao_texel(
                        &depth,
                        cx,
                        cy,
                        radius,
                        1.0,
                        2,
                        4,
                        false,
                        0.2,
                        fov_y,
                        near,
                        far,
                        noise_frame,
                    );
                    assert!(
                        (vis - 1.0).abs() < bound,
                        "fov_y={fov_y} texel ({cx},{cy}): flat plane must give ~full visibility (out.r~1), got {vis}"
                    );
                }
            }
        }
    }

    #[test]
    fn gtao_flat_plane_full_visibility() {
        assert_flat_plane_visible(0.02, 0.1, 1000.0, 0.05, 0.5, 0, 1e-3);
    }

    /// The import camera's field of view; `raw` puts the plane ~2.5 units out.
    #[test]
    fn gtao_flat_plane_full_visibility_at_import_fov() {
        assert_flat_plane_visible(0.9, 0.05, 200.0, 0.5, 0.0198, 2, 0.05);
    }

    #[test]
    fn gtao_flat_plane_full_visibility_at_90_degree_fov() {
        assert_flat_plane_visible(
            std::f32::consts::FRAC_PI_2,
            0.1,
            1000.0,
            0.5,
            0.0399,
            2,
            0.05,
        );
    }

    /// A radius five times the plane's distance: every tap clamps to the
    /// buffer edge. It must stay under the old ~0.3 worst-pixel loss.
    #[test]
    fn gtao_flat_plane_boundary_clamped_regime_does_not_regress() {
        assert_flat_plane_visible(0.9, 0.05, 200.0, 0.5, 0.5, 0, 0.2);
    }
}

/// A near block in front of a far wall: the wall beside the step is occluded,
/// flat regions away from it are not.
#[cfg(test)]
mod corner_sanity {
    use super::cpu_reference::{DepthBuffer, gtao_texel};

    #[test]
    fn corner_shows_real_occlusion_flat_region_does_not() {
        let (w, h) = (64i32, 64i32);
        let step_x = 32i32;
        let (raw_near, raw_far) = (0.022f32, 0.0198f32);
        let mut raw = vec![raw_far; (w * h) as usize];
        for y in 0..h {
            for x in step_x..w {
                raw[(y * w + x) as usize] = raw_near;
            }
        }
        let depth = DepthBuffer { w, h, raw: &raw };
        let (fov_y, near, far, radius) = (0.9f32, 0.05f32, 200.0f32, 0.5);
        // Averaged down the column, as the blur after this atom does: one
        // pixel's two slices may run parallel to the step.
        let ao = |x: i32| {
            (8..h - 8)
                .map(|y| {
                    gtao_texel(
                        &depth, x, y, radius, 1.0, 2, 4, false, 0.2, fov_y, near, far, 0,
                    )
                })
                .sum::<f32>()
                / (h - 16) as f32
        };

        let (at_crease, flat_far, flat_near) = (ao(step_x - 2), ao(4), ao(w - 4));
        assert!(
            flat_far > 0.9,
            "flat region on the far surface must read near-unoccluded, got {flat_far}"
        );
        assert!(
            flat_near > 0.9,
            "flat region on the near block must read near-unoccluded, got {flat_near}"
        );
        assert!(
            at_crease < flat_far - 0.15,
            "the wall beside the step must show occlusion: crease {at_crease}, flat {flat_far}"
        );
    }
}

#[cfg(test)]
mod heightfield_sanity {
    use super::cpu_reference::{DepthBuffer, gtao_texel};

    /// The orthographic frame has no perspective tilt: a flat height map is
    /// exactly visible.
    #[test]
    fn heightfield_flat_plane_full_visibility_exact() {
        let (w, h) = (16i32, 16i32);
        let raw = vec![0.5f32; (w * h) as usize];
        let depth = DepthBuffer { w, h, raw: &raw };
        for cy in 0..h {
            for cx in 0..w {
                let vis = gtao_texel(
                    &depth, cx, cy, 0.02, 1.0, 4, 8, true, 0.25, 0.9, 1.0, 3.0, 0,
                );
                assert!(
                    (vis - 1.0).abs() < 1e-4,
                    "texel ({cx},{cy}): flat heightfield must be fully visible, got {vis}"
                );
            }
        }
    }
}

/// Peter's case: a 1 m cube on the ground (resting, and falling 10 cm
/// above it), seen from 10 m. The ground in front of the cube is averaged
/// across the cube's width into one AO profile against distance from the
/// contact. A hard cut at the radius, or tap distances shared by every
/// pixel, bend that profile sharply: the lines parallel to the edge.
#[cfg(test)]
mod contact_lines {
    use super::cpu_reference::{DepthBuffer, gtao_texel};

    const W: i32 = 1280;
    const H: i32 = 720;
    const FOV_Y: f32 = 0.9;
    const NEAR: f32 = 0.05;
    const FAR: f32 = 200.0;
    const RADIUS: f32 = 0.5;
    /// Where the fade-out starts.
    const FADE_START: f32 = 0.85;
    /// The largest upward bend (second difference between neighbouring
    /// rows) a smooth fade-out shows. The hard cut at the radius measured
    /// 0.025 here; the falloff measures 0.004 on one still frame.
    const LINE_BEND: f32 = 0.01;

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn normalize(a: [f32; 3]) -> [f32; 3] {
        let l = dot(a, a).sqrt();
        [a[0] / l, a[1] / l, a[2] / l]
    }

    /// Nearest hit along a ray: the ground (y = 0) or the cube `hover` above it.
    fn hit(o: [f32; 3], d: [f32; 3], hover: f32) -> Option<(f32, bool)> {
        let mut best = (d[1] < 0.0).then(|| (-o[1] / d[1], false));
        let (lo, hi) = ([-0.5f32, hover, -0.5], [0.5f32, 1.0 + hover, 0.5]);
        let (mut t0, mut t1) = (0.0f32, f32::INFINITY);
        for a in 0..3 {
            let inv = 1.0 / d[a];
            let (ta, tb) = ((lo[a] - o[a]) * inv, (hi[a] - o[a]) * inv);
            t0 = t0.max(ta.min(tb));
            t1 = t1.min(ta.max(tb));
        }
        if t0 <= t1 && best.is_none_or(|(t, _)| t0 < t) {
            best = Some((t0, true));
        }
        best
    }

    /// Raw reversed-Z depth of the scene, and per pixel whether it is cube.
    fn render(hover: f32) -> (Vec<f32>, Vec<bool>) {
        let (raw, hits) = render_from([0.0, 3.5, 10.0], hover);
        (raw, hits.iter().map(|h| h.is_some()).collect())
    }

    /// Raw reversed-Z depth seen from `eye`, and per pixel the cube point hit.
    fn render_from(eye: [f32; 3], hover: f32) -> (Vec<f32>, Vec<Option<[f32; 3]>>) {
        let fwd = normalize(sub([0.0, 0.3, 0.5], eye));
        let right = normalize(cross(fwd, [0.0, 1.0, 0.0]));
        let up = cross(right, fwd);
        let (tan_half, aspect) = ((FOV_Y * 0.5).tan(), W as f32 / H as f32);
        let mut raw = vec![0.0f32; (W * H) as usize];
        let mut cube = vec![None; (W * H) as usize];
        for y in 0..H {
            for x in 0..W {
                let nx = ((x as f32 + 0.5) / W as f32 * 2.0 - 1.0) * tan_half * aspect;
                let ny = (1.0 - (y as f32 + 0.5) / H as f32 * 2.0) * tan_half;
                let d = normalize([
                    fwd[0] + right[0] * nx + up[0] * ny,
                    fwd[1] + right[1] * nx + up[1] * ny,
                    fwd[2] + right[2] * nx + up[2] * ny,
                ]);
                if let Some((t, on_cube)) = hit(eye, d, hover) {
                    let z = t * dot(d, fwd);
                    let i = (y * W + x) as usize;
                    raw[i] = NEAR * (FAR - z) / (z * (FAR - NEAR));
                    cube[i] =
                        on_cube.then(|| [eye[0] + d[0] * t, eye[1] + d[1] * t, eye[2] + d[2] * t]);
                }
            }
        }
        (raw, cube)
    }

    /// Mean AO of the ground rows below the cube, across the middle of its
    /// width, over the given noise frames.
    fn profile(hover: f32, noise_frames: &[u32]) -> Vec<f32> {
        let (raw, cube) = render(hover);
        let depth = DepthBuffer {
            w: W,
            h: H,
            raw: &raw,
        };
        let centre = W / 2;
        let bottom = (0..H)
            .rev()
            .find(|&y| cube[(y * W + centre) as usize])
            .expect("cube in view");
        let cols: Vec<i32> = (0..W)
            .filter(|&x| cube[(bottom * W + x) as usize])
            .collect();
        let fifth = cols.len() / 5;
        let cols = &cols[fifth..cols.len() - fifth];
        (bottom + 1..(bottom + 120).min(H - 2))
            .map(|y| {
                let mut sum = 0.0;
                for &x in cols {
                    for &frame in noise_frames {
                        sum += gtao_texel(
                            &depth, x, y, RADIUS, 1.0, 2, 4, false, 0.2, FOV_Y, NEAR, FAR, frame,
                        );
                    }
                }
                sum / (cols.len() * noise_frames.len()) as f32
            })
            .collect()
    }

    /// The largest upward bend of the fade-out: from the first row past
    /// `FADE_START` on, the profile may only ease into white. A line is the
    /// profile suddenly speeding up there. The rows nearer the cube are the
    /// contact's own shape (and the gap under a falling cube), not lines.
    fn largest_bend(p: &[f32]) -> f32 {
        let start = p.iter().position(|&v| v > FADE_START).unwrap_or(p.len());
        p[start..]
            .windows(3)
            .map(|w| w[2] - 2.0 * w[1] + w[0])
            .fold(0.0, f32::max)
    }

    #[test]
    fn no_lines_at_a_cube_contact_seen_from_ten_metres() {
        let moving: Vec<u32> = (0..16).map(|k| k * super::NOISE_FRAME_STRIDE).collect();
        for hover in [0.0, 0.1] {
            let still = profile(hover, &[0]);
            let averaged = profile(hover, &moving);
            assert!(
                averaged[0] < 0.8,
                "hover {hover}: the ground by the cube must stay occluded, got {}",
                averaged[0]
            );
            assert!(
                largest_bend(&still) < LINE_BEND,
                "hover {hover}: one still frame shows a line, bend {}",
                largest_bend(&still)
            );
            assert!(
                largest_bend(&averaged) < LINE_BEND,
                "hover {hover}: averaged frames show a line, bend {}",
                largest_bend(&averaged)
            );
        }
    }

    /// The cube's right side seen nearly edge-on, with the ground far behind
    /// it. The background pixel just outside the silhouette must read like
    /// the background beyond it: a central-difference normal there straddles
    /// the silhouette, leans toward the cube and draws a dark line down it.
    #[test]
    fn no_dark_line_beside_an_edge_on_face() {
        let (raw, hits) = render_from([3.0, 3.5, 10.0], 0.0);
        let depth = DepthBuffer {
            w: W,
            h: H,
            raw: &raw,
        };
        let ao = |x: i32, y: i32| {
            gtao_texel(
                &depth, x, y, RADIUS, 1.0, 2, 4, false, 0.2, FOV_Y, NEAR, FAR, 0,
            )
        };
        let mid_side = |x: i32, y: i32| {
            hits[(y * W + x) as usize]
                .is_some_and(|p| (p[0] - 0.5).abs() < 1e-3 && (0.3..0.7).contains(&p[1]))
        };
        let (mut outside, mut beyond) = (0.0, 0.0);
        let mut rows = 0;
        for y in 1..H - 1 {
            let Some(x) =
                (1..W - 4).find(|&x| mid_side(x, y) && hits[(y * W + x + 1) as usize].is_none())
            else {
                continue;
            };
            outside += ao(x + 1, y);
            beyond += ao(x + 3, y);
            rows += 1;
        }
        assert!(
            rows > 10,
            "the side must be in view on more than {rows} rows"
        );
        let (outside, beyond) = (outside / rows as f32, beyond / rows as f32);
        assert!(
            outside > beyond - 0.05,
            "a dark line beside the silhouette: {outside} against {beyond} beyond it"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The generated standalone kernel (the one that ships) against
    //! `cpu_reference::gtao_texel`. Integer tap offsets mean a last-digit
    //! difference between GPU and CPU trig can land a tap on a neighbouring
    //! texel, so a minority of texels differ by a discrete jump; the bounds
    //! below carry that.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };

    use super::cpu_reference::{DepthBuffer, gtao_texel};
    use super::{SsaoGtao, SsaoGtaoUniforms};
    use manifold_node_engine::gpu::render_target::RenderTarget;

    fn upload_depth(device: &GpuDevice, w: u32, h: u32, raw: &[f32]) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for (i, &r) in raw.iter().enumerate() {
            px[i * 4] = f16::from_f32(r);
            px[i * 4 + 1] = f16::from_f32(r);
            px[i * 4 + 2] = f16::from_f32(r);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "gtao-depth",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Depth that varies in x and y, so the normal is never degenerate.
    fn depth_ramp_2d(w: u32, h: u32) -> Vec<f32> {
        let mut raw = vec![0.0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                let fy = y as f32 / (h.saturating_sub(1).max(1)) as f32;
                raw[(y * w + x) as usize] = 0.8 - 0.6 * (0.5 * fx + 0.5 * fy);
            }
        }
        raw
    }

    fn uniforms(
        radius: f32,
        fov_y: f32,
        near: f32,
        far: f32,
        noise_frame: u32,
    ) -> SsaoGtaoUniforms {
        SsaoGtaoUniforms {
            radius,
            intensity: 1.0,
            slices: 2.0,
            steps: 4.0,
            projection: 0,
            relief: 0.2,
            noise_per_frame: u32::from(noise_frame != 0),
            fov_y,
            near,
            far,
            noise_frame,
            _pad0: 0.0,
        }
    }

    fn run_generated(
        device: &GpuDevice,
        depth: &GpuTexture,
        w: u32,
        h: u32,
        uniforms: &SsaoGtaoUniforms,
    ) -> Vec<f32> {
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_boundary_spec::<SsaoGtao>()
                .expect("node.ssao_gtao standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "gtao-generated",
        );
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "gtao-out");
        let mut enc = device.create_encoder("gtao-dispatch");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: depth,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &out.texture,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "gtao-dispatch",
        );
        enc.commit_and_wait_completed();
        let bytes_per_row = w * 8;
        let readback = device.create_buffer(
            u64::from(h * bytes_per_row),
            manifold_gpu::BufferUse::Upload,
        );
        let mut enc = device.create_encoder("gtao-readback");
        enc.copy_texture_to_buffer(&out.texture, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| f16::from_bits(halves[i * 4]).to_f32())
            .collect()
    }

    #[test]
    fn gtao_matches_cpu_reference() {
        let device = manifold_gpu::testkit::test_device();
        let (w, h) = (24u32, 16u32);
        let raw = depth_ramp_2d(w, h);
        let depth_tex = upload_depth(&device, w, h, &raw);
        let (radius, fov_y, near, far) = (0.02f32, std::f32::consts::FRAC_PI_2, 0.1, 100.0);
        let depth_buf = DepthBuffer {
            w: w as i32,
            h: h as i32,
            raw: &raw,
        };
        for noise_frame in [0, 5 * super::NOISE_FRAME_STRIDE] {
            let gpu_out = run_generated(
                &device,
                &depth_tex,
                w,
                h,
                &uniforms(radius, fov_y, near, far, noise_frame),
            );
            let mut mismatches = 0usize;
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let cpu = gtao_texel(
                        &depth_buf,
                        x,
                        y,
                        radius,
                        1.0,
                        2,
                        4,
                        false,
                        0.2,
                        fov_y,
                        near,
                        far,
                        noise_frame,
                    );
                    let gpu = gpu_out[(y as u32 * w + x as u32) as usize];
                    let diff = (cpu - gpu).abs();
                    assert!(
                        diff < 0.25,
                        "frame {noise_frame} texel ({x},{y}): cpu={cpu} gpu={gpu} diff={diff}"
                    );
                    if diff >= 1e-3 {
                        mismatches += 1;
                    }
                }
            }
            assert!(
                mismatches * 3 <= (w * h) as usize,
                "frame {noise_frame}: {mismatches} texels differ by more than 1e-3"
            );
        }
    }

    #[test]
    fn generated_gtao_flat_plane_gives_full_visibility() {
        let device = manifold_gpu::testkit::test_device();
        let (w, h) = (16u32, 16u32);
        let depth_tex = upload_depth(&device, w, h, &vec![0.5f32; (w * h) as usize]);
        let out = run_generated(
            &device,
            &depth_tex,
            w,
            h,
            &uniforms(0.05, 0.02, 0.1, 1000.0, 0),
        );
        for (i, v) in out.iter().enumerate() {
            assert!(
                (v - 1.0).abs() < 1e-3,
                "texel {i}: flat plane must give ~full visibility, got {v}"
            );
        }
    }
}
