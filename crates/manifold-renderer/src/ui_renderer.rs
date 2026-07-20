use std::sync::Arc;

use manifold_gpu::{
    FrameFence, GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice,
    GpuEncoder, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};

#[cfg(target_os = "macos")]
use crate::native_text::NativeTextRenderer;

use manifold_ui::node::*;
use manifold_ui::text::TextMeasure;
use manifold_ui::transform2d::Affine2;
use manifold_ui::tree::{TraversalEvent, UITree};

/// Vertex for UI quad rendering.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UIVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    /// [rect_w, rect_h, corner_radius, border_width]
    rect_params: [f32; 4],
    border_color: [f32; 4],
    /// Gradient end colour. Ignored unless `grad.z` (enable) is set.
    color2: [f32; 4],
    /// [dir_x, dir_y, enable, _]. Linear gradient `color`→`color2` along the
    /// unit direction in uv space; `enable <= 0.5` → flat `color`.
    grad: [f32; 4],
}

const UI_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rect_params: vec4<f32>,
    @location(4) border_color: vec4<f32>,
    @location(5) color2: vec4<f32>,
    @location(6) grad: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) rect_params: vec4<f32>,
    @location(3) border_color: vec4<f32>,
    @location(4) color2: vec4<f32>,
    @location(5) grad: vec4<f32>,
};

struct Globals {
    viewport_size: vec2<f32>,
    offset: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Convert pixel coordinates to NDC with optional offset for panel-local rendering
    let ndc_x = ((in.position.x - globals.offset.x) / globals.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((in.position.y - globals.offset.y) / globals.viewport_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.rect_params = in.rect_params;
    out.border_color = in.border_color;
    out.color2 = in.color2;
    out.grad = in.grad;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let rect_w = in.rect_params.x;
    let rect_h = in.rect_params.y;
    let radius = in.rect_params.z;
    let border_w = in.rect_params.w;

    // Soft drop-shadow: a shadow quad is grown by `blur` on every side and
    // flagged by a negative border width (= -blur). The element's rounded rect
    // sits inset by `blur` at the quad centre; alpha falls from color.a at the
    // element edge to 0 over `blur` px outside it. Branch BEFORE the fast path
    // (a negative border_w would otherwise be swallowed by `border_w <= 0`).
    if border_w < 0.0 {
        let blur = -border_w;
        let pixel = in.uv * vec2<f32>(rect_w, rect_h);
        let center = vec2<f32>(rect_w, rect_h) * 0.5;
        let half_size = max(center - vec2<f32>(blur) - vec2<f32>(radius), vec2<f32>(0.0));
        let d = length(max(abs(pixel - center) - half_size, vec2<f32>(0.0))) - radius;
        let a = in.color.a * (1.0 - smoothstep(0.0, blur, max(d, 0.0)));
        if a <= 0.0 {
            discard;
        }
        return vec4<f32>(in.color.rgb, a);
    }

    // Body fill: flat `color`, or a linear gradient color→color2 along the unit
    // direction `grad.xy` (in uv space) when `grad.z` is set. The gradient
    // primitive (`draw_gradient_rect`); border + shadow paths never use it.
    var fill = in.color;
    if in.grad.z > 0.5 {
        let t = clamp(dot(in.uv - vec2<f32>(0.5), in.grad.xy) + 0.5, 0.0, 1.0);
        fill = mix(in.color, in.color2, t);
    }

    // If no corner radius, just output the body fill (fast path)
    if radius <= 0.0 && border_w <= 0.0 {
        return fill;
    }

    // SDF rounded rectangle
    let pixel = in.uv * vec2<f32>(rect_w, rect_h);
    let center = vec2<f32>(rect_w, rect_h) * 0.5;
    let half_size = center - vec2<f32>(radius);
    // Full rounded-box SDF. The `min(max(q.x,q.y),0)` interior term is what makes
    // `d` go NEGATIVE inside the box; without it (the old form) `d` is 0 across the
    // whole interior at radius 0, so a border floods the entire fill white. The
    // term is 0 outside/on the edge, so radius>0 rects are unchanged.
    let q = abs(pixel - center) - half_size;
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;

    // Antialiased edge. AA band = ONE physical pixel via the screen-space
    // derivative of d (fwidth), so edges stay crisp at any DPI. The old fixed
    // `aa = 1.0` was one *logical* px ≈ two physical px on retina → smeared.
    let aa = fwidth(d) + 1e-4;
    let shape_cov = 1.0 - smoothstep(-aa, aa, d);

    if shape_cov <= 0.0 {
        discard;
    }

    // Border ring: AA'd on BOTH the outer edge AND the inner (fill) edge — the
    // old code hard-stepped the inner edge (`inner_d > 0`), which aliased the
    // selection ring. Composite the ring over the fill with straight alpha so a
    // translucent hairline reads correctly over the chip body.
    if border_w > 0.0 {
        let fill_cov = 1.0 - smoothstep(-aa, aa, d + border_w);
        let border_a = in.border_color.a * clamp(shape_cov - fill_cov, 0.0, 1.0);
        let fa = fill.a * fill_cov;
        let out_a = border_a + fa * (1.0 - border_a);
        if out_a <= 0.0 {
            discard;
        }
        let out_rgb =
            (in.border_color.rgb * border_a + fill.rgb * fa * (1.0 - border_a)) / max(out_a, 1e-4);
        return vec4<f32>(out_rgb, out_a);
    }

    return vec4<f32>(fill.rgb, fill.a * shape_cov);
}
"#;

/// Z-depth for a draw command. Lower values paint first (further back).
///
/// The renderer paints in ascending depth, and **within a single depth it
/// draws rects+lines first, then text+icons**. So a surface's own background
/// never occludes its own text, but a *higher* depth occludes everything below
/// it — text included. Two surfaces that stack (a dropdown over a panel, the
/// graph mapping popover over nodes) must therefore sit at *distinct* depths,
/// or the lower one's text bleeds through the higher one's fill.
///
/// Named tiers below are spaced by 100 so new surfaces slot between them
/// without renumbering — adding a floating surface is "pick a depth," never a
/// renderer edit. A frame that only touches one depth issues the identical
/// draw sequence to the pre-depth renderer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Depth(pub i32);

impl Depth {
    /// Main UI: panels, timeline, graph wires.
    pub const BASE: Depth = Depth(0);
    /// Graph nodes — above the wires on `BASE`.
    pub const CONTENT: Depth = Depth(100);
    /// Floating top-level surfaces: panels, modals, perf HUD. The overlay
    /// driver offsets each open overlay upward from here by its stack index
    /// (`OVERLAY.above(i)`), so a later-opened overlay always paints over an
    /// earlier one — text included.
    pub const OVERLAY: Depth = Depth(200);
    /// Surfaces that open *on top of* an overlay or node: dropdown menus, the
    /// graph mapping popover.
    pub const POPOVER: Depth = Depth(300);
    /// Topmost transient surfaces: hover tooltips, drag ghosts, debug HUD.
    pub const TOOLTIP: Depth = Depth(400);

    /// This depth shifted up by `n` (e.g. `Depth::OVERLAY.above(i)` for the
    /// i-th stacked overlay).
    pub const fn above(self, n: i32) -> Depth {
        Depth(self.0 + n)
    }
}

/// Queued draw command.
struct RectCommand {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
    corner_radius: f32,
    border_width: f32,
    border_color: [f32; 4],
    /// Gradient end colour + `[dir_x, dir_y, enable, _]`. `grad` all-zero (the
    /// `NO_GRAD` default on every solid command) → flat `color`.
    color2: [f32; 4],
    grad: [f32; 4],
    /// Transform captured at draw time (current composed top of
    /// `UIRenderer::transform_stack`, identity when empty). Applied to the 4
    /// corner *positions* in `prepare()` — `uv`/`rect_params` are untouched, so
    /// the fragment shader's local-space SDF rotates/scales for free.
    transform: Affine2,
    /// Z-depth captured at draw time — see `LineCommand::depth`.
    depth: Depth,
    /// Clip captured at draw time — the tree traversal's `clip_stack` top for
    /// `draw_node` rects, `immediate_clip` for the immediate API. Bound at
    /// enqueue, never inferred at flush (BUG-060: flush-time inference let
    /// transform/depth boundaries strip the tree scissor from pending rects).
    clip: Option<Rect>,
}

/// `grad` value for a solid (non-gradient) rect: enable channel is 0.
const NO_GRAD: [f32; 4] = [0.0; 4];

/// Vertical optical-centring nudge, as a fraction of font size. Text placed so
/// its `font_size` box centres in a node sits slightly high — caps/x-height ink
/// centres above the box centre. Shifting the baseline down by this fraction
/// makes chip values + the name row read truly centred. 0.10 overshot low;
/// 0.05 lands on true centre against the
/// drawn icons (chevron / badge / hamburger), which carry no nudge.
const VERTICAL_OPTICAL_NUDGE: f32 = 0.05;

/// A solid-coloured line drawn as an oriented quad. Piggybacks on the
/// rect pipeline by emitting four rotated corner positions with
/// `rect_params = [0; 4]` so the fragment shader's fast path returns a
/// flat fill (no SDF, no border).
struct LineCommand {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    thickness: f32,
    color: [f32; 4],
    depth: Depth,
    /// Immediate clip captured at draw time (lines are only ever emitted via
    /// the immediate API; trees draw rects and text only).
    clip: Option<Rect>,
    /// Transform captured at draw time — see `RectCommand::transform`.
    transform: Affine2,
}

/// A batch of rects sharing the same scissor state.
/// Accumulated during tree traversal, converted to PreparedBatch during prepare.
struct ScissorBatch {
    /// Scissor rect in logical coordinates. None = no clip (full viewport).
    scissor: Option<Rect>,
    /// Index of the first rect_command in this batch.
    rect_start: usize,
    /// Number of rects in this batch.
    rect_count: usize,
    /// Z-depth the batch draws on.
    depth: Depth,
}

/// GPU-ready batch with physical pixel scissor coordinates.
struct PreparedBatch {
    /// Scissor rect in physical pixels. None = full viewport.
    scissor: Option<[u32; 4]>,
    /// Byte offset into the index buffer.
    index_offset: u64,
    /// Number of indices to draw.
    index_count: u32,
    /// Z-depth the batch draws on.
    depth: Depth,
}

/// A static-image draw (PRESET_LIBRARY_DESIGN P6, D7): browser cells filling
/// their body with a save-time-rendered thumbnail. Non-interactive — the
/// existing button node drawn in the same rect keeps handling clicks/hover;
/// this only queues the picture.  Accumulated during tree traversal from
/// [`UITree`] `Image` nodes (see `draw_node`), converted to a
/// [`PreparedImageDraw`] during `prepare()`. A SEPARATE textured pipeline
/// from the flat/gradient rect pipeline (one texture bound per quad — no
/// atlas), so each is its own draw call; the browser's cell count is bounded
/// (tens of presets), so this is cheap.
struct ImageCommand {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    corner_radius: f32,
    handle: TextureHandle,
    depth: Depth,
    /// Source UV sub-rect `[u0, v0, u1, v1]`. `[0, 0, 1, 1]` samples the whole
    /// texture (static browser thumbnails); a narrower rect samples one cell of
    /// an atlas (graph-node output previews via the immediate `draw_image_uv`).
    uv: [f32; 4],
    /// Clip captured at draw time: the tree-traversal `clip_stack` for
    /// `draw_node` images, or `immediate_clip` for the immediate `draw_image_uv`.
    clip: Option<Rect>,
}

/// GPU-ready image draw with physical-pixel scissor.
struct PreparedImageDraw {
    scissor: Option<[u32; 4]>,
    depth: Depth,
    handle: TextureHandle,
    vertex_offset: u64,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageVertex {
    position: [f32; 2],
    uv: [f32; 2],
    /// (w, h, corner_radius, _) in LOCAL (unrotated) rect space — same
    /// rounded-rect SDF convention `UI_SHADER`'s fragment stage uses for
    /// `rect_params`.
    rect_params: [f32; 4],
}

/// Textured rounded-rect: samples `t_image` and masks it to the rounded-rect
/// SDF (same `fwidth`-based one-physical-pixel AA as `UI_SHADER`), so a
/// thumbnail fills a browser cell with clean rounded corners instead of a
/// square image poking over them.
const IMAGE_SHADER: &str = r#"
struct Globals { viewport_size: vec2<f32>, offset: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_image: texture_2d<f32>;
@group(0) @binding(2) var s_image: sampler;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) rect_params: vec4<f32>,
};
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) rect_params: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let ndc_x = ((in.position.x - globals.offset.x) / globals.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((in.position.y - globals.offset.y) / globals.viewport_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.rect_params = in.rect_params;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let rect_w = in.rect_params.x;
    let rect_h = in.rect_params.y;
    let radius = in.rect_params.z;
    let color = textureSample(t_image, s_image, in.uv);
    if radius <= 0.0 {
        return color;
    }
    let pixel = in.uv * vec2<f32>(rect_w, rect_h);
    let center = vec2<f32>(rect_w, rect_h) * 0.5;
    let half_size = center - vec2<f32>(radius);
    let q = abs(pixel - center) - half_size;
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    let aa = fwidth(d) + 1e-4;
    let cov = 1.0 - smoothstep(-aa, aa, d);
    if cov <= 0.0 {
        discard;
    }
    return vec4<f32>(color.rgb, color.a * cov);
}
"#;

/// Max image quads a single `prepare()` call queues — a generous bound for a
/// browser cell grid (tens of presets visible at once at most).
const MAX_IMAGE_QUADS: usize = 128;
/// Ring size for the image vertex buffer — mirrors `ClipThumbGpu`'s
/// `VBUF_RING_SIZE`. Stall-frequency relief only; correctness against slot
/// reuse comes from `frame_fence` (see `guard_slot`), not this depth.
const IMAGE_VBUF_RING_SIZE: usize = 6;

/// Initial vertex/index buffer capacities (vertices / indices).
const INITIAL_VERTEX_CAPACITY: usize = 1024;
const INITIAL_INDEX_CAPACITY: usize = 1536;

/// Ring buffer slots for GPU buffers. Each prepare() call uses the next slot.
/// After RING_SIZE prepare() calls the ring wraps around. With ~10 prepare
/// calls per frame (panel cache + sub-regions + overlay), 64 slots keeps
/// wraparound infrequent under normal GPU load — but depth alone can't
/// guarantee a slot's prior use has retired under backlog, so `frame_fence`
/// gates each claim (see `guard_slot`) for correctness.
const BUF_RING_SIZE: usize = 64;

/// RAII guard for [`UIRenderer::lane_content_scissor`] (D7). Holds the
/// timeline's lane-content scissor open; `Drop` issues the matching
/// `pop_immediate_clip` so the clip cannot leak past this scope even on an
/// early return. `Deref`/`DerefMut` to `UIRenderer` so a call site draws
/// through the guard exactly like it would through `ui` directly.
pub struct LaneContentScissor<'a> {
    ui: &'a mut UIRenderer,
}

impl std::ops::Deref for LaneContentScissor<'_> {
    type Target = UIRenderer;
    fn deref(&self) -> &UIRenderer {
        self.ui
    }
}

impl std::ops::DerefMut for LaneContentScissor<'_> {
    fn deref_mut(&mut self) -> &mut UIRenderer {
        self.ui
    }
}

impl Drop for LaneContentScissor<'_> {
    fn drop(&mut self) {
        self.ui.pop_immediate_clip();
    }
}

/// Simple batched 2D UI renderer using native Metal via manifold-gpu.
pub struct UIRenderer {
    pipeline: GpuRenderPipeline,

    // Text rendering — CoreText renderer.
    #[cfg(target_os = "macos")]
    text_renderer: NativeTextRenderer,

    // Rect draw queue.
    rect_commands: Vec<RectCommand>,

    // Line draw queue. Drained alongside rect_commands during prepare.
    line_commands: Vec<LineCommand>,

    // Per-frame vertex/index scratch (CPU side).
    vertices: Vec<UIVertex>,
    indices: Vec<u32>,

    // Ring-buffered GPU buffers — prevents aliasing between prepare/commit
    // cycles within the same frame AND across frames in flight.
    vbuf_ring: Vec<Option<GpuBuffer>>,
    ibuf_ring: Vec<Option<GpuBuffer>>,
    ring_idx: usize,
    /// Frame each `vbuf_ring`/`ibuf_ring` slot (shared index) was last
    /// claimed at (0 = never claimed); checked/updated via `frame_fence`.
    ring_stamps: Vec<u64>,
    /// Which ring slot the current prepared data lives in.
    prepared_slot: usize,
    prepared_index_count: u32,
    /// [viewport_w, viewport_h, offset_x, offset_y] — passed as inline uniform.
    prepared_globals: [f32; 4],

    // Clip stack for render_tree — its top is captured onto each `RectCommand`
    // (and `ImageCommand`, text `clip_bounds`) at enqueue time in `draw_node`.
    clip_stack: Vec<Rect>,
    // Clip applied to immediate-mode draws (the caller's `draw_rect`/`draw_line`
    // queued before an overlay). The graph canvas sets this to its lane so its
    // nodes AND wires stay scissored under the side panels instead of bleeding
    // over them. `None` (the default, reset every `begin_frame`) keeps the legacy
    // full-viewport behaviour for every other caller.
    immediate_clip: Option<Rect>,
    // Stack backing the immediate clip — entries are pre-intersected with
    // their enclosing clip, so the top IS the effective clip.
    immediate_clip_stack: Vec<Rect>,
    // Depth stack for z-ordered drawing. Empty = Depth::BASE. Each draw
    // command captures `current_depth()` at enqueue time (`RectCommand::depth`,
    // `LineCommand::depth`, …), so pushing/popping never needs to flush
    // anything — commands on either side of the boundary already carry the
    // right depth.
    depth_stack: Vec<Depth>,
    // Transform stack for scale/rotate draws (`docs/UI_TRANSFORM_STACK_DESIGN.md`).
    // Empty = identity. Entries are pre-composed at push time (like
    // `immediate_clip_stack`), so the top IS the effective transform. Each
    // draw command captures `current_transform()` at enqueue time, so
    // pushing/popping never needs to flush anything.
    transform_stack: Vec<Affine2>,
    // Scissor batches for the current frame. Derived in `prepare()` by a
    // linear run-scan over `rect_commands`, grouping consecutive commands
    // that share the same `(clip, depth)` — NOT accumulated during tree
    // traversal (BUG-060: flush-time batching let transform/depth boundaries
    // stamp the wrong scissor on already-enqueued rects; binding clip/depth
    // per command at enqueue makes that class of bug unrepresentable).
    scissor_batches: Vec<ScissorBatch>,
    // BUG-060 trace: current panel slot (set by the cache manager per panel
    // render) so batch logs can name the pass. Plain usize store — free when
    // the trace is disarmed. Remove with BUG-060.
    pub bug060_pass: usize,
    // GPU-ready batches produced by prepare().
    prepared_batches: Vec<PreparedBatch>,
    // Distinct depths present this frame, ascending — the union of rect/line
    // batch depths and the text renderer's text/icon depths. `render_in_pass`
    // walks this so it never assumes a fixed set of layers.
    prepared_depths: Vec<Depth>,
    // Scratch for grouping line quads into batches during prepare():
    // (depth, clip at draw time, index buffer byte offset, index count).
    line_batch_scratch: Vec<(Depth, Option<Rect>, u64, u32)>,
    // Physical dimensions of the render target (for full-viewport scissor reset).
    prepared_physical_w: u32,
    prepared_physical_h: u32,

    // ── Static images (PRESET_LIBRARY_DESIGN P6, D7) ────────────────────
    // A separate small textured pipeline from the flat/gradient rect one —
    // one texture bound per quad, no atlas (the browser's cell count is
    // bounded — tens of presets — so per-quad draw calls are cheap).
    image_pipeline: GpuRenderPipeline,
    image_sampler: GpuSampler,
    image_index_buf: GpuBuffer,
    image_vbuf_ring: Vec<Option<GpuBuffer>>,
    image_ring_idx: usize,
    /// Frame each `image_vbuf_ring` slot was last claimed at (0 = never
    /// claimed); checked/updated via `frame_fence`.
    image_ring_stamps: Vec<u64>,
    // Per-frame queue, accumulated by `draw_node` from `UINodeType::Image`
    // tree nodes; converted to `prepared_image_draws` and cleared in
    // `prepare()` (mirrors `rect_commands`).
    image_commands: Vec<ImageCommand>,
    // GPU-ready draws produced by `prepare()`.
    prepared_image_draws: Vec<PreparedImageDraw>,
    /// Which `image_vbuf_ring` slot holds this frame's prepared vertex data.
    prepared_image_slot: usize,
    /// Registered textures, decoded+uploaded ONCE per distinct key by the app
    /// (`register_image`) — persists across frames and across browser opens
    /// (no eviction: a bounded, stable corpus of tens of presets). Never
    /// populated or consulted on a per-frame render path — only at the
    /// (rare) point a new key shows up.
    image_textures: ahash::AHashMap<TextureHandle, GpuTexture>,

    // ── GPU-completion fence (rect ring + image ring) ───────────────────
    /// Shared GPU-completion fence — `None` in the headless test harness,
    /// which never constructs one (unchanged stamp-0 behavior).
    frame_fence: Option<Arc<FrameFence>>,
    /// Rate limiter for `[frame-fence]` stall logging, shared by both rings.
    fence_wait_events: u64,
}

impl UIRenderer {
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let blend = GpuBlendState {
            src_factor: GpuBlendFactor::SrcAlpha,
            dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
            operation: GpuBlendOp::Add,
            src_alpha_factor: GpuBlendFactor::One,
            dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: GpuBlendOp::Add,
        };
        let layout = GpuVertexLayout {
            stride: std::mem::size_of::<UIVertex>() as u32, // 96 bytes
            attributes: vec![
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 2,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 3,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 4,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 64,
                    shader_location: 5,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 80,
                    shader_location: 6,
                },
            ],
        };
        let pipeline = device.create_render_pipeline_with_vertex_layout(
            UI_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &layout,
            "UI Pipeline",
        );

        #[cfg(target_os = "macos")]
        let text_renderer = NativeTextRenderer::new(device, format);

        let vbuf_ring = (0..BUF_RING_SIZE).map(|_| None).collect();
        let ibuf_ring = (0..BUF_RING_SIZE).map(|_| None).collect();

        // ── Static-image pipeline (PRESET_LIBRARY_DESIGN P6, D7) ──
        let image_layout = GpuVertexLayout {
            stride: std::mem::size_of::<ImageVertex>() as u32,
            attributes: vec![
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 0, shader_location: 0 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 8, shader_location: 1 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 16, shader_location: 2 },
            ],
        };
        let image_pipeline = device.create_render_pipeline_with_vertex_layout(
            IMAGE_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &image_layout,
            "UI Image Pipeline",
        );
        let image_sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            ..Default::default()
        });
        let image_index_data: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let image_index_buf = device.create_buffer_shared(24);
        unsafe {
            std::ptr::copy_nonoverlapping(
                image_index_data.as_ptr(),
                image_index_buf.mapped_ptr().unwrap().cast::<u32>(),
                6,
            );
        }
        let image_vbuf_size = (MAX_IMAGE_QUADS * 4 * std::mem::size_of::<ImageVertex>()) as u64;
        let image_vbuf_ring = (0..IMAGE_VBUF_RING_SIZE)
            .map(|_| Some(device.create_buffer_shared(image_vbuf_size)))
            .collect();

        Self {
            pipeline,
            #[cfg(target_os = "macos")]
            text_renderer,
            rect_commands: Vec::with_capacity(256),
            line_commands: Vec::with_capacity(64),
            vertices: Vec::with_capacity(INITIAL_VERTEX_CAPACITY),
            indices: Vec::with_capacity(INITIAL_INDEX_CAPACITY),
            vbuf_ring,
            ibuf_ring,
            ring_idx: 0,
            ring_stamps: vec![0; BUF_RING_SIZE],
            prepared_slot: 0,
            prepared_index_count: 0,
            prepared_globals: [0.0; 4],
            clip_stack: Vec::with_capacity(8),
            immediate_clip: None,
            immediate_clip_stack: Vec::with_capacity(4),
            depth_stack: Vec::with_capacity(4),
            transform_stack: Vec::with_capacity(4),
            scissor_batches: Vec::with_capacity(8),
            bug060_pass: 0,
            prepared_batches: Vec::with_capacity(8),
            prepared_depths: Vec::with_capacity(8),
            line_batch_scratch: Vec::with_capacity(8),
            prepared_physical_w: 0,
            prepared_physical_h: 0,
            image_pipeline,
            image_sampler,
            image_index_buf,
            image_vbuf_ring,
            image_ring_idx: 0,
            image_ring_stamps: vec![0; IMAGE_VBUF_RING_SIZE],
            image_commands: Vec::with_capacity(32),
            prepared_image_draws: Vec::with_capacity(32),
            prepared_image_slot: 0,
            image_textures: ahash::AHashMap::new(),
            frame_fence: None,
            fence_wait_events: 0,
        }
    }

    /// Install the shared GPU-completion fence used to gate rect-ring and
    /// image-ring slot reuse. Not set by the headless test harness.
    pub fn set_frame_fence(&mut self, fence: Arc<FrameFence>) {
        self.frame_fence = Some(fence);
    }

    // ── Static images (PRESET_LIBRARY_DESIGN P6, D7) ────────────────────

    /// Whether `handle` already has a registered GPU texture — the app calls
    /// this before decoding a PNG so a distinct key is decoded+uploaded at
    /// most once per process (stronger than "once per browser open": once
    /// cached, re-opening the browser is free too).
    pub fn has_image(&self, handle: TextureHandle) -> bool {
        self.image_textures.contains_key(&handle)
    }

    /// Register a decoded RGBA8 image under `handle`, uploading it to a fresh
    /// GPU texture. No-op (returns `false`) if `handle` is already
    /// registered — idempotent, so a caller that doesn't bother checking
    /// [`Self::has_image`] first still never re-uploads. `rgba.len()` must be
    /// `w * h * 4`.
    pub fn register_image(
        &mut self,
        device: &GpuDevice,
        handle: TextureHandle,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> bool {
        if self.image_textures.contains_key(&handle) {
            return false;
        }
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize, "rgba buffer size must be w*h*4");
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba8UnormSrgb,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
            label: "ui-registered-image",
            mip_levels: 1,
        });
        device.upload_texture(&tex, rgba);
        self.image_textures.insert(handle, tex);
        true
    }

    /// Install an already-GPU-resident texture under `handle`, replacing any
    /// prior entry. Unlike [`Self::register_image`] (which uploads CPU RGBA to a
    /// fresh texture), this shares an existing [`GpuTexture`] — a cheap
    /// `Retained` clone, no GPU allocation — so it's called every frame to point
    /// the graph node-preview handle at the current front of the rotating
    /// IOSurface atlas (or, in the headless harness, at each node's output).
    pub fn register_external_texture(&mut self, handle: TextureHandle, texture: GpuTexture) {
        self.image_textures.insert(handle, texture);
    }

    // ── Immediate-mode draw API ─────────────────────────────────────

    /// Clip subsequent immediate-mode draws (rects, lines, AND text) to
    /// `(x, y, w, h)` in logical coordinates, intersected with any enclosing
    /// immediate clip, until the matching [`Self::pop_immediate_clip`]. The
    /// graph canvas pushes its lane so nodes and wires can't bleed under the
    /// side panels. Must be balanced before `prepare` runs.
    pub fn push_immediate_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let rect = Rect::new(x, y, w, h);
        let clipped = match self.immediate_clip_stack.last() {
            Some(outer) => intersect_rects(*outer, rect),
            None => rect,
        };
        self.immediate_clip_stack.push(clipped);
        self.immediate_clip = Some(clipped);
    }

    /// Restore the immediate clip that was active before the matching push.
    pub fn pop_immediate_clip(&mut self) {
        debug_assert!(
            !self.immediate_clip_stack.is_empty(),
            "pop_immediate_clip without push"
        );
        self.immediate_clip_stack.pop();
        self.immediate_clip = self.immediate_clip_stack.last().copied();
    }

    /// Open the timeline's lane-content scissor (`docs/TIMELINE_INTERACTION_
    /// P1_SPEC.md` D7): clip bodies, the region/cursor/marker overlay, and any
    /// future drag chrome all draw through this, never a raw
    /// `push_immediate_clip` of their own. Returns an RAII guard so the clip
    /// closes on drop — including on an early return — instead of relying on
    /// a call site remembering the matching `pop_immediate_clip`. This is the
    /// structural half of D7; the two GPU content passes (clip waveforms,
    /// clip thumbnails) already can't opt out a different way — `tracks_rect`
    /// is a required parameter of their `render()`, not an optional one.
    pub fn lane_content_scissor(&mut self, tracks: Rect) -> LaneContentScissor<'_> {
        self.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
        LaneContentScissor { ui: self }
    }

    // ── Depth ───────────────────────────────────────────────────────

    /// The depth subsequent draw commands land on.
    pub fn current_depth(&self) -> Depth {
        self.depth_stack.last().copied().unwrap_or(Depth::BASE)
    }

    /// Draw subsequent commands at `depth` until the matching [`Self::pop_depth`].
    /// Must be balanced before `prepare` runs; an unbalanced push would
    /// silently float everything after it.
    pub fn push_depth(&mut self, depth: Depth) {
        self.depth_stack.push(depth);
    }

    /// Return to the depth that was active before the matching push.
    pub fn pop_depth(&mut self) {
        debug_assert!(!self.depth_stack.is_empty(), "pop_depth without push");
        self.depth_stack.pop();
    }

    // ── Transform ───────────────────────────────────────────────────

    /// The transform subsequent draw commands are captured under. Identity
    /// when the stack is empty.
    pub fn current_transform(&self) -> Affine2 {
        self.transform_stack.last().copied().unwrap_or(Affine2::IDENTITY)
    }

    /// Compose `transform` onto the current transform for subsequent commands
    /// (rects, lines, text, icons) until the matching [`Self::pop_transform`].
    pub fn push_transform(&mut self, transform: Affine2) {
        let composed = match self.transform_stack.last() {
            Some(top) => top.mul(&transform),
            None => transform,
        };
        self.transform_stack.push(composed);
    }

    /// Return to the transform that was active before the matching push.
    pub fn pop_transform(&mut self) {
        debug_assert!(!self.transform_stack.is_empty(), "pop_transform without push");
        self.transform_stack.pop();
    }

    /// Queue a filled rectangle.
    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: impl Into<LinearColor>) {
        self.rect_commands.push(RectCommand {
            x,
            y,
            w,
            h,
            color: color.into().0,
            corner_radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
            color2: [0.0; 4],
            grad: NO_GRAD,
            transform: self.current_transform(),
            depth: self.current_depth(),
            clip: self.immediate_clip,
        });
    }

    /// Queue a rounded rectangle.
    pub fn draw_rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: impl Into<LinearColor>,
        corner_radius: f32,
    ) {
        self.rect_commands.push(RectCommand {
            x,
            y,
            w,
            h,
            color: color.into().0,
            corner_radius,
            border_width: 0.0,
            border_color: [0.0; 4],
            color2: [0.0; 4],
            grad: NO_GRAD,
            transform: self.current_transform(),
            depth: self.current_depth(),
            clip: self.immediate_clip,
        });
    }

    /// Immediate-mode textured quad: sample `handle`'s `uv` sub-rect into the
    /// rect, masked to a rounded rect. Queued at the current depth + immediate
    /// clip, so it interleaves with the rects/text of the same depth (the
    /// per-depth render loop draws rects, then images, then text). A graph
    /// node's output preview drawn right after its body — inside the node's own
    /// depth band — is therefore occluded correctly by any node stacked above
    /// it, instead of the old flat post-pass blit that ignored node z-order.
    /// `uv` is `[u0, v0, u1, v1]`: `[0, 0, 1, 1]` for a whole texture, a cell
    /// rect for one node's slot in the shared preview atlas.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image_uv(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        handle: TextureHandle,
        uv: [f32; 4],
        corner_radius: f32,
    ) {
        self.image_commands.push(ImageCommand {
            x,
            y,
            w,
            h,
            corner_radius,
            handle,
            depth: self.current_depth(),
            uv,
            clip: self.immediate_clip,
        });
    }

    /// Queue a rounded rectangle with a linear-gradient body from `start` to
    /// `end`, interpolated along the unit direction `dir` in the rect's UV space
    /// (`[0.0, 1.0]` = top→bottom, `[1.0, 0.0]` = left→right). The shared
    /// primitive behind gradient card/clip bodies (§24 5a); border + shadow paths
    /// are unaffected. `corner_radius: 0.0` gives a flat (non-rounded) gradient.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_gradient_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corner_radius: f32,
        start: impl Into<LinearColor>,
        end: impl Into<LinearColor>,
        dir: [f32; 2],
    ) {
        self.rect_commands.push(RectCommand {
            x,
            y,
            w,
            h,
            color: start.into().0,
            corner_radius,
            border_width: 0.0,
            border_color: [0.0; 4],
            color2: end.into().0,
            grad: [dir[0], dir[1], 1.0, 0.0],
            transform: self.current_transform(),
            depth: self.current_depth(),
            clip: self.immediate_clip,
        });
    }

    /// Queue a rounded rectangle with border.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_bordered_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: impl Into<LinearColor>,
        corner_radius: f32,
        border_width: f32,
        border_color: impl Into<LinearColor>,
    ) {
        self.rect_commands.push(RectCommand {
            x,
            y,
            w,
            h,
            color: color.into().0,
            corner_radius,
            border_width,
            border_color: border_color.into().0,
            color2: [0.0; 4],
            grad: NO_GRAD,
            transform: self.current_transform(),
            depth: self.current_depth(),
            clip: self.immediate_clip,
        });
    }

    /// Queue a soft drop-shadow for a rounded element (§17 elevation). The
    /// shadow quad is the element rect grown by `blur` on every side; the
    /// element's rounded rect sits inset at the centre and alpha falls from
    /// `color.a` at its edge to 0 over `blur` px. Encoded as a negative border
    /// width so it needs no extra vertex attribute. Draw it BEFORE (under) the
    /// element, offset slightly for a directional drop. Floating surfaces only
    /// — keep the colour dark and the alpha low (a lift, not a glow).
    pub fn draw_shadow(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corner_radius: f32,
        blur: f32,
        color: impl Into<LinearColor>,
    ) {
        if blur <= 0.0 {
            return;
        }
        self.rect_commands.push(RectCommand {
            x: x - blur,
            y: y - blur,
            w: w + 2.0 * blur,
            h: h + 2.0 * blur,
            color: color.into().0,
            corner_radius,
            border_width: -blur,
            border_color: [0.0; 4],
            color2: [0.0; 4],
            grad: NO_GRAD,
            transform: self.current_transform(),
            depth: self.current_depth(),
            clip: self.immediate_clip,
        });
    }

    /// Queue a solid-coloured line segment with the given thickness.
    /// Drawn as an oriented filled quad; honours the current scissor
    /// batch like any rect.
    pub fn draw_line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        thickness: f32,
        color: impl Into<LinearColor>,
    ) {
        self.line_commands.push(LineCommand {
            x0,
            y0,
            x1,
            y1,
            thickness,
            color: color.into().0,
            depth: self.current_depth(),
            clip: self.immediate_clip,
            transform: self.current_transform(),
        });
    }

    /// Queue text at a position.
    pub fn draw_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        font_size: f32,
        color: impl Into<TextColor>,
    ) {
        #[cfg(target_os = "macos")]
        {
            let color = color.into().0;
            // Honour the immediate-mode clip so canvas labels stay in their lane
            // alongside the clipped nodes/wires (None = unclipped, as before).
            let clip = self
                .immediate_clip
                .map(|r| [r.x, r.y, r.x + r.width, r.y + r.height]);
            let depth = self.current_depth();
            let transform = self.current_transform();
            self.text_renderer
                .draw_text(x, y, text, font_size, color, FontWeight::Medium, clip, depth, transform);
        }
    }

    // ── Scissor batch helpers ───────────────────────────────────────
    //
    // Every rect command carries its own `(clip, depth)`, captured at enqueue
    // time (`RectCommand::clip`/`depth` — see `draw_rect` et al. and
    // `draw_node`). `ScissorBatch`es are not accumulated during drawing at
    // all; `prepare()` derives them in one linear run-scan over
    // `rect_commands`, grouping consecutive commands that share the same
    // `(clip, depth)`. There is no "pending run" to flush on a layer/clip/
    // transform boundary, so a boundary crossed mid-traversal can no longer
    // stamp the wrong scissor on already-enqueued rects (BUG-060).

    /// Begin scissor tracking for a tree traversal: reset the clip context.
    fn begin_scissor_tracking(&mut self) {
        self.clip_stack.clear();
    }

    /// BUG-060 trace: with `MANIFOLD_BUG060_TRACE=x0,y0,x1,y1` (logical px) set,
    /// log every rect in the batch that intersects that band, with the
    /// batch's effective scissor, origin, and the panel slot set by the
    /// cache manager. Attributes escaped pixels found in the surface dumps
    /// to the exact draw that produced them. One `Option` check when
    /// disarmed. Remove with BUG-060.
    fn bug060_log_batch(&self, origin: &str, scissor: Option<Rect>, start: usize, count: usize) {
        let Some(band) = bug060_trace_band() else {
            return;
        };
        for rc in &self.rect_commands[start..start + count] {
            let intersects = rc.x < band[2]
                && rc.x + rc.w > band[0]
                && rc.y < band[3]
                && rc.y + rc.h > band[1];
            if intersects {
                eprintln!(
                    "[BUG060-TRACE] {origin} pass={} rect=({:.1},{:.1} {:.1}x{:.1}) color=({:.3},{:.3},{:.3},{:.3}) scissor={:?}",
                    self.bug060_pass,
                    rc.x,
                    rc.y,
                    rc.w,
                    rc.h,
                    rc.color[0],
                    rc.color[1],
                    rc.color[2],
                    rc.color[3],
                    scissor,
                );
            }
        }
    }

    /// Handle a PushClip event: push the intersected clip onto `clip_stack`.
    fn handle_push_clip(&mut self, rect: Rect) {
        let clipped = if let Some(current) = self.clip_stack.last() {
            intersect_rects(*current, rect)
        } else {
            rect
        };
        self.clip_stack.push(clipped);
    }

    /// Handle a PopClip event: pop `clip_stack`.
    fn handle_pop_clip(&mut self) {
        self.clip_stack.pop();
    }

    // ── UITree rendering ────────────────────────────────────────────

    /// Render a UITree. When `skip_from` is `Some(n)`, nodes with
    /// `id >= n` are skipped (used to exclude dropdown overlay nodes
    /// that render separately via `render_tree_range`).
    pub fn render_tree(&mut self, tree: &UITree, skip_from: Option<usize>) {
        self.begin_scissor_tracking();

        tree.traverse(|event| match event {
            TraversalEvent::Node(node) => {
                if let Some(start) = skip_from
                    && node.id.index() >= start
                {
                    return;
                }
                self.draw_node(node);
            }
            TraversalEvent::PushClip(rect) => self.handle_push_clip(rect),
            TraversalEvent::PopClip => self.handle_pop_clip(),
        });
    }

    /// Render tree nodes in range [start_node, end_node) on the current
    /// layer. Uses `traverse_range` to only walk root nodes in the given
    /// range, avoiding a full-tree traversal per section. Traversals are
    /// additive within a prepare cycle: earlier commands (including pending
    /// immediate-mode draws) are preserved — batches derive from all of
    /// `rect_commands` together in `prepare()`.
    pub fn render_tree_range(&mut self, tree: &UITree, start_node: usize, end_node: usize) {
        self.begin_scissor_tracking();

        tree.traverse_range(start_node, end_node, |event| match event {
            TraversalEvent::Node(node) => self.draw_node(node),
            TraversalEvent::PushClip(rect) => self.handle_push_clip(rect),
            TraversalEvent::PopClip => self.handle_pop_clip(),
        });
    }

    /// Render a sub-region using flat sequential traversal.
    /// Used for incremental inspector rendering — correctly handles reparented
    /// nodes (where `traverse_range` would skip them).
    ///
    /// When `dirty_only` is true, only draws dirty nodes. The atlas already
    /// contains previous content via LoadOp::Load, so non-dirty nodes are
    /// preserved. Clip events are always processed for correctness.
    pub fn render_sub_region(&mut self, tree: &UITree, start: usize, end: usize, dirty_only: bool) {
        self.begin_scissor_tracking();

        tree.traverse_flat_range(start, end, dirty_only, |event| match event {
            TraversalEvent::Node(node) => self.draw_node(node),
            TraversalEvent::PushClip(rect) => self.handle_push_clip(rect),
            TraversalEvent::PopClip => self.handle_pop_clip(),
        });
    }

    /// Draw a single UI node — resolves effective colors and emits commands.
    /// Uses original node bounds (no geometry clamping). Scissor rects handle
    /// pixel-level clipping at the GPU level.
    fn draw_node(&mut self, node: &UINode) {
        let style = &node.style;
        let bounds = node.bounds;

        // Skip zero-area rects
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }

        // Early out: skip nodes entirely outside the clip region.
        if let Some(clip) = self.clip_stack.last()
            && (bounds.x >= clip.x_max()
                || bounds.x_max() <= clip.x
                || bounds.y >= clip.y_max()
                || bounds.y_max() <= clip.y)
        {
            return;
        }

        // Node-local affine (pivot = this node's own rect center — bounds
        // aren't known until layout runs, so `UIStyle::transform` is expressed
        // about the local origin and pivoted here; no subtree inheritance in
        // v1, `docs/UI_TRANSFORM_STACK_DESIGN.md`). Pushed once around ALL of
        // this node's draws below — background, text, dropdown caret — and
        // popped at the end of the function.
        let has_transform = if let Some(t) = style.transform {
            let cx = bounds.x + bounds.width * 0.5;
            let cy = bounds.y + bounds.height * 0.5;
            let pivoted = Affine2::translate(cx, cy).mul(&t).mul(&Affine2::translate(-cx, -cy));
            self.push_transform(pivoted);
            true
        } else {
            false
        };

        // Resolve effective background color from interaction flags
        let mut bg_color = style.bg_color;
        if node.flags.contains(UIFlags::PRESSED) && style.pressed_bg_color.a > 0 {
            bg_color = style.pressed_bg_color;
        } else if node.flags.contains(UIFlags::HOVERED) && style.hover_bg_color.a > 0 {
            bg_color = style.hover_bg_color;
        }

        // DISABLED: dim background and text to signal non-interactive state.
        let disabled = node.flags.contains(UIFlags::DISABLED);
        if disabled {
            bg_color = Color32::new(bg_color.r, bg_color.g, bg_color.b, bg_color.a / 3);
        }

        // Background
        if bg_color.a > 0 {
            let color = bg_color.to_f32();
            if style.border_width > 0.0 && style.border_color.a > 0 {
                self.rect_commands.push(RectCommand {
                    x: bounds.x,
                    y: bounds.y,
                    w: bounds.width,
                    h: bounds.height,
                    color,
                    corner_radius: style.corner_radius,
                    border_width: style.border_width,
                    border_color: style.border_color.to_f32(),
                    color2: [0.0; 4],
                    grad: NO_GRAD,
                    transform: self.current_transform(),
                    depth: self.current_depth(),
                    clip: self.clip_stack.last().copied(),
                });
            } else if style.corner_radius > 0.0 {
                self.rect_commands.push(RectCommand {
                    x: bounds.x,
                    y: bounds.y,
                    w: bounds.width,
                    h: bounds.height,
                    color,
                    corner_radius: style.corner_radius,
                    border_width: 0.0,
                    border_color: [0.0; 4],
                    color2: [0.0; 4],
                    grad: NO_GRAD,
                    transform: self.current_transform(),
                    depth: self.current_depth(),
                    clip: self.clip_stack.last().copied(),
                });
            } else {
                self.rect_commands.push(RectCommand {
                    x: bounds.x,
                    y: bounds.y,
                    w: bounds.width,
                    h: bounds.height,
                    color,
                    corner_radius: 0.0,
                    border_width: 0.0,
                    border_color: [0.0; 4],
                    color2: [0.0; 4],
                    grad: NO_GRAD,
                    transform: self.current_transform(),
                    depth: self.current_depth(),
                    clip: self.clip_stack.last().copied(),
                });
            }
        } else if style.border_width > 0.0 && style.border_color.a > 0 {
            // Border-only (transparent bg)
            self.rect_commands.push(RectCommand {
                x: bounds.x,
                y: bounds.y,
                w: bounds.width,
                h: bounds.height,
                color: [0.0, 0.0, 0.0, 0.0],
                corner_radius: style.corner_radius,
                border_width: style.border_width,
                border_color: style.border_color.to_f32(),
                color2: [0.0; 4],
                grad: NO_GRAD,
                transform: self.current_transform(),
                depth: self.current_depth(),
                clip: self.clip_stack.last().copied(),
            });
        }

        // Static image (PRESET_LIBRARY_DESIGN P6, D7): a browser cell's
        // thumbnail fill. Queued after the (transparent, for an Image node)
        // background so it paints in the same relative order a bg fill
        // would, and before text so a caption/badge label drawn on the same
        // rect stays legible on top.
        if node.node_type == UINodeType::Image
            && let Some(handle) = node.texture
        {
            self.image_commands.push(ImageCommand {
                x: bounds.x,
                y: bounds.y,
                w: bounds.width,
                h: bounds.height,
                corner_radius: style.corner_radius,
                handle,
                depth: self.current_depth(),
                uv: [0.0, 0.0, 1.0, 1.0],
                clip: self.clip_stack.last().copied(),
            });
        }

        // Every glyph a node draws is clipped to the node's own rect,
        // intersected with the tree clip (scroll containers etc.). Bound here
        // at enqueue, like `RectCommand::clip` — a label longer than its
        // widget cuts at the edge instead of painting over the neighbour
        // (same doctrine as BUG-060: containment is structural, not a per-
        // call-site courtesy). Layout sizes label nodes to their measured
        // text, so a well-fitted label is untouched; only genuine overrun
        // clips. `draw_node` already skips zero-area nodes above, so this
        // never turns a real label into an empty clip.
        #[cfg(target_os = "macos")]
        let node_clip: [f32; 4] = match self.clip_stack.last() {
            Some(c) => [
                c.x.max(bounds.x),
                c.y.max(bounds.y),
                c.x_max().min(bounds.x_max()),
                c.y_max().min(bounds.y_max()),
            ],
            None => [bounds.x, bounds.y, bounds.x_max(), bounds.y_max()],
        };

        // Text (or icon if the first char is an atlas-icon codepoint — see
        // `manifold_ui::icons::Icon`).
        // Comfort inset for genuinely overrunning text — see the clip
        // computation in the non-icon branch below.
        const TEXT_CLIP_PAD_X: f32 = 3.0;
        #[cfg(target_os = "macos")]
        if let Some(text) = &node.text
            && !text.is_empty()
        {
            let text_color = if disabled {
                [
                    style.text_color.r,
                    style.text_color.g,
                    style.text_color.b,
                    style.text_color.a / 3,
                ]
            } else {
                [
                    style.text_color.r,
                    style.text_color.g,
                    style.text_color.b,
                    style.text_color.a,
                ]
            };

            let depth = self.current_depth();
            let transform = self.current_transform();
            let first_char = text.chars().next().unwrap();
            if let Some(icon_id) = manifold_ui::icons::Icon::id_from_char(first_char) {
                // Icon: square aspect ratio, centered in bounds
                let pad = 2.0_f32;
                let icon_size = (bounds.width.min(bounds.height) - pad * 2.0).max(4.0);
                let icon_w = icon_size;
                let icon_h = icon_size;
                let icon_x = bounds.x + (bounds.width - icon_size) * 0.5;
                let icon_y = bounds.y + (bounds.height - icon_size) * 0.5;
                self.text_renderer.draw_icon(
                    icon_id,
                    icon_x,
                    icon_y,
                    icon_w,
                    icon_h,
                    text_color,
                    // Un-padded: the icon is already inset + square-fit above;
                    // the text pad would shave its edge on narrow chips.
                    Some(node_clip),
                    depth,
                    transform,
                );
            } else {
                let text_size = self.text_renderer.measure_text_cached(
                    text,
                    style.font_size,
                    style.font_weight,
                );
                let fs = style.font_size as f32;
                let text_y =
                    bounds.y + (bounds.height - text_size.y) * 0.5 + fs * VERTICAL_OPTICAL_NUDGE;
                let inset = style.text_inset_x;

                // Optional leading dim micro-label (mockup `.blend b`) painted in
                // `prefix_color`. Measured FIRST so alignment positions the whole
                // label+value block as a unit — Center centres "BLEND Normal"
                // together, not just the value (which would split the label off).
                const PREFIX_GAP: f32 = 5.0;
                let prefix = style.prefix_label.filter(|p| !p.is_empty());
                let prefix_advance = match prefix {
                    Some(p) => {
                        self.text_renderer
                            .measure_text_cached(p, style.font_size, style.font_weight)
                            .x
                            + PREFIX_GAP
                    }
                    None => 0.0,
                };

                let block_w = prefix_advance + text_size.x;
                let start_x = match style.text_align {
                    TextAlign::Center => bounds.x + (bounds.width - block_w) * 0.5,
                    TextAlign::Right => bounds.x + bounds.width - block_w - inset,
                    TextAlign::Left => bounds.x + inset,
                };

                // Overrunning text cuts TEXT_CLIP_PAD_X inside the node edge,
                // not flush against it — a hard cut touching the border reads
                // as broken; a small gap reads as deliberate truncation. The
                // pad applies per side, and ONLY on a side the text actually
                // overruns (vs the node's own rect): layout sizes label nodes
                // to their measured text, so well-fitted text legitimately
                // sits flush at the edge (left-aligned menu rows, right-
                // aligned param labels) and an unconditional pad shaves real
                // glyphs off it — the 2026-07-10 regression. A node cut by
                // the tree clip (scroll container) keeps the hard cut like
                // every other element; the pad band lives at the node's own
                // edge only. Horizontal only (ascenders/descenders sit close
                // to top/bottom on short chips), text only (icons are already
                // inset + square-fit, the caret is pinned inside). The pad
                // shrinks on nodes too narrow to afford it rather than
                // inverting the clip. Half-pixel slack: flush placement
                // reaches the edge via float arithmetic; a sub-pixel
                // "overrun" doesn't warrant the gap.
                let text_end = start_x + block_w;
                let pad = TEXT_CLIP_PAD_X.min(bounds.width * 0.25);
                let clip_x0 = if start_x < bounds.x - 0.5 {
                    node_clip[0].max(bounds.x + pad)
                } else {
                    node_clip[0]
                };
                // `.max(clip_x0)`: a scrolled-out sliver intersected with the
                // pad band can invert; an empty clip beats a flipped quad.
                let clip_x1 = if text_end > bounds.x_max() + 0.5 {
                    node_clip[2].min(bounds.x_max() - pad)
                } else {
                    node_clip[2]
                }
                .max(clip_x0);
                let clip_bounds = Some([clip_x0, node_clip[1], clip_x1, node_clip[3]]);

                if let Some(p) = prefix {
                    let pc = style.prefix_color;
                    self.text_renderer.draw_text(
                        start_x,
                        text_y,
                        p,
                        fs,
                        [pc.r, pc.g, pc.b, pc.a],
                        style.font_weight,
                        clip_bounds,
                        depth,
                        transform,
                    );
                }

                self.text_renderer.draw_text(
                    start_x + prefix_advance,
                    text_y,
                    text,
                    fs,
                    text_color,
                    style.font_weight,
                    clip_bounds,
                    depth,
                    transform,
                );
            }
        }

        // Dropdown caret (§M / mockup `.sel::after`): a dim ▼ pinned to the
        // node's right edge, drawn independent of the main text so a value chip
        // reads as a dropdown without baking the glyph into the value string.
        #[cfg(target_os = "macos")]
        if style.dropdown_caret {
            const CARET: &str = "\u{25BC}";
            let clip_bounds = Some(node_clip);
            let caret_color = manifold_ui::color::CHIP_CARET;
            let caret_color = [caret_color.r, caret_color.g, caret_color.b, caret_color.a];
            let caret_font = manifold_ui::color::CHIP_CARET_FONT;
            let size = self
                .text_renderer
                .measure_text_cached(CARET, caret_font, style.font_weight);
            let caret_x =
                bounds.x_max() - manifold_ui::color::CHIP_CARET_PAD_X - size.x;
            let caret_y = bounds.y + (bounds.height - size.y) * 0.5
                + (caret_font as f32) * VERTICAL_OPTICAL_NUDGE;
            let depth = self.current_depth();
            let transform = self.current_transform();
            self.text_renderer.draw_text(
                caret_x,
                caret_y,
                CARET,
                caret_font as f32,
                caret_color,
                style.font_weight,
                clip_bounds,
                depth,
                transform,
            );
        }

        if has_transform {
            self.pop_transform();
        }
    }

    /// Queue an atlas-icon draw. `icon_id` is a [`manifold_ui::icons::Icon`] id.
    /// Mirrors [`Self::draw_text`]: the method exists on every target, the body is
    /// macOS-only (the glyph atlas is CoreText-backed).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_icon(
        &mut self,
        icon_id: u8,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: impl Into<TextColor>,
        clip_bounds: Option<[f32; 4]>,
    ) {
        #[cfg(target_os = "macos")]
        {
            let depth = self.current_depth();
            let transform = self.current_transform();
            self.text_renderer
                .draw_icon(icon_id, x, y, w, h, color.into().0, clip_bounds, depth, transform);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (icon_id, x, y, w, h, color, clip_bounds);
    }

    /// Text measurement using NativeTextRenderer's cached measurement.
    pub fn measure_text_cached(
        &mut self,
        text: &str,
        font_size: u16,
        font_weight: FontWeight,
    ) -> Vec2 {
        #[cfg(target_os = "macos")]
        return self
            .text_renderer
            .measure_text_cached(text, font_size, font_weight);
        #[cfg(not(target_os = "macos"))]
        {
            let em = font_size as f32;
            Vec2::new(text.len() as f32 * em * 0.54, em)
        }
    }

    // ── Render pass ─────────────────────────────────────────────────

    /// Advance text renderer frame counter (call once per frame).
    pub fn begin_frame(&mut self) {
        // Each frame starts unclipped on the Base layer; a caller (the graph
        // canvas) re-arms its lane clip after this, before queuing draws.
        self.immediate_clip = None;
        self.immediate_clip_stack.clear();
        self.depth_stack.clear();
        self.transform_stack.clear();
        #[cfg(target_os = "macos")]
        self.text_renderer.begin_frame();
    }

    /// Prepare vertex/index buffers and text for drawing. Call before `render()`.
    /// Returns `true` if there is content to draw.
    pub fn prepare(
        &mut self,
        device: &GpuDevice,
        width: u32,
        height: u32,
        scale_factor: f64,
    ) -> bool {
        self.prepare_with_offset(device, width, height, 0.0, 0.0, scale_factor)
    }

    /// Prepare with viewport offset for panel-local rendering.
    ///
    /// `viewport_w`/`viewport_h`: panel texture size in logical pixels.
    /// `offset_x`/`offset_y`: panel's screen-space origin (subtracted in shader).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_with_offset(
        &mut self,
        device: &GpuDevice,
        viewport_w: u32,
        viewport_h: u32,
        offset_x: f32,
        offset_y: f32,
        scale_factor: f64,
    ) -> bool {
        debug_assert!(
            self.depth_stack.is_empty(),
            "unbalanced push_depth at prepare — everything after the push floats"
        );
        debug_assert!(
            self.immediate_clip_stack.is_empty(),
            "unbalanced push_immediate_clip at prepare — the clip leaks into later draws"
        );
        debug_assert!(
            self.transform_stack.is_empty(),
            "unbalanced push_transform at prepare — the transform leaks into later draws"
        );
        self.prepared_globals = [viewport_w as f32, viewport_h as f32, offset_x, offset_y];
        let sf = scale_factor as f32;
        self.prepared_physical_w = (viewport_w as f32 * sf).ceil() as u32;
        self.prepared_physical_h = (viewport_h as f32 * sf).ceil() as u32;

        // Build vertex/index data from rect commands.
        self.vertices.clear();
        self.indices.clear();

        for cmd in &self.rect_commands {
            let base = self.vertices.len() as u32;

            let (x0, y0) = (cmd.x, cmd.y);
            let (x1, y1) = (cmd.x + cmd.w, cmd.y + cmd.h);
            // Multiply each corner POSITION by the command's captured affine
            // (`docs/UI_TRANSFORM_STACK_DESIGN.md`). `uv`/`rect_params` are
            // untouched — the fragment shader's rounded-rect SDF, border ring,
            // gradient, and shadow all run in local uv-space, so they
            // rotate/scale correctly with zero shader changes.
            let (p0x, p0y) = cmd.transform.apply((x0, y0));
            let (p1x, p1y) = cmd.transform.apply((x1, y0));
            let (p2x, p2y) = cmd.transform.apply((x1, y1));
            let (p3x, p3y) = cmd.transform.apply((x0, y1));

            self.vertices.push(UIVertex {
                position: [p0x, p0y],
                uv: [0.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
                color2: cmd.color2,
                grad: cmd.grad,
            });
            self.vertices.push(UIVertex {
                position: [p1x, p1y],
                uv: [1.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
                color2: cmd.color2,
                grad: cmd.grad,
            });
            self.vertices.push(UIVertex {
                position: [p2x, p2y],
                uv: [1.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
                color2: cmd.color2,
                grad: cmd.grad,
            });
            self.vertices.push(UIVertex {
                position: [p3x, p3y],
                uv: [0.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
                color2: cmd.color2,
                grad: cmd.grad,
            });

            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }

        // Derive scissor batches: a linear run-scan over `rect_commands`
        // grouping CONSECUTIVE commands that share the same `(clip, depth)`
        // — both captured per command at enqueue time, never inferred here.
        // This is the structural fix for BUG-060: there is no flush-time
        // decision left to get wrong, so a transform/depth boundary crossed
        // mid-traversal can no longer stamp the wrong scissor on rects that
        // were already enqueued.
        self.scissor_batches.clear();
        let rect_count = self.rect_commands.len();
        if rect_count > 0 {
            let mut run_start = 0usize;
            let mut run_clip = self.rect_commands[0].clip;
            let mut run_depth = self.rect_commands[0].depth;
            let mut run_count = 1usize;
            for i in 1..rect_count {
                let cmd_clip = self.rect_commands[i].clip;
                let cmd_depth = self.rect_commands[i].depth;
                if cmd_clip == run_clip && cmd_depth == run_depth {
                    run_count += 1;
                } else {
                    self.bug060_log_batch("prepared", run_clip, run_start, run_count);
                    self.scissor_batches.push(ScissorBatch {
                        scissor: run_clip,
                        rect_start: run_start,
                        rect_count: run_count,
                        depth: run_depth,
                    });
                    run_start = i;
                    run_clip = cmd_clip;
                    run_depth = cmd_depth;
                    run_count = 1;
                }
            }
            self.bug060_log_batch("prepared", run_clip, run_start, run_count);
            self.scissor_batches.push(ScissorBatch {
                scissor: run_clip,
                rect_start: run_start,
                rect_count: run_count,
                depth: run_depth,
            });
        }

        // Distinct depths present across rect batches and lines, ascending.
        // The render loop walks this union (plus any text-only depths) instead
        // of a fixed layer set, so a new floating surface only has to pick a
        // depth. A single-depth frame yields a one-element list and the
        // identical draw sequence to the pre-depth renderer.
        let mut depths: Vec<Depth> = self
            .scissor_batches
            .iter()
            .map(|b| b.depth)
            .chain(self.line_commands.iter().map(|c| c.depth))
            .chain(self.image_commands.iter().map(|c| c.depth))
            .collect();
        depths.sort_unstable();
        depths.dedup();

        // Emit lines after rects, grouped by depth so each depth's lines form
        // a contiguous index range. Each line is an oriented quad — four
        // corners offset by ±(perpendicular * half_thickness). The fragment
        // shader's fast path (rect_params zeroed) returns a flat fill, so we
        // don't need a separate pipeline. Within a depth, runs of lines
        // sharing a clip become one batch (clip is captured at draw time).
        let mut line_batches = std::mem::take(&mut self.line_batch_scratch);
        line_batches.clear();
        for &depth in &depths {
            for cmd in self.line_commands.iter().filter(|c| c.depth == depth) {
                let dx = cmd.x1 - cmd.x0;
                let dy = cmd.y1 - cmd.y0;
                let len_sq = dx * dx + dy * dy;
                if len_sq <= f32::EPSILON {
                    continue;
                }
                let inv_len = len_sq.sqrt().recip();
                let half = cmd.thickness * 0.5;
                let nx = -dy * inv_len * half;
                let ny = dx * inv_len * half;
                let zero_params = [0.0; 4];
                let zero_border = [0.0; 4];
                let base = self.vertices.len() as u32;
                // Corner positions in local (unrotated) space, then through the
                // command's captured affine — same treatment as rect corners.
                let (q0x, q0y) = cmd.transform.apply((cmd.x0 + nx, cmd.y0 + ny));
                let (q1x, q1y) = cmd.transform.apply((cmd.x1 + nx, cmd.y1 + ny));
                let (q2x, q2y) = cmd.transform.apply((cmd.x1 - nx, cmd.y1 - ny));
                let (q3x, q3y) = cmd.transform.apply((cmd.x0 - nx, cmd.y0 - ny));
                self.vertices.push(UIVertex {
                    position: [q0x, q0y],
                    uv: [0.0, 0.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                    color2: cmd.color,
                    grad: zero_params,
                });
                self.vertices.push(UIVertex {
                    position: [q1x, q1y],
                    uv: [1.0, 0.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                    color2: cmd.color,
                    grad: zero_params,
                });
                self.vertices.push(UIVertex {
                    position: [q2x, q2y],
                    uv: [1.0, 1.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                    color2: cmd.color,
                    grad: zero_params,
                });
                self.vertices.push(UIVertex {
                    position: [q3x, q3y],
                    uv: [0.0, 1.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                    color2: cmd.color,
                    grad: zero_params,
                });
                let idx_offset = (self.indices.len() * std::mem::size_of::<u32>()) as u64;
                self.indices
                    .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);

                match line_batches.last_mut() {
                    Some((d, clip, _, count)) if *d == depth && *clip == cmd.clip => {
                        *count += 6;
                    }
                    _ => line_batches.push((depth, cmd.clip, idx_offset, 6)),
                }
            }
        }

        // Build prepared batches: for each depth, that depth's rect batches
        // (insertion order) then its line batches — so within a depth, lines
        // draw over rects exactly as they always have globally, and a
        // single-depth frame issues the identical sequence to the pre-depth
        // renderer.
        let pw = self.prepared_physical_w;
        let ph = self.prepared_physical_h;
        let to_physical = |r: Rect| {
            let x0 = ((r.x - offset_x) * sf).floor().max(0.0) as u32;
            let y0 = ((r.y - offset_y) * sf).floor().max(0.0) as u32;
            let x1 = ((r.x + r.width - offset_x) * sf).ceil() as u32;
            let y1 = ((r.y + r.height - offset_y) * sf).ceil() as u32;
            [
                x0.min(pw),
                y0.min(ph),
                (x1 - x0).min(pw - x0.min(pw)),
                (y1 - y0).min(ph - y0.min(ph)),
            ]
        };
        self.prepared_batches.clear();
        for &depth in &depths {
            for batch in self.scissor_batches.iter().filter(|b| b.depth == depth) {
                if batch.rect_count == 0 {
                    continue;
                }
                self.prepared_batches.push(PreparedBatch {
                    scissor: batch.scissor.map(to_physical),
                    index_offset: (batch.rect_start * 6 * std::mem::size_of::<u32>()) as u64,
                    index_count: (batch.rect_count * 6) as u32,
                    depth,
                });
            }
            for &(d, clip, index_offset, index_count) in &line_batches {
                if d != depth {
                    continue;
                }
                self.prepared_batches.push(PreparedBatch {
                    scissor: clip.map(to_physical),
                    index_offset,
                    index_count,
                    depth,
                });
            }
        }

        // Static images (PRESET_LIBRARY_DESIGN P6, D7): one quad per queued
        // command, into a dedicated small ring-buffered vertex buffer (a
        // separate textured pipeline from the flat/gradient rect one, so it
        // can't share `self.vertices`). Capped at `MAX_IMAGE_QUADS` — a
        // generous bound for a browser cell grid; past it, extras are
        // silently dropped (same graceful-truncation shape `ClipThumbGpu`
        // uses) rather than overflowing the fixed-size buffer.
        self.prepared_image_draws.clear();
        if !self.image_commands.is_empty() {
            let slot = self.image_ring_idx % IMAGE_VBUF_RING_SIZE;
            self.image_ring_idx += 1;
            if let Some(fence) = &self.frame_fence {
                fence.guard_slot(
                    "UIRenderer(image)",
                    slot,
                    &mut self.image_ring_stamps[slot],
                    &mut self.fence_wait_events,
                );
            }
            self.prepared_image_slot = slot;
            let mut image_vertices: Vec<ImageVertex> =
                Vec::with_capacity(self.image_commands.len().min(MAX_IMAGE_QUADS) * 4);
            for cmd in self.image_commands.iter().take(MAX_IMAGE_QUADS) {
                let rect_params = [cmd.w, cmd.h, cmd.corner_radius, 0.0];
                let [u0, v0, u1, v1] = cmd.uv;
                let vertex_offset =
                    (image_vertices.len() * std::mem::size_of::<ImageVertex>()) as u64;
                image_vertices.push(ImageVertex { position: [cmd.x, cmd.y], uv: [u0, v0], rect_params });
                image_vertices.push(ImageVertex {
                    position: [cmd.x + cmd.w, cmd.y],
                    uv: [u1, v0],
                    rect_params,
                });
                image_vertices.push(ImageVertex {
                    position: [cmd.x + cmd.w, cmd.y + cmd.h],
                    uv: [u1, v1],
                    rect_params,
                });
                image_vertices.push(ImageVertex {
                    position: [cmd.x, cmd.y + cmd.h],
                    uv: [u0, v1],
                    rect_params,
                });
                self.prepared_image_draws.push(PreparedImageDraw {
                    scissor: cmd.clip.map(to_physical),
                    depth: cmd.depth,
                    handle: cmd.handle,
                    vertex_offset,
                });
            }
            let vdata = bytemuck::cast_slice::<ImageVertex, u8>(&image_vertices);
            let vbuf = match self.image_vbuf_ring[slot].take() {
                Some(buf) if buf.size >= vdata.len() as u64 => buf,
                _ => device.create_buffer_shared(vdata.len() as u64),
            };
            unsafe {
                vbuf.write(0, vdata);
            }
            self.image_vbuf_ring[slot] = Some(vbuf);
        }
        self.image_commands.clear();

        // Ring-buffered GPU buffers: each prepare() call advances to the next
        // ring slot, preventing aliasing with in-flight GPU work from previous
        // prepare/commit cycles (both within this frame and across frames).
        // Buffers grow in-place when needed but are never freed.
        if !self.vertices.is_empty() {
            let slot = self.ring_idx % BUF_RING_SIZE;
            self.ring_idx += 1;
            if let Some(fence) = &self.frame_fence {
                fence.guard_slot(
                    "UIRenderer(rect)",
                    slot,
                    &mut self.ring_stamps[slot],
                    &mut self.fence_wait_events,
                );
            }

            let vdata = bytemuck::cast_slice::<UIVertex, u8>(&self.vertices);
            let vbuf = match self.vbuf_ring[slot].take() {
                Some(buf) if buf.size >= vdata.len() as u64 => buf,
                _ => device.create_buffer_shared(vdata.len() as u64),
            };
            unsafe {
                vbuf.write(0, vdata);
            }

            let idata = bytemuck::cast_slice::<u32, u8>(&self.indices);
            let ibuf = match self.ibuf_ring[slot].take() {
                Some(buf) if buf.size >= idata.len() as u64 => buf,
                _ => device.create_buffer_shared(idata.len() as u64),
            };
            unsafe {
                ibuf.write(0, idata);
            }

            self.vbuf_ring[slot] = Some(vbuf);
            self.ibuf_ring[slot] = Some(ibuf);
            self.prepared_slot = slot;
            self.prepared_index_count = self.indices.len() as u32;
        } else {
            self.prepared_index_count = 0;
        }

        // Prepare text.
        #[cfg(target_os = "macos")]
        let has_text = self.text_renderer.prepare(
            device,
            viewport_w,
            viewport_h,
            offset_x,
            offset_y,
            scale_factor,
        );
        #[cfg(not(target_os = "macos"))]
        let has_text = false;

        // The render loop must visit every depth that has rects, lines, OR
        // text — a depth with text but no rect (e.g. an unfilled label) would
        // otherwise be skipped. Merge the text renderer's depths into the
        // rect/line set.
        #[cfg(target_os = "macos")]
        {
            depths.extend_from_slice(self.text_renderer.depths());
            depths.sort_unstable();
            depths.dedup();
        }
        self.prepared_depths.clear();
        self.prepared_depths.extend_from_slice(&depths);

        self.rect_commands.clear();
        self.line_commands.clear();
        // scissor_batches is already fresh for this frame (rebuilt by the
        // run-scan above) — no reset needed; the next prepare() call clears
        // and rebuilds it from scratch regardless.
        self.line_batch_scratch = line_batches;

        self.prepared_index_count > 0 || has_text || !self.prepared_image_draws.is_empty()
    }

    /// Render prepared rect and text geometry into `target`.
    /// Must call `prepare()` or `prepare_with_offset()` first.
    pub fn render(
        &self,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        load_action: GpuLoadAction,
    ) {
        encoder.begin_render_pass(target, load_action, "UI Overlay");

        self.render_in_pass(encoder);

        encoder.end_render_pass();
    }

    /// Draw UI rects + text into an already-active render pass.
    /// Used when the caller manages the render pass lifetime (e.g. batching
    /// UI draws with layer bitmap draws into a single pass).
    pub fn render_in_pass(&self, encoder: &mut GpuEncoder) {
        // Issue draws depth by depth: a depth's rects + lines, then its text,
        // so geometry at a higher depth covers text at a lower one.
        for &depth in &self.prepared_depths {
            if self.prepared_index_count > 0 {
                let vbuf = self.vbuf_ring[self.prepared_slot].as_ref().unwrap();
                let ibuf = self.ibuf_ring[self.prepared_slot].as_ref().unwrap();
                let globals = &[GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&self.prepared_globals),
                }];
                let pw = self.prepared_physical_w;
                let ph = self.prepared_physical_h;
                let mut drew = false;
                for batch in self.prepared_batches.iter().filter(|b| b.depth == depth) {
                    if let Some([x, y, w, h]) = batch.scissor {
                        encoder.set_scissor_rect(x, y, w, h);
                    } else {
                        encoder.set_scissor_rect(0, 0, pw, ph);
                    }
                    encoder.draw_in_render_pass(
                        &self.pipeline,
                        globals,
                        vbuf,
                        0,
                        ibuf,
                        batch.index_count,
                        batch.index_offset,
                        None,
                        "UI Rects",
                    );
                    drew = true;
                }
                // Reset scissor to full viewport so text draws unclipped.
                if drew {
                    encoder.set_scissor_rect(0, 0, pw, ph);
                }
            }

            // Static images (PRESET_LIBRARY_DESIGN P6, D7) — after this
            // depth's rects, before its text, so a caption/badge label
            // painted on the same cell stays legible on top of the picture.
            // One draw call per image (a distinct texture each — no atlas),
            // skipped silently if its texture hasn't been registered yet
            // (self-corrects the next frame once the app decodes it).
            if !self.prepared_image_draws.is_empty() {
                let pw = self.prepared_physical_w;
                let ph = self.prepared_physical_h;
                let vbuf = self.image_vbuf_ring[self.prepared_image_slot]
                    .as_ref()
                    .expect("image_vbuf_ring slot populated when prepared_image_draws is non-empty");
                let mut drew_image = false;
                for draw in self.prepared_image_draws.iter().filter(|d| d.depth == depth) {
                    let Some(tex) = self.image_textures.get(&draw.handle) else {
                        continue;
                    };
                    if let Some([x, y, w, h]) = draw.scissor {
                        encoder.set_scissor_rect(x, y, w, h);
                    } else {
                        encoder.set_scissor_rect(0, 0, pw, ph);
                    }
                    encoder.draw_in_render_pass(
                        &self.image_pipeline,
                        &[
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::bytes_of(&self.prepared_globals),
                            },
                            GpuBinding::Texture { binding: 1, texture: tex },
                            GpuBinding::Sampler { binding: 2, sampler: &self.image_sampler },
                        ],
                        vbuf,
                        draw.vertex_offset,
                        &self.image_index_buf,
                        6,
                        0,
                        None,
                        "UI Image",
                    );
                    drew_image = true;
                }
                if drew_image {
                    encoder.set_scissor_rect(0, 0, pw, ph);
                }
            }

            #[cfg(target_os = "macos")]
            self.text_renderer.render_depth_in_pass(encoder, depth);
        }
    }
}

/// BUG-060 trace band: parsed once from `MANIFOLD_BUG060_TRACE=x0,y0,x1,y1`
/// (logical px). `None` when unset — the trace is fully disarmed. Remove with
/// BUG-060.
fn bug060_trace_band() -> Option<[f32; 4]> {
    static BAND: std::sync::OnceLock<Option<[f32; 4]>> = std::sync::OnceLock::new();
    *BAND.get_or_init(|| {
        let v = std::env::var("MANIFOLD_BUG060_TRACE").ok()?;
        let p: Vec<f32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
        (p.len() == 4).then(|| [p[0], p[1], p[2], p[3]])
    })
}

/// Implement TextMeasure for UIRenderer so panels can compute layout.
impl TextMeasure for UIRenderer {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        #[cfg(target_os = "macos")]
        return self
            .text_renderer
            .measure_text(text, font_size, font_weight);
        #[cfg(not(target_os = "macos"))]
        {
            let em = font_size as f32;
            let avg_char_width = match font_weight {
                FontWeight::Bold => em * 0.56,
                FontWeight::Medium => em * 0.54,
                FontWeight::Regular => em * 0.52,
            };
            Vec2::new(text.len() as f32 * avg_char_width, em)
        }
    }
}

/// Implement the immediate-mode `Painter` for `UIRenderer` so the graph canvas
/// and its mapping popover (now in `manifold-ui`) can paint through
/// `&mut dyn Painter` without depending on `manifold-renderer`. Each method
/// forwards to the inherent `UIRenderer` method of the same name (method-call
/// syntax resolves to the inherent one, so there is no recursion); `Depth` maps
/// 1:1 since the two share the same tier constants. See
/// `crates/manifold-ui/src/draw.rs`.
impl manifold_ui::draw::Painter for UIRenderer {
    fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: manifold_ui::Color32) {
        self.draw_rect(x, y, w, h, color);
    }

    fn draw_rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: manifold_ui::Color32, corner: f32) {
        self.draw_rounded_rect(x, y, w, h, color, corner);
    }

    fn draw_bordered_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: manifold_ui::Color32,
        corner: f32,
        border_width: f32,
        border_color: manifold_ui::Color32,
    ) {
        self.draw_bordered_rect(x, y, w, h, color, corner, border_width, border_color);
    }

    fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: manifold_ui::Color32) {
        self.draw_line(x0, y0, x1, y1, thickness, color);
    }

    fn draw_text(&mut self, x: f32, y: f32, text: &str, font_size: f32, color: [u8; 4]) {
        self.draw_text(x, y, text, font_size, color);
    }

    fn draw_image_uv(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        handle: manifold_ui::node::TextureHandle,
        uv: [f32; 4],
        corner: f32,
    ) {
        self.draw_image_uv(x, y, w, h, handle, uv, corner);
    }

    fn push_immediate_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.push_immediate_clip(x, y, w, h);
    }

    fn pop_immediate_clip(&mut self) {
        self.pop_immediate_clip();
    }

    fn push_depth(&mut self, depth: manifold_ui::draw::Depth) {
        self.push_depth(Depth(depth.0));
    }

    fn pop_depth(&mut self) {
        self.pop_depth();
    }

    fn push_transform(&mut self, transform: Affine2) {
        self.push_transform(transform);
    }

    fn pop_transform(&mut self) {
        self.pop_transform();
    }
}

// ── Geometry helpers ────────────────────────────────────────────────────────

/// Intersect two rects (for nested clipping).
fn intersect_rects(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.x_max().min(b.x_max());
    let y1 = a.y_max().min(b.y_max());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// Decode-cache proof (PRESET_LIBRARY_DESIGN P6, D7): `register_image` is
    /// idempotent — the SAME `TextureHandle` uploads a GPU texture only on
    /// its first call, reporting `false` (no-op) on every subsequent one, so
    /// the app-side "decode + register" loop never re-uploads a key it has
    /// already cached (the "decoded once per distinct path" contract). Needs
    /// a real GPU device; run with `--ignored`.
    #[test]
    #[ignore = "needs a real GPU device; run with --ignored"]
    fn register_image_is_idempotent_per_handle() {
        let device = crate::test_device();
        let mut ui = UIRenderer::new(&device, GpuTextureFormat::Rgba8Unorm);

        let handle = manifold_ui::node::texture_handle_for_key("/fake/path/Bloom.png");
        assert!(!ui.has_image(handle), "must start unregistered");

        let rgba = vec![255u8; 4 * 4 * 4]; // 4x4 opaque white
        let first = ui.register_image(&device, handle, 4, 4, &rgba);
        assert!(first, "first registration must upload and report true");
        assert!(ui.has_image(handle));

        let second = ui.register_image(&device, handle, 4, 4, &rgba);
        assert!(!second, "re-registering the same handle must no-op (false)");
        assert!(ui.has_image(handle));

        // A DIFFERENT key/handle registers independently.
        let other_handle = manifold_ui::node::texture_handle_for_key("/fake/path/Glitch.png");
        assert!(!ui.has_image(other_handle));
        assert!(ui.register_image(&device, other_handle, 4, 4, &rgba));
    }

    /// BUG-060 regression: a transform boundary inside a tree traversal must
    /// not strip the tree scissor from rects already enqueued before it.
    /// Before the fix, `push_transform`/`pop_transform` (and the depth
    /// pushes) cut the pending run via a flush-time flush, which batched the
    /// prior TREE rects under `immediate_clip` (`None`) — on the rig, every
    /// effect-card ON pill drawn before the card's rotated chevron escaped
    /// the inspector's column clip, depositing stale copies at the
    /// scroll-viewport edges on every scroll step. Post-refactor, clip is
    /// bound directly on each `RectCommand` at enqueue time (`draw_node`
    /// captures `clip_stack.last()`), so this asserts the invariant on the
    /// commands themselves, before `prepare()` even derives batches from
    /// them — there is no flush step left to get it wrong.
    #[test]
    #[ignore = "needs a real GPU device; run with --ignored"]
    fn transform_boundary_keeps_tree_scissor_on_pending_batch() {
        let device = crate::test_device();
        let mut ui = UIRenderer::new(&device, GpuTextureFormat::Bgra8Unorm);

        let mut tree = UITree::new();
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, 400.0, 300.0),
            manifold_ui::ZTier::Base,
            "bug060-test",
            UIFlags::empty(),
        );
        let content_start = tree.count();
        let clip = Rect::new(10.0, 20.0, 200.0, 100.0);
        let container = tree.add_node(
            None,
            clip,
            UINodeType::Panel,
            UIStyle {
                bg_color: Color32::new(20, 20, 20, 255),
                ..UIStyle::default()
            },
            None,
            UIFlags::CLIPS_CHILDREN,
        );
        // A filled rect (the "pill")…
        tree.add_node(
            Some(container),
            Rect::new(15.0, 25.0, 30.0, 10.0),
            UINodeType::Panel,
            UIStyle {
                bg_color: Color32::new(80, 120, 255, 255),
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );
        // …followed by a TRANSFORMED sibling (the "chevron").
        tree.add_node(
            Some(container),
            Rect::new(60.0, 25.0, 10.0, 10.0),
            UINodeType::Panel,
            UIStyle {
                bg_color: Color32::new(200, 200, 200, 255),
                transform: Some(Affine2::rotate(1.0)),
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );

        tree.end_region(region, content_start);
        ui.render_sub_region(&tree, content_start, tree.count(), false);

        // The pill (x=15) and the chevron (x=60) are children of the clip
        // container: their RectCommands must each carry the container's
        // bounds as their own `clip` — never `None` — regardless of the
        // transform boundary the chevron's push_transform/pop_transform
        // opened and closed between them.
        let mut pill_checked = false;
        let mut chevron_checked = false;
        for rc in &ui.rect_commands {
            if rc.x == 15.0 || rc.x == 60.0 {
                assert_eq!(
                    rc.clip,
                    Some(clip),
                    "child rect at x={} lost the tree scissor (command clip {:?})",
                    rc.x,
                    rc.clip,
                );
                if rc.x == 15.0 {
                    pill_checked = true;
                } else {
                    chevron_checked = true;
                }
            }
        }
        assert!(pill_checked, "sanity: pill rect must appear in rect_commands");
        assert!(chevron_checked, "sanity: transformed rect must appear in rect_commands");
    }
}
