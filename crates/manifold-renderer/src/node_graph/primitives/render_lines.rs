//! `node.draw_lines` — render-pass primitive that draws an
//! `Array<CurvePoint>` as anti-aliased capsule line segments with
//! 4× MSAA and additive blending.
//!
//! Input positions are in **pre-aspect curve space** centred at
//! the origin — the natural output shape of
//! [`crate::node_graph::primitives::PackCurveXy`] (the curve-pipeline
//! pack atom) and the other curve-generator primitives that emit
//! `Array<CurvePoint>`. Aspect correction + centre
//! offset is applied in the vertex shader so the same curve sample
//! draws cleanly on any aspect ratio without per-target CPU work.
//!
//! Animation + dots are first-class: when `animate=true`, a
//! scrolling window of `window`-fraction of the edges is drawn
//! with a fade ramp at the trailing edge; `speed` scales how fast
//! the window scrolls. When `show_verts=true`, a dot is drawn at
//! each vertex (or each *visible* vertex in animated mode),
//! matching the legacy [`crate::generators::line_pipeline::LineGeneratorHelper`]
//! pipeline. `beat_flash_amount` reproduces the per-beat luminance
//! pulse from the legacy `generator_lines.wgsl` shader for
//! bit-perfect parity with the pre-graph line generators.
//!
//! Per-instance `EdgeInstance` carries the two endpoint indices
//! `a, b` and an `alpha` (encoded as f32 bits). When `a == b` the
//! capsule degenerates to a dot using `dot_thickness` rather than
//! `edge_thickness`. Dot instances are appended after edge
//! instances, and the `num_edges` uniform tells the shader the
//! boundary.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuLoadAction};

use crate::generators::mesh_common::{EdgePair, CurvePoint};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MSAA_SAMPLE_COUNT: u32 = 4;

/// Default vertex-dot radius in screen-fraction units. Matches the
/// legacy `generator_math::DEFAULT_DOT_RADIUS` (0.005) so the
/// graph-rendered Lissajous emits identical dot sizes to the
/// pre-graph generator at `vert_size = 1.0`.
const DEFAULT_DOT_RADIUS: f32 = 0.005;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineRenderUniforms {
    rt_width: f32,
    rt_height: f32,
    edge_half_thickness: f32,
    dot_half_thickness: f32,
    color: [f32; 4],
    num_edges: u32,
    beat: f32,
    beat_flash_amount: f32,
    /// 1 when the optional `widths` input is wired (per-point taper);
    /// 0 when the dummy buffer is bound and every point renders at 1.0.
    has_widths: u32,
}

/// Per-instance edge data uploaded to the GPU. `a` and `b` are
/// vertex indices into the `points` buffer; when `a == b` the
/// capsule degenerates to a dot. `alpha_bits` carries the
/// per-instance fade as the bit-pattern of an `f32`.
///
/// `pub` (rather than file-private) because the `primitive!` macro
/// expands `extra_fields` into `pub` struct fields — Rust then
/// requires the field's type to be at least as public as the
/// field. Keeping it inside the `render_lines` module via re-export
/// would be cleaner, but the macro doesn't have a hook for that.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EdgeInstance {
    a: u32,
    b: u32,
    alpha_bits: u32,
    _pad: u32,
}

crate::primitive! {
    name: RenderLines,
    type_id: "node.draw_lines",
    purpose: "Draw an Array<CurvePoint> as anti-aliased capsule line segments with 4x MSAA and additive blending. Input points are in pre-aspect curve space centred at the origin; this node applies aspect correction + centre offset on its way to the framebuffer. Wire the optional `widths` Array<f32> (parallel to `points`) and each segment renders as a tapered capsule interpolating its endpoints' thickness multipliers — trunk-to-tip width decay for bolts, strokes, and plant stems. `animate=true` enables a scrolling-window reveal that matches the legacy line-generator helper; `show_verts=true` draws a dot at each (visible) vertex. `beat_flash_amount` pulses luminance per beat to match the legacy generator_lines.wgsl flash. Pair with node.combine_xy (for parametric curve graphs built from generate_range + array_math chains) or other curve-source primitives upstream.",
    inputs: {
        points: Array(CurvePoint) required,
        // Optional explicit edge topology. When wired, each non-sentinel
        // EdgePair `(a, b)` in the buffer becomes one rendered line
        // segment from `points[a]` to `points[b]`. When absent, the
        // primitive falls back to the implicit sequential / closed-loop
        // topology driven by the `closed_loop` param. `animate` /
        // `speed` / `window` work in both paths — when `edges` is
        // wired the scrolling window walks edges in producer
        // declaration order (matches legacy Wireframe / Tesseract /
        // Duocylinder, all of which left projected_z at zero so the
        // legacy depth-sort collapsed to identity).
        edges: Array(EdgePair) optional,
        // Optional per-point thickness multipliers, parallel to
        // `points` (index i scales the capsule radius at points[i]).
        // Wired: each segment renders as a tapered capsule
        // interpolating its endpoints' widths — the lightning-bolt
        // trunk-to-tip taper. Unwired: every point is 1.0 and the
        // geometry is bit-identical to the pre-taper renderer.
        widths: Array(f32) optional,
    },
    outputs: {
        color: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("edge_thickness"),
            label: "Edge Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.002),
            range: Some((0.0001, 0.1)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("closed_loop"),
            label: "Closed Loop",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("show_verts"),
            label: "Show Vertices",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("vert_size"),
            label: "Vertex Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("animate"),
            label: "Animate",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("window"),
            label: "Window",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.001, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("beat_flash_amount"),
            label: "Beat Flash",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_r"),
            label: "Color R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Color G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Color B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_a"),
            label: "Color A",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "edge_thickness is half-thickness in screen-fraction units (≈0.002 = ~1px at 1080p). `vert_size = 1.0` matches the legacy `generator_math::DEFAULT_DOT_RADIUS` (0.005 screen-fraction). `beat_flash_amount = 0` disables the per-beat luminance pulse. `animate`-mode draws a window of `window`-fraction of the edges with a smooth fade at the trailing edge — speed in proportion to (segment_count / 100) * dt * 60, matching the legacy LineGeneratorHelper. Color values above 1.0 produce HDR bloom-friendly output (additive blending).",
    examples: [],
    picker: { label: "Draw Lines", category: Atom },
    summary: "Draws a set of smooth anti-aliased lines onto the image from a list of points. Used for wireframes, paths, and curve overlays.",
    category: Generate,
    role: Filter,
    aliases: ["draw lines", "render lines", "strokes", "wireframe"],
    boundary_reason: Blocked,
    extra_fields: {
        render_pipeline: Option<manifold_gpu::GpuRenderPipeline> = None,
        msaa_texture: Option<manifold_gpu::GpuTexture> = None,
        msaa_width: u32 = 0,
        msaa_height: u32 = 0,
        instances_buf: Option<manifold_gpu::GpuBuffer> = None,
        instances_capacity: u64 = 0,
        cpu_instances: Vec<EdgeInstance> = Vec::new(),
        vert_visible: Vec<bool> = Vec::new(),
        anim_progress: f32 = 0.0,
        // Filtered edges (sentinels + out-of-range stripped) for the
        // topology-driven animation path. Reused across frames; sized
        // to the producer's edge count on each `build_instances_from_edges`
        // call to keep the animation walk's modular arithmetic stable.
        valid_edges: Vec<EdgePair> = Vec::new(),
        // 4-byte placeholder bound at the widths slot when the
        // `widths` port is unwired — Metal requires a valid buffer
        // at every declared binding; `has_widths = 0` means the
        // shader never reads it.
        dummy_widths: Option<manifold_gpu::GpuBuffer> = None,
    },
}

impl RenderLines {
    fn ensure_msaa_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.msaa_width == width
            && self.msaa_height == height
            && self.msaa_texture.is_some()
        {
            return;
        }
        self.msaa_texture = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLE_COUNT,
            "node.draw_lines MSAA",
        ));
        self.msaa_width = width;
        self.msaa_height = height;
    }

    /// Grow the GPU instance buffer to hold `count` `EdgeInstance`s.
    /// Allocates a fresh shared buffer; the old one drops. Doubling
    /// avoids resizing every frame when the active edge count
    /// fluctuates by a few segments around the animation window.
    fn ensure_instances_buf(&mut self, device: &manifold_gpu::GpuDevice, count: u64) {
        let needed = (count.max(64)).next_power_of_two();
        if needed <= self.instances_capacity && self.instances_buf.is_some() {
            return;
        }
        let bytes = needed * std::mem::size_of::<EdgeInstance>() as u64;
        self.instances_buf = Some(device.create_buffer_shared(bytes));
        self.instances_capacity = needed;
    }

    /// Build per-instance edge + dot data into `cpu_instances`.
    /// `num_edges` is the count of edge instances (animated subset
    /// or full strip); dots come after, one per *visible* vertex.
    /// Returns `(num_edges_emitted, num_dots_emitted)`.
    #[allow(clippy::too_many_arguments)]
    fn build_instances(
        &mut self,
        num_points: u32,
        closed_loop: bool,
        animate: bool,
        speed: f32,
        window: f32,
        show_verts: bool,
        dt: f32,
    ) -> (u32, u32) {
        self.cpu_instances.clear();
        let segments_total: u32 = if closed_loop {
            num_points
        } else {
            num_points.saturating_sub(1)
        };
        if segments_total == 0 {
            return (0, 0);
        }

        // Per-vertex visibility tracker for dot filtering in
        // animated mode. Sized once per frame to current N.
        self.vert_visible.clear();
        self.vert_visible.resize(num_points as usize, !animate);

        if animate {
            // Match the legacy LineGeneratorHelper exactly:
            //   anim_progress += speed * (N/100) * dt * 60
            // wraps at `segments_total`. `window_edges` is the
            // ceil-rounded count of edges to reveal, with one extra
            // fading-in at the leading position.
            self.anim_progress += speed * (segments_total as f32 / 100.0) * dt * 60.0;
            let total = segments_total as f32;
            if self.anim_progress >= total {
                self.anim_progress -= total;
            }
            if self.anim_progress < 0.0 {
                self.anim_progress += total;
            }
            let window_edges =
                ((segments_total as f32 * window).ceil() as usize).max(1);
            let window_start = self.anim_progress.floor() as usize % segments_total as usize;
            let fract = self.anim_progress.fract();

            for offset in 0..=window_edges {
                let sort_pos = (window_start + offset) % segments_total as usize;
                let smooth_offset = offset as f32 - fract;
                let fade = (1.0 - smooth_offset / window_edges as f32).clamp(0.0, 1.0);
                if fade <= 0.0 {
                    continue;
                }
                // For a closed loop, segment i connects i → (i+1)%N.
                // For an open strip, segment i connects i → i+1
                // (and there's no segment N-1 → 0).
                let a = sort_pos as u32;
                let b = if closed_loop {
                    ((sort_pos + 1) % num_points as usize) as u32
                } else {
                    (sort_pos + 1) as u32
                };
                self.cpu_instances.push(EdgeInstance {
                    a,
                    b,
                    alpha_bits: fade.to_bits(),
                    _pad: 0,
                });
                self.vert_visible[a as usize] = true;
                self.vert_visible[b as usize] = true;
            }
        } else {
            for i in 0..segments_total {
                let a = i;
                let b = if closed_loop {
                    (i + 1) % num_points
                } else {
                    i + 1
                };
                self.cpu_instances.push(EdgeInstance {
                    a,
                    b,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
            }
        }

        let num_edges = self.cpu_instances.len() as u32;
        let mut num_dots = 0u32;
        if show_verts {
            for i in 0..num_points as usize {
                if animate && !self.vert_visible[i] {
                    continue;
                }
                self.cpu_instances.push(EdgeInstance {
                    a: i as u32,
                    b: i as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
                num_dots += 1;
            }
        }
        (num_edges, num_dots)
    }

    /// Build per-instance edge data from an explicit `Array<EdgePair>`
    /// buffer (the topology-driven path used by wireframe-shape
    /// generators). When `animate=false`, every non-sentinel pair
    /// `(a, b)` becomes one rendered segment with `alpha = 1`. When
    /// `animate=true`, the same scrolling window + fade from the
    /// implicit-strip path walks edges in producer declaration order
    /// — matches legacy Wireframe / Tesseract / Duocylinder, all
    /// of which left `projected_z` at zero so the depth-sort in the
    /// legacy pipeline collapsed to declaration order anyway. See
    /// `crates/manifold-renderer/src/generators/line_pipeline.rs:277`
    /// for the legacy reference.
    ///
    /// `show_verts=true` appends one degenerate (a==b) dot instance
    /// per vertex that participates in at least one currently-visible
    /// edge — matches the legacy behaviour where unconnected (or
    /// animated-out) vertices don't show dots.
    #[allow(clippy::too_many_arguments)]
    fn build_instances_from_edges(
        &mut self,
        edges: &[EdgePair],
        num_points: u32,
        animate: bool,
        speed: f32,
        window: f32,
        show_verts: bool,
        dt: f32,
    ) -> (u32, u32) {
        self.cpu_instances.clear();
        self.vert_visible.clear();
        // Explicit-edge topology: a vertex is visible iff at least one
        // currently-drawn edge touches it. Even in the non-animated
        // path this stays `false` (legacy Wireframe behaviour:
        // isolated vertices don't get dots).
        self.vert_visible.resize(num_points as usize, false);

        // Filter sentinels + out-of-range indices once so the
        // animation walk can index modulo a contiguous list rather
        // than re-scanning every frame.
        self.valid_edges.clear();
        for edge in edges {
            if edge.a == u32::MAX {
                continue;
            }
            if edge.a >= num_points || edge.b >= num_points {
                continue;
            }
            self.valid_edges.push(*edge);
        }

        let segments_total = self.valid_edges.len() as u32;
        if segments_total == 0 {
            return (0, 0);
        }

        if animate {
            // Same phase advance as `build_instances`: legacy formula
            //   anim_progress += speed * (N/100) * dt * 60
            // wrapping at `segments_total`. `window_edges` is the
            // ceil-rounded count of edges to reveal, with one extra
            // fading-in at the leading position.
            self.anim_progress += speed * (segments_total as f32 / 100.0) * dt * 60.0;
            let total = segments_total as f32;
            if self.anim_progress >= total {
                self.anim_progress -= total;
            }
            if self.anim_progress < 0.0 {
                self.anim_progress += total;
            }
            let window_edges = ((segments_total as f32 * window).ceil() as usize).max(1);
            let window_start = self.anim_progress.floor() as usize % segments_total as usize;
            let fract = self.anim_progress.fract();

            for offset in 0..=window_edges {
                let sort_pos = (window_start + offset) % segments_total as usize;
                let smooth_offset = offset as f32 - fract;
                let fade = (1.0 - smooth_offset / window_edges as f32).clamp(0.0, 1.0);
                if fade <= 0.0 {
                    continue;
                }
                let edge = self.valid_edges[sort_pos];
                self.cpu_instances.push(EdgeInstance {
                    a: edge.a,
                    b: edge.b,
                    alpha_bits: fade.to_bits(),
                    _pad: 0,
                });
                self.vert_visible[edge.a as usize] = true;
                self.vert_visible[edge.b as usize] = true;
            }
        } else {
            for edge in &self.valid_edges {
                self.cpu_instances.push(EdgeInstance {
                    a: edge.a,
                    b: edge.b,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
                self.vert_visible[edge.a as usize] = true;
                self.vert_visible[edge.b as usize] = true;
            }
        }

        let num_edges = self.cpu_instances.len() as u32;
        let mut num_dots = 0u32;
        if show_verts {
            for i in 0..num_points as usize {
                if !self.vert_visible[i] {
                    continue;
                }
                self.cpu_instances.push(EdgeInstance {
                    a: i as u32,
                    b: i as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
                num_dots += 1;
            }
        }
        (num_edges, num_dots)
    }
}

impl Primitive for RenderLines {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // ── Param read (port-shadows-param not yet used; all
        // params are static knobs on this primitive). ──
        let edge_thickness = match ctx.params.get("edge_thickness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.002,
        };
        let closed_loop = matches!(ctx.params.get("closed_loop"), Some(ParamValue::Bool(true)));
        let show_verts = matches!(ctx.params.get("show_verts"), Some(ParamValue::Bool(true)));
        let vert_size = match ctx.params.get("vert_size") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let animate = matches!(ctx.params.get("animate"), Some(ParamValue::Bool(true)));
        let speed = match ctx.params.get("speed") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let window = match ctx.params.get("window") {
            Some(ParamValue::Float(f)) => f.clamp(0.001, 1.0),
            _ => 0.1,
        };
        let beat_flash_amount = match ctx.params.get("beat_flash_amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.4,
        };
        let color_r = match ctx.params.get("color_r") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_g = match ctx.params.get("color_g") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_b = match ctx.params.get("color_b") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let color_a = match ctx.params.get("color_a") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let beat = ctx.time.beats.0 as f32;
        let dt = ctx.time.delta.0 as f32;

        // ── Resolve input/output slots ──
        // Both buffers are pre-bound by the chain build; an absent
        // input here means the upstream Array producer didn't get
        // pre-allocated (or didn't dispatch). Warn instead of
        // silently dropping the frame so the host gets a single
        // line of diagnostic instead of a black render.
        let Some(points) = ctx.inputs.array("points") else {
            log::warn!(
                "node.draw_lines: no GpuBuffer bound to input port `points` — \
                 nothing to draw this frame. The producing Array<CurvePoint> node \
                 was either skipped or its output buffer wasn't pre-allocated.",
            );
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("color") else {
            log::warn!(
                "node.draw_lines: no GpuTexture bound to output port `color` — \
                 the host did not pre-bind a render target.",
            );
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }
        let item_size = std::mem::size_of::<CurvePoint>() as u64;
        let num_points = (points.size / item_size) as u32;
        if num_points < 2 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        // ── Build per-frame instance buffer (CPU side) ──
        // Two paths:
        //   - `edges` input wired  → topology-driven (e.g. Wireframe).
        //   - `edges` input absent → sequential / closed-loop (e.g. Lissajous).
        let edges_input = ctx.inputs.array("edges");
        let (num_edges, num_dots) = if let Some(edges_buf) = edges_input {
            // Read the shared edges buffer CPU-side. The buffer is
            // host-mapped (per the Array<T> pre-allocation policy);
            // the producer's `run()` ran earlier in the executor's
            // sequential walk so the data is already visible to us.
            let edge_count = (edges_buf.size as usize) / std::mem::size_of::<EdgePair>();
            let edges_slice: &[EdgePair] = match edges_buf.mapped_ptr() {
                Some(ptr) => unsafe {
                    let byte_slice =
                        std::slice::from_raw_parts(ptr as *const u8, edges_buf.size as usize);
                    bytemuck::cast_slice(byte_slice)
                },
                None => {
                    log::warn!(
                        "node.draw_lines: `edges` input buffer is not host-mapped — \
                         expected a shared-memory Array<EdgePair> from the upstream producer.",
                    );
                    &[]
                }
            };
            let _ = edge_count; // sanity: equals edges_slice.len() by construction
            self.build_instances_from_edges(
                edges_slice,
                num_points,
                animate,
                speed,
                window,
                show_verts,
                dt,
            )
        } else {
            self.build_instances(
                num_points,
                closed_loop,
                animate,
                speed,
                window,
                show_verts,
                dt,
            )
        };
        let total_instances = num_edges + num_dots;
        if total_instances == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            return;
        }

        // ── Thickness in pixel-space half-thickness ──
        let edge_half_px = edge_thickness * (height as f32);
        let dot_half_px = DEFAULT_DOT_RADIUS * (height as f32) * vert_size;

        let widths_input = ctx.inputs.array("widths");

        let uniforms = LineRenderUniforms {
            rt_width: width as f32,
            rt_height: height as f32,
            edge_half_thickness: edge_half_px,
            dot_half_thickness: dot_half_px,
            color: [color_r, color_g, color_b, color_a],
            num_edges,
            beat,
            beat_flash_amount,
            has_widths: u32::from(widths_input.is_some()),
        };

        // ── GPU setup: pipeline + MSAA + instance buffer ──
        let gpu = ctx.gpu_encoder();
        if self.render_pipeline.is_none() {
            let blend = manifold_gpu::GpuBlendState {
                src_factor: manifold_gpu::GpuBlendFactor::One,
                dst_factor: manifold_gpu::GpuBlendFactor::One,
                operation: manifold_gpu::GpuBlendOp::Max,
                src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                alpha_operation: manifold_gpu::GpuBlendOp::Max,
            };
            self.render_pipeline = Some(gpu.device.create_render_pipeline_msaa(
                include_str!("shaders/render_lines.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(blend),
                MSAA_SAMPLE_COUNT,
                "node.draw_lines",
            ));
        }
        self.ensure_msaa_texture(gpu.device, width, height);
        self.ensure_instances_buf(gpu.device, total_instances as u64);
        if widths_input.is_none() && self.dummy_widths.is_none() {
            self.dummy_widths = Some(gpu.device.create_buffer_shared(4));
        }

        // Upload the CPU instance list into the shared GPU buffer.
        let inst_bytes: &[u8] = bytemuck::cast_slice(&self.cpu_instances);
        let inst_buf = self.instances_buf.as_ref().expect("just ensured");
        // Safety: the buffer is shared-memory, sized to fit. The
        // caller's borrow of `self` guarantees no concurrent
        // writers, and we copy into it before the draw is dispatched.
        unsafe {
            inst_buf.write(0, inst_bytes);
        }

        let pipeline = self.render_pipeline.as_ref().expect("just inserted");
        let msaa_tex = self.msaa_texture.as_ref().expect("just inserted");
        // Metal requires a valid buffer at every declared binding, so
        // the unwired case binds the 4-byte dummy; `has_widths = 0`
        // guarantees the shader never reads it.
        let widths_buf = widths_input
            .unwrap_or_else(|| self.dummy_widths.as_ref().expect("just ensured"));

        gpu.native_enc.draw_instanced_msaa(
            pipeline,
            msaa_tex,
            target,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: points,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: inst_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: widths_buf,
                    offset: 0,
                },
            ],
            6,
            total_instances,
            GpuLoadAction::Clear,
            "node.draw_lines",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_linepoint_input_optional_edges_and_texture_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let points_layout = ArrayType::of_known::<CurvePoint>();
        let edges_layout = ArrayType::of_known::<EdgePair>();
        let widths_layout = ArrayType::of_known::<f32>();

        assert_eq!(RenderLines::TYPE_ID, "node.draw_lines");
        assert_eq!(RenderLines::INPUTS.len(), 3);
        assert_eq!(RenderLines::INPUTS[0].name, "points");
        assert!(RenderLines::INPUTS[0].required);
        assert_eq!(RenderLines::INPUTS[0].ty, PortType::Array(points_layout));
        assert_eq!(RenderLines::INPUTS[1].name, "edges");
        assert!(!RenderLines::INPUTS[1].required);
        assert_eq!(RenderLines::INPUTS[1].ty, PortType::Array(edges_layout));
        assert_eq!(RenderLines::INPUTS[2].name, "widths");
        assert!(!RenderLines::INPUTS[2].required);
        assert_eq!(RenderLines::INPUTS[2].ty, PortType::Array(widths_layout));
        assert_eq!(RenderLines::OUTPUTS.len(), 1);
        assert_eq!(RenderLines::OUTPUTS[0].name, "color");
        assert_eq!(RenderLines::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn params_cover_thickness_animation_dots_color_and_flash() {
        let names: Vec<&str> = RenderLines::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec![
                "edge_thickness",
                "closed_loop",
                "show_verts",
                "vert_size",
                "animate",
                "speed",
                "window",
                "beat_flash_amount",
                "color_r",
                "color_g",
                "color_b",
                "color_a",
            ]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RenderLines::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.draw_lines");
    }

    /// `build_instances` non-animated, closed-loop: N segments
    /// connecting i → (i+1)%N, all alpha=1, no dots.
    #[test]
    fn non_animated_closed_loop_emits_full_strip() {
        let mut prim = RenderLines::new();
        let (edges, dots) = prim.build_instances(
            5, /*closed_loop*/ true, /*animate*/ false, 1.0, 0.1,
            /*show_verts*/ false, 1.0 / 60.0,
        );
        assert_eq!(edges, 5);
        assert_eq!(dots, 0);
        for i in 0..5 {
            assert_eq!(prim.cpu_instances[i].a, i as u32);
            assert_eq!(prim.cpu_instances[i].b, ((i + 1) % 5) as u32);
            assert_eq!(f32::from_bits(prim.cpu_instances[i].alpha_bits), 1.0);
        }
    }

    /// Open-strip (closed_loop=false) drops the wrap segment:
    /// N-1 segments, none from N-1 back to 0.
    #[test]
    fn non_animated_open_strip_drops_wrap_segment() {
        let mut prim = RenderLines::new();
        let (edges, _) = prim.build_instances(
            4, /*closed_loop*/ false, /*animate*/ false, 1.0, 0.1, false, 1.0 / 60.0,
        );
        assert_eq!(edges, 3);
        assert_eq!(prim.cpu_instances[2].a, 2);
        assert_eq!(prim.cpu_instances[2].b, 3);
    }

    /// `show_verts=true` appends one degenerate (a==b) instance per
    /// vertex when animation is off.
    #[test]
    fn show_verts_appends_one_dot_per_vertex() {
        let mut prim = RenderLines::new();
        let (edges, dots) = prim.build_instances(
            4, true, false, 1.0, 0.1, /*show_verts*/ true, 1.0 / 60.0,
        );
        assert_eq!(edges, 4);
        assert_eq!(dots, 4);
        for i in 0..4 {
            let dot = prim.cpu_instances[edges as usize + i];
            assert_eq!(dot.a, i as u32);
            assert_eq!(dot.b, i as u32);
        }
    }

    /// Animated mode emits a window of edges sized by `window`, with
    /// fading alpha along the trailing edge. With N=10, window=0.5
    /// we expect ceil(10*0.5)+1 = 6 edges.
    #[test]
    fn animated_mode_emits_window_with_fade() {
        let mut prim = RenderLines::new();
        prim.anim_progress = 0.0;
        let (edges, _) = prim.build_instances(
            10, true, /*animate*/ true, 1.0, /*window*/ 0.5, false, 1.0 / 60.0,
        );
        // window_edges = ceil(10 * 0.5) = 5, plus one fading-in
        // edge at the leading position → 6 instances total. The
        // trailing edge has the smallest fade and may be clipped to
        // zero, so we accept 5 or 6.
        assert!(
            (5..=6).contains(&edges),
            "expected 5 or 6 animated edges with window=0.5, got {edges}",
        );
        // Alphas should monotonically decrease along the window.
        let alphas: Vec<f32> = prim
            .cpu_instances
            .iter()
            .map(|e| f32::from_bits(e.alpha_bits))
            .collect();
        for pair in alphas.windows(2) {
            assert!(pair[0] >= pair[1], "alphas must monotonically fade: {alphas:?}");
        }
    }

    /// Explicit-edges path: each `EdgePair` becomes one segment,
    /// sentinels are skipped, out-of-range indices are skipped.
    #[test]
    fn explicit_edges_path_builds_one_segment_per_valid_pair() {
        let mut prim = RenderLines::new();
        let edges = [
            EdgePair { a: 0, b: 1 },
            EdgePair { a: 1, b: 2 },
            EdgePair { a: 2, b: 0 },
            EdgePair::SENTINEL,
            EdgePair::SENTINEL,
        ];
        let (n_edges, n_dots) = prim.build_instances_from_edges(
            &edges, 3, /*animate*/ false, 1.0, 0.1, /*show_verts*/ false, 1.0 / 60.0,
        );
        assert_eq!(n_edges, 3, "three valid pairs, two sentinels");
        assert_eq!(n_dots, 0);
        // All three EdgeInstances must reference valid vertex indices.
        for i in 0..3 {
            let inst = prim.cpu_instances[i];
            assert!(inst.a < 3 && inst.b < 3);
            assert_eq!(f32::from_bits(inst.alpha_bits), 1.0);
        }
    }

    /// Out-of-range indices (vertex count smaller than the edge buffer
    /// references) are skipped silently — the alternative is a black
    /// frame from a vertex-shader bad read.
    #[test]
    fn explicit_edges_path_skips_out_of_range_indices() {
        let mut prim = RenderLines::new();
        let edges = [
            EdgePair { a: 0, b: 1 },
            EdgePair { a: 5, b: 6 }, // out of range for num_points=3
        ];
        let (n_edges, _) = prim.build_instances_from_edges(
            &edges, 3, /*animate*/ false, 1.0, 0.1, /*show_verts*/ false, 1.0 / 60.0,
        );
        assert_eq!(n_edges, 1, "only the in-range pair survives");
    }

    /// `show_verts` in explicit-edges mode draws a dot at every vertex
    /// that participates in at least one edge — matches the legacy
    /// Wireframe behaviour where unconnected vertices stay hidden.
    #[test]
    fn explicit_edges_show_verts_dots_only_connected_vertices() {
        let mut prim = RenderLines::new();
        // 4 vertices, but only 0–1–2 form a connected component;
        // vertex 3 is isolated.
        let edges = [
            EdgePair { a: 0, b: 1 },
            EdgePair { a: 1, b: 2 },
        ];
        let (n_edges, n_dots) = prim.build_instances_from_edges(
            &edges, 4, /*animate*/ false, 1.0, 0.1, /*show_verts*/ true, 1.0 / 60.0,
        );
        assert_eq!(n_edges, 2);
        assert_eq!(n_dots, 3, "three connected vertices, vertex 3 stays hidden");
        // Each dot's a==b.
        for i in 0..n_dots as usize {
            let dot = prim.cpu_instances[n_edges as usize + i];
            assert_eq!(dot.a, dot.b);
        }
    }

    /// Animated + show_verts: dots only appear at vertices touched
    /// by visible edges, not at every vertex in the curve.
    #[test]
    fn animated_show_verts_filters_dots_by_visible_edges() {
        let mut prim = RenderLines::new();
        prim.anim_progress = 0.0;
        let (edges, dots) = prim.build_instances(
            20, true, true, 1.0, /*window*/ 0.1, /*show_verts*/ true, 1.0 / 60.0,
        );
        assert!(dots > 0, "must emit at least one dot");
        assert!(
            (dots as usize) < 20,
            "animated mode must NOT emit all 20 dots — got {dots} (full {edges} edges + dots)",
        );
        // Every dot must be at a vertex marked visible.
        for i in 0..dots as usize {
            let inst = prim.cpu_instances[edges as usize + i];
            assert_eq!(inst.a, inst.b, "dot must be degenerate (a==b)");
            assert!(
                prim.vert_visible[inst.a as usize],
                "dot at vertex {} that isn't marked visible",
                inst.a
            );
        }
    }

    /// Regression for the "Animate / Speed / Window are exposed on
    /// Wireframe's `render_lines` node but do nothing" bug. The
    /// pre-fix `build_instances_from_edges` ignored animate entirely
    /// because the topology-driven path was thought to need depth-
    /// sorted reveal — but the legacy line_pipeline left `projected_z`
    /// at zero on every wireframe generator (Wireframe, Tesseract,
    /// Duocylinder), so the legacy "depth sort" collapsed to identity
    /// and the visible animation was input-order reveal all along.
    ///
    /// Asserts: with 12 edges, window=0.1 + animate=true emits strictly
    /// fewer instances than the full 12, AND each emitted instance
    /// carries a non-default alpha (fade > 0, fade < 1 for at least
    /// the trailing edge of the window) — both signals that the
    /// animation walk actually ran.
    #[test]
    fn edges_wired_animate_reveals_a_window_not_the_whole_topology() {
        let mut prim = RenderLines::new();
        prim.anim_progress = 0.0;
        // 12 edges forming an icosahedron-shaped topology (just enough
        // to make a 0.1 window pick out a small subset).
        let edges: Vec<EdgePair> = (0..12)
            .map(|i| EdgePair { a: i, b: (i + 1) % 6 })
            .collect();
        let (n_edges, n_dots) = prim.build_instances_from_edges(
            &edges, 6, /*animate*/ true, /*speed*/ 1.0, /*window*/ 0.1,
            /*show_verts*/ false, 1.0 / 60.0,
        );
        assert!(
            n_edges < 12,
            "animated edges-wired path must reveal a subset of 12 edges, got {n_edges}",
        );
        assert!(n_edges >= 1, "the window always reveals at least one edge");
        assert_eq!(n_dots, 0, "show_verts=false → no dots");

        // Every instance must reference a valid vertex.
        for i in 0..n_edges as usize {
            let inst = prim.cpu_instances[i];
            assert!(inst.a < 6 && inst.b < 6);
            let fade = f32::from_bits(inst.alpha_bits);
            assert!(
                fade > 0.0 && fade <= 1.0,
                "fade must be in (0, 1], got {fade} at instance {i}",
            );
        }
    }

    /// `anim_progress` must advance across frames in the edges-wired
    /// path, otherwise the window scrolls at zero — the visible
    /// symptom of forgetting to apply the `speed * (N/100) * dt * 60`
    /// phase update in the topology path.
    #[test]
    fn edges_wired_animate_advances_anim_progress_each_frame() {
        let mut prim = RenderLines::new();
        prim.anim_progress = 0.0;
        let edges: Vec<EdgePair> =
            (0..10).map(|i| EdgePair { a: i, b: (i + 1) % 5 }).collect();
        let dt = 1.0 / 60.0;

        let _ = prim.build_instances_from_edges(
            &edges, 5, true, /*speed*/ 2.0, 0.2, false, dt,
        );
        let progress_after_one_frame = prim.anim_progress;
        assert!(
            progress_after_one_frame > 0.0,
            "anim_progress must advance on the first animated frame, got {progress_after_one_frame}",
        );

        let _ = prim.build_instances_from_edges(
            &edges, 5, true, 2.0, 0.2, false, dt,
        );
        let progress_after_two_frames = prim.anim_progress;
        assert!(
            progress_after_two_frames > progress_after_one_frame,
            "anim_progress must keep advancing, got {progress_after_one_frame} then {progress_after_two_frames}",
        );
    }

    /// Without animation, the edges-wired path must emit every valid
    /// edge with alpha=1 (the static-wireframe baseline that legacy
    /// Wireframe defaults to). Guards against an over-eager refactor
    /// later that always runs the animation walk.
    #[test]
    fn edges_wired_static_path_emits_every_valid_edge_full_alpha() {
        let mut prim = RenderLines::new();
        let edges = [
            EdgePair { a: 0, b: 1 },
            EdgePair { a: 1, b: 2 },
            EdgePair { a: 2, b: 3 },
            EdgePair { a: 3, b: 0 },
        ];
        let (n_edges, _) = prim.build_instances_from_edges(
            &edges, 4, /*animate*/ false, 1.0, 0.1, false, 1.0 / 60.0,
        );
        assert_eq!(n_edges, 4);
        for i in 0..4 {
            assert_eq!(
                f32::from_bits(prim.cpu_instances[i].alpha_bits),
                1.0,
                "static path must use alpha=1 on every edge",
            );
        }
    }
}
