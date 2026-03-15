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
};

@group(0) @binding(0) var<uniform> globals: Globals;

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Convert pixel coordinates to NDC: (0,0) top-left, (w,h) bottom-right
    let ndc_x = (in.position.x / globals.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / globals.screen_size.y) * 2.0;
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

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<UIVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 48,
                    shader_location: 4,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("UI Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
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

        let mut font_system = FontSystem::new();
        let font_data = include_bytes!("../assets/fonts/Inter-Regular.ttf");
        font_system.db_mut().load_font_data(font_data.to_vec());

        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let mut text_atlas = TextAtlas::new(device, queue, &cache, target_format);
        let text_renderer =
            TextRenderer::new(&mut text_atlas, device, wgpu::MultisampleState::default(), None);
        let viewport = Viewport::new(device, &cache);

        Self {
            pipeline,
            globals_buffer,
            globals_bind_group_layout,
            font_system,
            swash_cache,
            text_cache: cache,
            text_atlas,
            text_renderer,
            viewport,
            text_buffers: Vec::new(),
            rect_commands: Vec::with_capacity(256),
            text_commands: Vec::with_capacity(128),
            vertices: Vec::with_capacity(1024),
            indices: Vec::with_capacity(1536),
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
        self.text_commands.push(TextCommand {
            x, y,
            text: text.to_string(),
            font_size,
            color,
            clip_bounds: None,
        });
    }

    // ── UITree rendering ────────────────────────────────────────────

    /// Render a UITree by walking it in DFS order, resolving styles, and
    /// emitting draw commands. Handles clipping mathematically (clamping
    /// child geometry to clip ancestors).
    pub fn render_tree(&mut self, tree: &UITree) {
        self.clip_stack.clear();

        tree.traverse(|event| match event {
            TraversalEvent::Node(node) => {
                self.draw_node(node);
            }
            TraversalEvent::PushClip(rect) => {
                // Intersect with current clip if nested
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
        if let Some(text) = &node.text {
            if !text.is_empty() {
                let text_size = self.measure_text_internal(text, style.font_size, style.font_weight);
                let text_y = bounds.y + (bounds.height - text_size.y) * 0.5;

                let text_x = match style.text_align {
                    TextAlign::Center => bounds.x + (bounds.width - text_size.x) * 0.5,
                    TextAlign::Right => bounds.x + bounds.width - text_size.x,
                    TextAlign::Left => bounds.x,
                };

                let clip_bounds = self.clip_stack.last().map(|c| [c.x, c.y, c.x_max(), c.y_max()]);

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

    /// Internal text measurement using glyphon/cosmic-text.
    fn measure_text_internal(&mut self, text: &str, font_size: u16, _font_weight: FontWeight) -> Vec2 {
        let metrics = Metrics::new(font_size as f32, font_size as f32 * 1.2);
        let mut buffer = TextBuffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(10000.0), Some(font_size as f32 * 2.0));
        buffer.set_text(
            &mut self.font_system,
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut height = 0.0f32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_y + font_size as f32 * 0.2);
        }

        Vec2::new(width, height.max(font_size as f32))
    }

    // ── Render pass ─────────────────────────────────────────────────

    /// Render all queued commands to the target view.
    ///
    /// `width`/`height`: logical pixel dimensions (matches UITree coordinates).
    /// `scale_factor`: HiDPI scale (e.g. 2.0 on Retina). Used for crisp text.
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

        // Update globals — logical pixel dimensions for NDC mapping
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

        // Build vertex/index buffers from rect commands
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

        // Prepare text buffers for glyphon
        self.text_buffers.clear();
        let mut text_areas = Vec::new();

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

        let sf = scale_factor as f32;
        for (i, cmd) in self.text_commands.iter().enumerate() {
            // TextArea positions and bounds must be in physical pixels
            // because the viewport is set to physical resolution.
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

        // Update viewport — physical resolution for crisp text
        self.viewport.update(queue, Resolution { width: physical_w, height: physical_h });

        self.text_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.text_atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("Failed to prepare text renderer");

        if self.vertices.is_empty() && self.text_commands.is_empty() {
            self.rect_commands.clear();
            self.text_commands.clear();
            return;
        }

        // Render pass — rects then text
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

/// Implement TextMeasure for UIRenderer so panels can compute layout.
impl TextMeasure for UIRenderer {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        // TextMeasure requires &self, but glyphon needs &mut FontSystem.
        // Use an approximate measurement: Inter is ~0.5em per character on average.
        // This is good enough for layout; exact measurement happens in draw_node.
        let _ = font_weight;
        let em = font_size as f32;
        let avg_char_width = em * 0.52; // Inter average glyph width
        let width = text.len() as f32 * avg_char_width;
        Vec2::new(width, em)
    }
}

// ── Geometry helpers ────────────────────────────────────────────────

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
fn clamp_rect_to_clip(rect: Rect, clip: Rect) -> Rect {
    intersect_rects(rect, clip)
}
