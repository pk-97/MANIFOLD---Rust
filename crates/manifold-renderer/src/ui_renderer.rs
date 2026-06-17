use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice, GpuEncoder,
    GpuLoadAction, GpuRenderPipeline, GpuTexture, GpuTextureFormat, GpuVertexAttribute,
    GpuVertexFormat, GpuVertexLayout,
};

#[cfg(target_os = "macos")]
use crate::native_text::NativeTextRenderer;

use manifold_ui::node::*;
use manifold_ui::text::TextMeasure;
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
}

const UI_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) rect_params: vec4<f32>,
    @location(4) border_color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) rect_params: vec4<f32>,
    @location(3) border_color: vec4<f32>,
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
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let rect_w = in.rect_params.x;
    let rect_h = in.rect_params.y;
    let radius = in.rect_params.z;
    let border_w = in.rect_params.w;

    // If no corner radius, just output solid color (fast path)
    if radius <= 0.0 && border_w <= 0.0 {
        return in.color;
    }

    // SDF rounded rectangle
    let pixel = in.uv * vec2<f32>(rect_w, rect_h);
    let center = vec2<f32>(rect_w, rect_h) * 0.5;
    let half_size = center - vec2<f32>(radius);
    let d = length(max(abs(pixel - center) - half_size, vec2<f32>(0.0))) - radius;

    // Antialiased edge
    let aa = 1.0;
    let alpha = 1.0 - smoothstep(-aa, aa, d);

    if alpha <= 0.0 {
        discard;
    }

    // Border
    if border_w > 0.0 {
        let inner_d = d + border_w;
        if inner_d > 0.0 {
            // In border region
            return vec4<f32>(in.border_color.rgb, in.border_color.a * alpha);
        }
    }

    return vec4<f32>(in.color.rgb, in.color.a * alpha);
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
}

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

/// Initial vertex/index buffer capacities (vertices / indices).
const INITIAL_VERTEX_CAPACITY: usize = 1024;
const INITIAL_INDEX_CAPACITY: usize = 1536;

/// Ring buffer slots for GPU buffers. Each prepare() call uses the next slot.
/// After RING_SIZE prepare() calls the ring wraps around. With ~10 prepare
/// calls per frame (panel cache + sub-regions + overlay) and 3 frames in
/// flight, 32 slots guarantees no aliasing with in-flight GPU work.
const BUF_RING_SIZE: usize = 32;

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
    /// Which ring slot the current prepared data lives in.
    prepared_slot: usize,
    prepared_index_count: u32,
    /// [viewport_w, viewport_h, offset_x, offset_y] — passed as inline uniform.
    prepared_globals: [f32; 4],

    // Clip stack for render_tree — used for text clip_bounds and scissor batching.
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
    // Depth stack for z-ordered drawing. Empty = Depth::BASE. Pushing/popping
    // flushes the pending immediate-mode run so commands on either side
    // land in batches tagged with the correct depth.
    depth_stack: Vec<Depth>,
    // Scissor batches accumulated during tree traversal.
    scissor_batches: Vec<ScissorBatch>,
    // Index into rect_commands where the current batch started.
    current_batch_start: usize,
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
            stride: std::mem::size_of::<UIVertex>() as u32, // 64 bytes
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
            prepared_slot: 0,
            prepared_index_count: 0,
            prepared_globals: [0.0; 4],
            clip_stack: Vec::with_capacity(8),
            immediate_clip: None,
            immediate_clip_stack: Vec::with_capacity(4),
            depth_stack: Vec::with_capacity(4),
            scissor_batches: Vec::with_capacity(8),
            current_batch_start: 0,
            prepared_batches: Vec::with_capacity(8),
            prepared_depths: Vec::with_capacity(8),
            line_batch_scratch: Vec::with_capacity(8),
            prepared_physical_w: 0,
            prepared_physical_h: 0,
        }
    }

    // ── Immediate-mode draw API ─────────────────────────────────────

    /// Clip subsequent immediate-mode draws (rects, lines, AND text) to
    /// `(x, y, w, h)` in logical coordinates, intersected with any enclosing
    /// immediate clip, until the matching [`Self::pop_immediate_clip`]. The
    /// graph canvas pushes its lane so nodes and wires can't bleed under the
    /// side panels. Must be balanced before `prepare` runs.
    pub fn push_immediate_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.flush_immediate_run();
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
        self.flush_immediate_run();
        self.immediate_clip_stack.pop();
        self.immediate_clip = self.immediate_clip_stack.last().copied();
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
        self.flush_immediate_run();
        self.depth_stack.push(depth);
    }

    /// Return to the depth that was active before the matching push.
    pub fn pop_depth(&mut self) {
        debug_assert!(!self.depth_stack.is_empty(), "pop_depth without push");
        self.flush_immediate_run();
        self.depth_stack.pop();
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
            self.text_renderer
                .draw_text(x, y, text, font_size, color, FontWeight::Medium, clip, depth);
        }
    }

    // ── Scissor batch helpers ───────────────────────────────────────
    //
    // Every rect command must be covered by exactly one ScissorBatch by the
    // time `prepare` runs — an uncovered rect sits in the index buffer but is
    // silently skipped. The protocol: `current_batch_start` marks where the
    // pending (not yet batched) run begins. Tree traversals flush the pending
    // immediate-mode run before they start, batch their own emissions on clip
    // boundaries, and flush at the end. Immediate-mode runs are flushed on
    // layer push/pop, on immediate-clip changes, and once at the top of
    // `prepare`. `prepare` resets the marker to 0 after draining the queues,
    // so a stale marker can never underflow a later flush.

    /// Wrap any pending immediate-mode rects (emitted via `draw_*` outside a
    /// tree traversal) into a batch carrying the current immediate clip and
    /// layer.
    fn flush_immediate_run(&mut self) {
        let count = self.rect_commands.len() - self.current_batch_start;
        if count > 0 {
            self.scissor_batches.push(ScissorBatch {
                scissor: self.immediate_clip,
                rect_start: self.current_batch_start,
                rect_count: count,
                depth: self.current_depth(),
            });
        }
        self.current_batch_start = self.rect_commands.len();
    }

    /// Begin scissor batch tracking for a tree traversal: cover any pending
    /// immediate-mode rects first, then reset the clip context. Batches from
    /// earlier traversals in the same prepare cycle are preserved.
    fn begin_scissor_tracking(&mut self) {
        self.flush_immediate_run();
        self.clip_stack.clear();
    }

    /// Flush the current scissor batch (if it has any rects).
    fn flush_scissor_batch(&mut self) {
        let count = self.rect_commands.len() - self.current_batch_start;
        if count > 0 {
            self.scissor_batches.push(ScissorBatch {
                scissor: self.clip_stack.last().copied(),
                rect_start: self.current_batch_start,
                rect_count: count,
                depth: self.current_depth(),
            });
        }
        self.current_batch_start = self.rect_commands.len();
    }

    /// Handle a PushClip event: flush current batch, push clip, start new batch.
    fn handle_push_clip(&mut self, rect: Rect) {
        self.flush_scissor_batch();
        let clipped = if let Some(current) = self.clip_stack.last() {
            intersect_rects(*current, rect)
        } else {
            rect
        };
        self.clip_stack.push(clipped);
    }

    /// Handle a PopClip event: flush current batch, pop clip, start new batch.
    fn handle_pop_clip(&mut self) {
        self.flush_scissor_batch();
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
                    && node.id as usize >= start
                {
                    return;
                }
                self.draw_node(node);
            }
            TraversalEvent::PushClip(rect) => self.handle_push_clip(rect),
            TraversalEvent::PopClip => self.handle_pop_clip(),
        });

        self.flush_scissor_batch();
    }

    /// Render tree nodes in range [start_node, end_node) on the current
    /// layer. Uses `traverse_range` to only walk root nodes in the given
    /// range, avoiding a full-tree traversal per section. Traversals are
    /// additive within a prepare cycle: earlier batches (including pending
    /// immediate-mode draws, which get covered first) are preserved.
    pub fn render_tree_range(&mut self, tree: &UITree, start_node: usize, end_node: usize) {
        self.begin_scissor_tracking();

        tree.traverse_range(start_node, end_node, |event| match event {
            TraversalEvent::Node(node) => self.draw_node(node),
            TraversalEvent::PushClip(rect) => self.handle_push_clip(rect),
            TraversalEvent::PopClip => self.handle_pop_clip(),
        });

        self.flush_scissor_batch();
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

        self.flush_scissor_batch();
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
            });
        }

        // Text (or icon if text starts with PUA marker U+E000..U+E004)
        #[cfg(target_os = "macos")]
        if let Some(text) = &node.text
            && !text.is_empty()
        {
            let clip_bounds = self
                .clip_stack
                .last()
                .map(|c| [c.x, c.y, c.x_max(), c.y_max()]);
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
            let first_char = text.chars().next().unwrap();
            if ('\u{E000}'..='\u{E004}').contains(&first_char) {
                // Icon: square aspect ratio, centered in bounds
                let icon_id = (first_char as u32 - 0xE000) as u8;
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
                    clip_bounds,
                    depth,
                );
            } else {
                let text_size = self.text_renderer.measure_text_cached(
                    text,
                    style.font_size,
                    style.font_weight,
                );
                let text_y = bounds.y + (bounds.height - text_size.y) * 0.5;

                let text_x = match style.text_align {
                    TextAlign::Center => bounds.x + (bounds.width - text_size.x) * 0.5,
                    TextAlign::Right => bounds.x + bounds.width - text_size.x,
                    TextAlign::Left => bounds.x,
                };

                self.text_renderer.draw_text(
                    text_x,
                    text_y,
                    text,
                    style.font_size as f32,
                    text_color,
                    style.font_weight,
                    clip_bounds,
                    depth,
                );
            }
        }
    }

    /// Queue an icon draw. `icon_id` is one of the `ICON_WAVE_*` constants.
    #[cfg(target_os = "macos")]
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
        let depth = self.current_depth();
        self.text_renderer
            .draw_icon(icon_id, x, y, w, h, color.into().0, clip_bounds, depth);
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
        // Cover any trailing immediate-mode rects (e.g. a floating widget
        // drawn after the last tree traversal) so they reach the GPU.
        self.flush_immediate_run();

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

            self.vertices.push(UIVertex {
                position: [x0, y0],
                uv: [0.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y0],
                uv: [1.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y1],
                uv: [1.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x0, y1],
                uv: [0.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });

            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
                self.vertices.push(UIVertex {
                    position: [cmd.x0 + nx, cmd.y0 + ny],
                    uv: [0.0, 0.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                });
                self.vertices.push(UIVertex {
                    position: [cmd.x1 + nx, cmd.y1 + ny],
                    uv: [1.0, 0.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                });
                self.vertices.push(UIVertex {
                    position: [cmd.x1 - nx, cmd.y1 - ny],
                    uv: [1.0, 1.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
                });
                self.vertices.push(UIVertex {
                    position: [cmd.x0 - nx, cmd.y0 - ny],
                    uv: [0.0, 1.0],
                    color: cmd.color,
                    rect_params: zero_params,
                    border_color: zero_border,
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

        // Ring-buffered GPU buffers: each prepare() call advances to the next
        // ring slot, preventing aliasing with in-flight GPU work from previous
        // prepare/commit cycles (both within this frame and across frames).
        // Buffers grow in-place when needed but are never freed.
        if !self.vertices.is_empty() {
            let slot = self.ring_idx % BUF_RING_SIZE;
            self.ring_idx += 1;

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
        self.scissor_batches.clear();
        // Reset the pending-run marker alongside the queues it indexes into —
        // a stale marker would make the next flush underflow into a malformed
        // batch (GPU reads past the index buffer).
        self.current_batch_start = 0;
        self.line_batch_scratch = line_batches;

        self.prepared_index_count > 0 || has_text
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

            #[cfg(target_os = "macos")]
            self.text_renderer.render_depth_in_pass(encoder, depth);
        }
    }
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

// ── Geometry helpers ────────────────────────────────────────────────────────

/// Intersect two rects (for nested clipping).
fn intersect_rects(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.x_max().min(b.x_max());
    let y1 = a.y_max().min(b.y_max());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}
