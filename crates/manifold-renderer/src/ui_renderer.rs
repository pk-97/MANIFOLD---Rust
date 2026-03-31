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

/// Initial vertex/index buffer capacities (vertices / indices).
const INITIAL_VERTEX_CAPACITY: usize = 1024;
const INITIAL_INDEX_CAPACITY: usize = 1536;

/// Simple batched 2D UI renderer using native Metal via manifold-gpu.
pub struct UIRenderer {
    pipeline: GpuRenderPipeline,

    // Text rendering — CoreText renderer.
    #[cfg(target_os = "macos")]
    text_renderer: NativeTextRenderer,

    // Rect draw queue.
    rect_commands: Vec<RectCommand>,

    // Per-frame vertex/index scratch (CPU side).
    vertices: Vec<UIVertex>,
    indices: Vec<u32>,

    // Fresh GpuBuffers created each prepare() call — avoids aliasing with in-flight GPU work.
    prepared_vertex_buf: Option<GpuBuffer>,
    prepared_index_buf: Option<GpuBuffer>,
    prepared_index_count: u32,
    /// [viewport_w, viewport_h, offset_x, offset_y] — passed as inline uniform.
    prepared_globals: [f32; 4],

    // Clip stack for render_tree (mathematical clipping).
    clip_stack: Vec<Rect>,
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
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 0, shader_location: 0 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 8, shader_location: 1 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 16, shader_location: 2 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 32, shader_location: 3 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 48, shader_location: 4 },
            ],
        };
        let pipeline = device.create_render_pipeline_with_vertex_layout(
            UI_SHADER, "vs_main", "fs_main", format, Some(blend), &layout, "UI Pipeline",
        );

        #[cfg(target_os = "macos")]
        let text_renderer = NativeTextRenderer::new(device, format);

        Self {
            pipeline,
            #[cfg(target_os = "macos")]
            text_renderer,
            rect_commands: Vec::with_capacity(256),
            vertices: Vec::with_capacity(INITIAL_VERTEX_CAPACITY),
            indices: Vec::with_capacity(INITIAL_INDEX_CAPACITY),
            prepared_vertex_buf: None,
            prepared_index_buf: None,
            prepared_index_count: 0,
            prepared_globals: [0.0; 4],
            clip_stack: Vec::with_capacity(8),
        }
    }

    // ── Immediate-mode draw API ─────────────────────────────────────

    /// Queue a filled rectangle.
    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color,
            corner_radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }

    /// Queue a rounded rectangle.
    pub fn draw_rounded_rect(
        &mut self,
        x: f32, y: f32, w: f32, h: f32,
        color: [f32; 4],
        corner_radius: f32,
    ) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color, corner_radius,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }

    /// Queue a rounded rectangle with border.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_bordered_rect(
        &mut self,
        x: f32, y: f32, w: f32, h: f32,
        color: [f32; 4],
        corner_radius: f32,
        border_width: f32,
        border_color: [f32; 4],
    ) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color, corner_radius, border_width, border_color,
        });
    }

    /// Queue text at a position.
    pub fn draw_text(
        &mut self,
        x: f32, y: f32,
        text: &str,
        font_size: f32,
        color: [u8; 4],
    ) {
        #[cfg(target_os = "macos")]
        self.text_renderer.draw_text(x, y, text, font_size, color, FontWeight::Medium, None);
    }

    // ── UITree rendering ────────────────────────────────────────────

    /// Render a UITree. When `skip_from` is `Some(n)`, nodes with
    /// `id >= n` are skipped (used to exclude dropdown overlay nodes
    /// that render in a separate pass via `render_overlay`).
    pub fn render_tree(&mut self, tree: &UITree, skip_from: Option<usize>) {
        self.clip_stack.clear();

        tree.traverse(|event| match event {
            TraversalEvent::Node(node) => {
                if let Some(start) = skip_from
                    && node.id as usize >= start {
                        return;
                    }
                self.draw_node(node);
            }
            TraversalEvent::PushClip(rect) => {
                let clipped = if let Some(current) = self.clip_stack.last() {
                    intersect_rects(*current, rect)
                } else {
                    rect
                };
                self.clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                self.clip_stack.pop();
            }
        });
    }

    /// Render only the overlay/dropdown nodes (from `start_node` onwards).
    /// Call this AFTER layer bitmaps and playhead so the dropdown sits on top.
    pub fn render_overlay(&mut self, tree: &UITree, start_node: usize) {
        self.render_overlay_range(tree, start_node, usize::MAX);
    }

    /// Render nodes in range [start_node, end_node).
    /// Used for rendering specific overlay sections (e.g. perf HUD between
    /// bitmap textures and dropdown popups).
    pub fn render_overlay_range(&mut self, tree: &UITree, start_node: usize, end_node: usize) {
        self.clip_stack.clear();

        tree.traverse(|event| match event {
            TraversalEvent::Node(node) => {
                let id = node.id as usize;
                if id >= start_node && id < end_node {
                    self.draw_node(node);
                }
            }
            TraversalEvent::PushClip(rect) => {
                let clipped = if let Some(current) = self.clip_stack.last() {
                    intersect_rects(*current, rect)
                } else {
                    rect
                };
                self.clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                self.clip_stack.pop();
            }
        });
    }

    /// Draw a single UI node — resolves effective colors and emits commands.
    fn draw_node(&mut self, node: &UINode) {
        let style = &node.style;
        let bounds = if let Some(clip) = self.clip_stack.last() {
            clamp_rect_to_clip(node.bounds, *clip)
        } else {
            node.bounds
        };

        // Skip zero-area rects
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
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

        // Text
        #[cfg(target_os = "macos")]
        if let Some(text) = &node.text
            && !text.is_empty() {
                let text_size = self.text_renderer.measure_text_cached(
                    text, style.font_size, style.font_weight,
                );
                let text_y = bounds.y + (bounds.height - text_size.y) * 0.5;

                let text_x = match style.text_align {
                    TextAlign::Center => bounds.x + (bounds.width - text_size.x) * 0.5,
                    TextAlign::Right => bounds.x + bounds.width - text_size.x,
                    TextAlign::Left => bounds.x,
                };

                let clip_bounds = self.clip_stack.last().map(|c| [c.x, c.y, c.x_max(), c.y_max()]);

                let text_color = if disabled {
                    [style.text_color.r, style.text_color.g, style.text_color.b, style.text_color.a / 3]
                } else {
                    [style.text_color.r, style.text_color.g, style.text_color.b, style.text_color.a]
                };
                self.text_renderer.draw_text(
                    text_x, text_y, text, style.font_size as f32,
                    text_color, style.font_weight, clip_bounds,
                );
            }
    }

    /// Text measurement using NativeTextRenderer's cached measurement.
    pub fn measure_text_cached(&mut self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        #[cfg(target_os = "macos")]
        return self.text_renderer.measure_text_cached(text, font_size, font_weight);
        #[cfg(not(target_os = "macos"))]
        {
            let em = font_size as f32;
            Vec2::new(text.len() as f32 * em * 0.54, em)
        }
    }

    // ── Render pass ─────────────────────────────────────────────────

    /// Advance text renderer frame counter (call once per frame).
    pub fn begin_frame(&mut self) {
        #[cfg(target_os = "macos")]
        self.text_renderer.begin_frame();
    }

    /// Render a range of tree nodes to draw commands.
    /// Equivalent to `render_overlay_range` but named for panel cache usage.
    pub fn render_tree_range(&mut self, tree: &UITree, start: usize, end: usize) {
        self.render_overlay_range(tree, start, end);
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
        self.prepared_globals = [viewport_w as f32, viewport_h as f32, offset_x, offset_y];

        // Build vertex/index data from rect commands.
        self.vertices.clear();
        self.indices.clear();

        for cmd in &self.rect_commands {
            let base = self.vertices.len() as u32;

            let (x0, y0) = (cmd.x, cmd.y);
            let (x1, y1) = (cmd.x + cmd.w, cmd.y + cmd.h);

            self.vertices.push(UIVertex {
                position: [x0, y0], uv: [0.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y0], uv: [1.0, 0.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y1], uv: [1.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x0, y1], uv: [0.0, 1.0],
                color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });

            self.indices.extend_from_slice(&[
                base, base + 1, base + 2,
                base, base + 2, base + 3,
            ]);
        }

        // Create fresh GpuBuffers each prepare() — avoids aliasing with in-flight GPU work.
        if !self.vertices.is_empty() {
            let vdata = bytemuck::cast_slice::<UIVertex, u8>(&self.vertices);
            let vbuf = device.create_buffer_shared(vdata.len() as u64);
            unsafe { vbuf.write(0, vdata); }

            let idata = bytemuck::cast_slice::<u32, u8>(&self.indices);
            let ibuf = device.create_buffer_shared(idata.len() as u64);
            unsafe { ibuf.write(0, idata); }

            self.prepared_vertex_buf = Some(vbuf);
            self.prepared_index_buf = Some(ibuf);
            self.prepared_index_count = self.indices.len() as u32;
        } else {
            self.prepared_vertex_buf = None;
            self.prepared_index_buf = None;
            self.prepared_index_count = 0;
        }

        // Prepare text.
        #[cfg(target_os = "macos")]
        let has_text = self.text_renderer.prepare(
            device, viewport_w, viewport_h, offset_x, offset_y, scale_factor,
        );
        #[cfg(not(target_os = "macos"))]
        let has_text = false;

        self.rect_commands.clear();

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
        if self.prepared_index_count > 0 {
            encoder.draw_indexed(
                &self.pipeline,
                target,
                &[GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&self.prepared_globals),
                }],
                self.prepared_vertex_buf.as_ref().unwrap(),
                self.prepared_index_buf.as_ref().unwrap(),
                self.prepared_index_count,
                None,
                load_action,
                "UI Rects",
            );
        }

        // Text always uses Load to preserve rects drawn above.
        #[cfg(target_os = "macos")]
        self.text_renderer.render(encoder, target, GpuLoadAction::Load);
    }
}

/// Implement TextMeasure for UIRenderer so panels can compute layout.
impl TextMeasure for UIRenderer {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        #[cfg(target_os = "macos")]
        return self.text_renderer.measure_text(text, font_size, font_weight);
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

/// Clamp a rect to a clip region (mathematical clipping).
/// Fixes the Unity "ClipsChildren broken" bug by clamping geometry instead
/// of relying on a flat-loop push/pop.
fn clamp_rect_to_clip(r: Rect, clip: Rect) -> Rect {
    let x0 = r.x.max(clip.x);
    let y0 = r.y.max(clip.y);
    let x1 = r.x_max().min(clip.x_max());
    let y1 = r.y_max().min(clip.y_max());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}
