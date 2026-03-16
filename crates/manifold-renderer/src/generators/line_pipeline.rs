use crate::generators::generator_math::DEFAULT_DOT_RADIUS;

/// Vertex format for CPU-expanded line quads.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub alpha: f32,
    pub _pad: f32,
}

const VERTEX_SIZE: u64 = std::mem::size_of::<LineVertex>() as u64;

/// Maximum vertex count: Duocylinder worst case = 1152 edges * 6 + 576 dots * 6 = 10368
const MAX_VERTICES: u64 = 12288;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineUniforms {
    beat: f32,
    _pad: [f32; 3],
}

/// GPU pipeline for anti-aliased line rendering via CPU-expanded quads.
pub struct LinePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
}

impl LinePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat, label: &str) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{} Line Shader", label)),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/generator_lines.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("{} Line BGL", label)),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{} Line Pipeline Layout", label)),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{} Line Pipeline", label)),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: VERTEX_SIZE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        // position: float2
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        // uv: float2
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        // alpha: float
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 16,
                            shader_location: 2,
                        },
                        // _pad: float
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: 20,
                            shader_location: 3,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
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

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{} Line Uniforms", label)),
            size: std::mem::size_of::<LineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{} Line Vertices", label)),
            size: MAX_VERTICES * VERTEX_SIZE,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            vertex_buffer,
        }
    }

    /// Draw the given vertices as anti-aliased line quads.
    /// Clears the target before drawing.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        vertices: &[LineVertex],
        beat: f32,
    ) {
        if vertices.is_empty() {
            // Still clear the target
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Line Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            return;
        }

        let uniforms = LineUniforms {
            beat,
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let vert_bytes = bytemuck::cast_slice(vertices);
        let upload_size = vert_bytes.len() as u64;
        if upload_size > MAX_VERTICES * VERTEX_SIZE {
            log::warn!("Line vertex count {} exceeds max {}, truncating", vertices.len(), MAX_VERTICES);
        }
        let clamped_size = upload_size.min(MAX_VERTICES * VERTEX_SIZE);
        queue.write_buffer(&self.vertex_buffer, 0, &vert_bytes[..clamped_size as usize]);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Line BG"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.uniform_buffer.as_entire_binding(),
            }],
        });

        let vert_count = (clamped_size / VERTEX_SIZE) as u32;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Line Draw Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..clamped_size));
            pass.draw(0..vert_count, 0..1);
        }
    }
}

// ─── Quad building functions ───

/// Build 6 vertices (2 triangles) for a line segment from A to B.
/// Positions in [0,1] screen space. `half_thick` is in pixels.
#[inline]
pub fn build_edge_quad(
    ax: f32, ay: f32, bx: f32, by: f32,
    half_thick: f32,
    rt_width: f32, rt_height: f32,
    alpha: f32,
) -> [LineVertex; 6] {
    let dx = (bx - ax) * rt_width;
    let dy = (by - ay) * rt_height;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.001 {
        return [LineVertex { position: [0.0; 2], uv: [0.0; 2], alpha: 0.0, _pad: 0.0 }; 6];
    }
    let inv_len = 1.0 / len;
    let perp_x = -dy * inv_len * half_thick / rt_width;
    let perp_y = dx * inv_len * half_thick / rt_height;

    let v0 = LineVertex { position: [ax - perp_x, ay - perp_y], uv: [-1.0, 0.0], alpha, _pad: 0.0 };
    let v1 = LineVertex { position: [ax + perp_x, ay + perp_y], uv: [1.0, 0.0], alpha, _pad: 0.0 };
    let v2 = LineVertex { position: [bx + perp_x, by + perp_y], uv: [1.0, 0.0], alpha, _pad: 0.0 };
    let v3 = LineVertex { position: [bx - perp_x, by - perp_y], uv: [-1.0, 0.0], alpha, _pad: 0.0 };

    // Two triangles: v0,v1,v2 and v0,v2,v3
    [v0, v1, v2, v0, v2, v3]
}

/// Build 6 vertices (2 triangles) for a dot at position (cx, cy).
/// `radius_px` is in pixels.
#[inline]
pub fn build_dot_quad(
    cx: f32, cy: f32,
    radius_px: f32,
    rt_width: f32, rt_height: f32,
    alpha: f32,
) -> [LineVertex; 6] {
    let half_x = radius_px / rt_width;
    let half_y = radius_px / rt_height;

    let v0 = LineVertex { position: [cx - half_x, cy - half_y], uv: [-1.0, -1.0], alpha, _pad: 0.0 };
    let v1 = LineVertex { position: [cx + half_x, cy - half_y], uv: [1.0, -1.0], alpha, _pad: 0.0 };
    let v2 = LineVertex { position: [cx + half_x, cy + half_y], uv: [1.0, 1.0], alpha, _pad: 0.0 };
    let v3 = LineVertex { position: [cx - half_x, cy + half_y], uv: [-1.0, 1.0], alpha, _pad: 0.0 };

    [v0, v1, v2, v0, v2, v3]
}

// ─── LineGeneratorHelper ───

/// Shared helper for line-based generators. Manages projected vertices,
/// edge connectivity, animation state, and vertex buffer assembly.
pub struct LineGeneratorHelper {
    pub projected_x: Vec<f32>,
    pub projected_y: Vec<f32>,
    pub projected_z: Vec<f32>,
    pub edge_a: Vec<usize>,
    pub edge_b: Vec<usize>,
    pub anim_progress: f32,
    vertices: Vec<LineVertex>,
}

impl LineGeneratorHelper {
    pub fn new(vertex_count: usize, edge_count: usize) -> Self {
        Self {
            projected_x: vec![0.0; vertex_count],
            projected_y: vec![0.0; vertex_count],
            projected_z: vec![0.0; vertex_count],
            edge_a: Vec::with_capacity(edge_count),
            edge_b: Vec::with_capacity(edge_count),
            anim_progress: 0.0,
            vertices: Vec::with_capacity((edge_count + vertex_count) * 6),
        }
    }

    /// Resize projected arrays when vertex count changes.
    pub fn resize_vertices(&mut self, count: usize) {
        self.projected_x.resize(count, 0.0);
        self.projected_y.resize(count, 0.0);
        self.projected_z.resize(count, 0.0);
    }

    /// Build the complete vertex array from projected positions and edge connectivity.
    /// Returns a slice of vertices ready for GPU upload.
    ///
    /// `line_thickness`: half-thickness in pixels
    /// `show_verts`: whether to draw vertex dots
    /// `vert_size`: dot radius multiplier
    /// `animate`: whether edge animation is active
    /// `speed`: animation speed multiplier
    /// `window`: animation window (fraction of total edges visible)
    /// `dt`: delta time
    /// `scale`: projection scale multiplier
    pub fn build_vertices(
        &mut self,
        rt_width: f32,
        rt_height: f32,
        line_thickness: f32,
        show_verts: bool,
        vert_size: f32,
        animate: bool,
        speed: f32,
        window: f32,
        dt: f32,
        _scale: f32,
    ) -> &[LineVertex] {
        self.vertices.clear();
        let half_thick = line_thickness * rt_height * 0.5;
        let edge_count = self.edge_a.len();

        if animate && edge_count > 0 {
            self.anim_progress += speed * dt;
            let total = edge_count as f32;
            if self.anim_progress >= total {
                self.anim_progress -= total;
            }
            let win = (window * total).max(1.0);
            for i in 0..edge_count {
                let fi = i as f32;
                let dist = (fi - self.anim_progress).rem_euclid(total);
                let alpha = if dist < win { 1.0 - dist / win } else { 0.0 };
                if alpha > 0.001 {
                    let a = self.edge_a[i];
                    let b = self.edge_b[i];
                    let ax = self.projected_x[a] + 0.5;
                    let ay = self.projected_y[a] + 0.5;
                    let bx = self.projected_x[b] + 0.5;
                    let by = self.projected_y[b] + 0.5;
                    let quad = build_edge_quad(ax, ay, bx, by, half_thick, rt_width, rt_height, alpha);
                    self.vertices.extend_from_slice(&quad);
                }
            }
        } else {
            for i in 0..edge_count {
                let a = self.edge_a[i];
                let b = self.edge_b[i];
                let ax = self.projected_x[a] + 0.5;
                let ay = self.projected_y[a] + 0.5;
                let bx = self.projected_x[b] + 0.5;
                let by = self.projected_y[b] + 0.5;
                let quad = build_edge_quad(ax, ay, bx, by, half_thick, rt_width, rt_height, 1.0);
                self.vertices.extend_from_slice(&quad);
            }
        }

        if show_verts {
            let base_radius = DEFAULT_DOT_RADIUS * rt_height * vert_size;
            let vert_count = self.projected_x.len();
            for i in 0..vert_count {
                let cx = self.projected_x[i] + 0.5;
                let cy = self.projected_y[i] + 0.5;
                let quad = build_dot_quad(cx, cy, base_radius, rt_width, rt_height, 1.0);
                self.vertices.extend_from_slice(&quad);
            }
        }

        &self.vertices
    }
}
