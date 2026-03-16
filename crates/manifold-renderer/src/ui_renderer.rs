use wgpu::util::DeviceExt;

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as GlyphonColor, Family, FontSystem, Metrics,
    Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

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
    screen_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Map logical pixel position to NDC: (0,0) = top-left, (w,h) = bottom-right
    let ndc = vec2<f32>(
        in.position.x / globals.screen_size.x * 2.0 - 1.0,
        1.0 - in.position.y / globals.screen_size.y * 2.0,
    );
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.rect_params = in.rect_params;
    out.border_color = in.border_color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let w = in.rect_params.x;
    let h = in.rect_params.y;
    let radius = in.rect_params.z;
    let border_w = in.rect_params.w;

    // SDF for rounded rectangle (distance from edge)
    let half_size = vec2<f32>(w * 0.5, h * 0.5);
    let p = (in.uv - 0.5) * vec2<f32>(w, h);
    let d = length(max(abs(p) - half_size + vec2<f32>(radius), vec2<f32>(0.0))) - radius;

    // Anti-aliased edge
    let alpha = 1.0 - smoothstep(-1.0, 0.5, d);

    // Border
    if border_w > 0.0 {
        let inner_d = length(max(abs(p) - half_size + vec2<f32>(radius + border_w), vec2<f32>(0.0))) - radius;
        let border_alpha = smoothstep(-1.0, 0.5, inner_d);
        let mixed = mix(in.color, in.border_color, border_alpha);
        return vec4<f32>(mixed.rgb, mixed.a * alpha);
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

/// Queued text command.
struct TextCommand {
    x: f32,
    y: f32,
    text: String,
    font_size: f32,
    color: [u8; 4],
    /// Clip bounds for this text (None = full viewport).
    clip_bounds: Option<[f32; 4]>,
}

/// Simple batched 2D UI renderer for wgpu.
pub struct UIRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind_group_layout: wgpu::BindGroupLayout,

    // Text rendering
    font_system: FontSystem,
    swash_cache: SwashCache,
    #[allow(dead_code)]
    text_cache: Cache,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    text_buffers: Vec<TextBuffer>,

    // Draw queues
    rect_commands: Vec<RectCommand>,
    text_commands: Vec<TextCommand>,

    // Per-frame vertex buffer
    vertices: Vec<UIVertex>,
    indices: Vec<u32>,

    // Clip stack for render_tree (mathematical clipping)
    clip_stack: Vec<Rect>,

    /// When set, base text overlapping this rect is hidden.
    /// Set by render_tree_with_overlay when a dropdown is open.
    overlay_occlude_rect: Option<Rect>,
    /// Node index where overlay starts (text after this is NOT occluded).
    overlay_start_node: Option<usize>,
    /// Whether we've passed the overlay start during traversal.
    in_overlay: bool,
}

impl UIRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("UI Shader"),
            source: wgpu::ShaderSource::Wgsl(UI_SHADER.into()),
        });

        let globals_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("UI Globals BGL"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("UI Pipeline Layout"),
            bind_group_layouts: &[&globals_bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("UI Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UIVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 8, shader_location: 1, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 16, shader_location: 2, format: wgpu::VertexFormat::Float32x4 },
                        wgpu::VertexAttribute { offset: 32, shader_location: 3, format: wgpu::VertexFormat::Float32x4 },
                        wgpu::VertexAttribute { offset: 48, shader_location: 4, format: wgpu::VertexFormat::Float32x4 },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UI Globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Font setup
        let mut font_system = FontSystem::new();
        let font_data = include_bytes!("../assets/fonts/Inter-Regular.ttf");
        font_system.db_mut().load_font_data(font_data.to_vec());

        let swash_cache = SwashCache::new();
        let text_cache = Cache::new(device);
        let mut text_atlas = TextAtlas::new(device, queue, &text_cache, target_format);
        let text_renderer =
            TextRenderer::new(&mut text_atlas, device, wgpu::MultisampleState::default(), None);
        let viewport = Viewport::new(device, &text_cache);

        Self {
            pipeline,
            globals_buffer,
            globals_bind_group_layout,
            font_system,
            swash_cache,
            text_cache,
            text_atlas,
            text_renderer,
            viewport,
            text_buffers: Vec::new(),
            rect_commands: Vec::with_capacity(256),
            text_commands: Vec::with_capacity(128),
            vertices: Vec::with_capacity(1024),
            indices: Vec::with_capacity(1536),
            clip_stack: Vec::with_capacity(8),
            overlay_occlude_rect: None,
            overlay_start_node: None,
            in_overlay: false,
        }
    }

    // ── Immediate-mode drawing API ───────────────────────────────────

    pub fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color,
            corner_radius: 0.0, border_width: 0.0, border_color: [0.0; 4],
        });
    }

    pub fn draw_rect_rounded(
        &mut self, x: f32, y: f32, w: f32, h: f32,
        color: [f32; 4], corner_radius: f32,
    ) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color, corner_radius,
            border_width: 0.0, border_color: [0.0; 4],
        });
    }

    pub fn draw_rect_bordered(
        &mut self, x: f32, y: f32, w: f32, h: f32,
        color: [f32; 4], corner_radius: f32, border_width: f32, border_color: [f32; 4],
    ) {
        self.rect_commands.push(RectCommand {
            x, y, w, h, color, corner_radius, border_width, border_color,
        });
    }

    // ── UITree rendering ────────────────────────────────────────────

    /// Render a UITree (single pass, no overlay).
    pub fn render_tree(&mut self, tree: &UITree) {
        self.render_tree_with_overlay(tree, None, None);
    }

    /// Render with optional overlay. When a dropdown is open:
    /// - `overlay_start_node`: tree node index where dropdown nodes begin
    /// - `overlay_bounds`: the dropdown container rect
    ///
    /// Base text that overlaps `overlay_bounds` is hidden (clip_bounds set
    /// to exclude the dropdown area). Dropdown text renders normally.
    /// ALL rects render in a single pass (dropdown rects on top by insertion order).
    pub fn render_tree_with_overlay(
        &mut self,
        tree: &UITree,
        overlay_start_node: Option<usize>,
        overlay_bounds: Option<Rect>,
    ) {
        self.clip_stack.clear();
        self.overlay_occlude_rect = overlay_bounds;
        self.overlay_start_node = overlay_start_node;
        self.in_overlay = overlay_start_node.is_none(); // if no overlay, never occlude

        tree.traverse(|event| match event {
            TraversalEvent::Node(node) => {
                // Track when we enter the overlay region
                if !self.in_overlay {
                    if let Some(start) = self.overlay_start_node {
                        if node.id as usize >= start {
                            self.in_overlay = true;
                        }
                    }
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

        // Background
        if bg_color.a > 0 {
            let color = bg_color.to_f32();
            if style.border_width > 0.0 && style.border_color.a > 0 {
                self.rect_commands.push(RectCommand {
                    x: bounds.x, y: bounds.y,
                    w: bounds.width, h: bounds.height,
                    color,
                    corner_radius: style.corner_radius,
                    border_width: style.border_width,
                    border_color: style.border_color.to_f32(),
                });
            } else if style.corner_radius > 0.0 {
                self.rect_commands.push(RectCommand {
                    x: bounds.x, y: bounds.y,
                    w: bounds.width, h: bounds.height,
                    color,
                    corner_radius: style.corner_radius,
                    border_width: 0.0, border_color: [0.0; 4],
                });
            } else {
                self.rect_commands.push(RectCommand {
                    x: bounds.x, y: bounds.y,
                    w: bounds.width, h: bounds.height,
                    color,
                    corner_radius: 0.0, border_width: 0.0, border_color: [0.0; 4],
                });
            }
        } else if style.border_width > 0.0 && style.border_color.a > 0 {
            // Border-only (no fill)
            self.rect_commands.push(RectCommand {
                x: bounds.x, y: bounds.y,
                w: bounds.width, h: bounds.height,
                color: [0.0, 0.0, 0.0, 0.0],
                corner_radius: style.corner_radius,
                border_width: style.border_width,
                border_color: style.border_color.to_f32(),
            });
        }

        // Text
        if let Some(text) = &node.text {
            if !text.is_empty() {
                let text_size = self.measure_text_internal(text, style.font_size, style.font_weight as u16);
                let text_y = bounds.y + (bounds.height - text_size.y) * 0.5;

                let text_x = match style.text_align {
                    TextAlign::Center => bounds.x + (bounds.width - text_size.x) * 0.5,
                    TextAlign::Right => bounds.x + bounds.width - text_size.x,
                    TextAlign::Left => bounds.x,
                };

                let mut clip_bounds = self.clip_stack.last().map(|c| [c.x, c.y, c.x_max(), c.y_max()]);

                // If this is a base (non-overlay) text and it overlaps the dropdown,
                // hide it by setting clip_bounds to a zero-area rect.
                if !self.in_overlay {
                    if let Some(ref occlude) = self.overlay_occlude_rect {
                        let text_rect = Rect::new(text_x, text_y, text_size.x, text_size.y);
                        if rects_overlap(&text_rect, occlude) {
                            // Hide this text — it would show through the dropdown
                            clip_bounds = Some([0.0, 0.0, 0.0, 0.0]);
                        }
                    }
                }

                self.text_commands.push(TextCommand {
                    x: text_x,
                    y: text_y,
                    text: text.clone(),
                    font_size: style.font_size as f32,
                    color: [style.text_color.r, style.text_color.g, style.text_color.b, style.text_color.a],
                    clip_bounds,
                });
            }
        }
    }

    // ── Text measurement ─────────────────────────────────────────────

    fn measure_text_internal(&mut self, text: &str, font_size: u16, _weight: u16) -> Vec2 {
        let em = font_size as f32;
        let approx_width = text.len() as f32 * em * 0.52;
        let height = em * 1.2;
        Vec2::new(approx_width, height)
    }

    // ── GPU Rendering ────────────────────────────────────────────────

    /// Render all queued commands to the target view.
    ///
    /// Single pass: all rects (in insertion order), then all text.
    /// Dropdown rects render on top of panel rects (added last to tree).
    /// Base text behind the dropdown is hidden via zero-area clip_bounds.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
        scale_factor: f64,
    ) {
        let physical_w = (width as f64 * scale_factor) as u32;
        let physical_h = (height as f64 * scale_factor) as u32;

        // Update globals
        let globals_data: [f32; 4] = [width as f32, height as f32, 0.0, 0.0];
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals_data));

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("UI Globals BG"),
            layout: &self.globals_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.globals_buffer.as_entire_binding(),
            }],
        });

        // Build vertices from ALL rect commands (single buffer, single draw)
        self.vertices.clear();
        self.indices.clear();

        for cmd in &self.rect_commands {
            let base = self.vertices.len() as u32;
            let (x0, y0) = (cmd.x, cmd.y);
            let (x1, y1) = (cmd.x + cmd.w, cmd.y + cmd.h);

            self.vertices.push(UIVertex {
                position: [x0, y0], uv: [0.0, 0.0], color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y0], uv: [1.0, 0.0], color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x1, y1], uv: [1.0, 1.0], color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });
            self.vertices.push(UIVertex {
                position: [x0, y1], uv: [0.0, 1.0], color: cmd.color,
                rect_params: [cmd.w, cmd.h, cmd.corner_radius, cmd.border_width],
                border_color: cmd.border_color,
            });

            self.indices.extend_from_slice(&[
                base, base + 1, base + 2,
                base, base + 2, base + 3,
            ]);
        }

        // Prepare ALL text (single glyphon prepare call)
        self.text_buffers.clear();
        let mut text_areas = Vec::new();
        let sf = scale_factor as f32;

        for cmd in &self.text_commands {
            let mut buffer = TextBuffer::new(
                &mut self.font_system,
                Metrics::new(cmd.font_size, cmd.font_size * 1.2),
            );
            buffer.set_size(&mut self.font_system, Some(width as f32), Some(height as f32));
            buffer.set_text(
                &mut self.font_system,
                &cmd.text,
                &Attrs::new().family(Family::SansSerif),
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            self.text_buffers.push(buffer);
        }

        for (i, cmd) in self.text_commands.iter().enumerate() {
            let bounds = if let Some(clip) = cmd.clip_bounds {
                TextBounds {
                    left: (clip[0] * sf) as i32,
                    top: (clip[1] * sf) as i32,
                    right: (clip[2] * sf) as i32,
                    bottom: (clip[3] * sf) as i32,
                }
            } else {
                TextBounds {
                    left: 0,
                    top: 0,
                    right: physical_w as i32,
                    bottom: physical_h as i32,
                }
            };

            text_areas.push(TextArea {
                buffer: &self.text_buffers[i],
                left: cmd.x * sf,
                top: cmd.y * sf,
                scale: sf,
                bounds,
                default_color: GlyphonColor::rgba(cmd.color[0], cmd.color[1], cmd.color[2], cmd.color[3]),
                custom_glyphs: &[],
            });
        }

        self.viewport.update(queue, Resolution { width: physical_w, height: physical_h });

        self.text_renderer
            .prepare(
                device, queue, &mut self.font_system, &mut self.text_atlas,
                &self.viewport, text_areas, &mut self.swash_cache,
            )
            .expect("Failed to prepare text renderer");

        if self.vertices.is_empty() && self.text_commands.is_empty() {
            self.rect_commands.clear();
            self.text_commands.clear();
            return;
        }

        // Single render pass: all rects then all text
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if !self.vertices.is_empty() {
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("UI Vertices"),
                    contents: bytemuck::cast_slice(&self.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("UI Indices"),
                    contents: bytemuck::cast_slice(&self.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });

                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &globals_bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.indices.len() as u32, 0, 0..1);
            }

            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .expect("Failed to render text");
        }

        self.rect_commands.clear();
        self.text_commands.clear();
    }
}

impl TextMeasure for UIRenderer {
    fn measure_text(&self, text: &str, font_size: u16, _font_weight: FontWeight) -> Vec2 {
        let em = font_size as f32;
        let approx_width = text.len() as f32 * em * 0.52;
        let height = em * 1.2;
        Vec2::new(approx_width, height)
    }
}

// ── Geometry helpers ─────────────────────────────────────────────────

fn clamp_rect_to_clip(rect: Rect, clip: Rect) -> Rect {
    let x = rect.x.max(clip.x);
    let y = rect.y.max(clip.y);
    let right = (rect.x + rect.width).min(clip.x + clip.width);
    let bottom = (rect.y + rect.height).min(clip.y + clip.height);
    Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
}

fn intersect_rects(a: Rect, b: Rect) -> Rect {
    clamp_rect_to_clip(b, a)
}

fn rects_overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && a.x + a.width > b.x &&
    a.y < b.y + b.height && a.y + a.height > b.y
}
