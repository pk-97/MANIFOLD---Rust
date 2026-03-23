use crate::generators::generator_math::DEFAULT_DOT_RADIUS;

/// Per-instance edge data uploaded to the GPU storage buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EdgeInstance {
    pub a: u32,
    pub b: u32,
    pub alpha_bits: u32,
    pub _pad: u32,
}

/// Maximum number of projected vertex positions (Duocylinder = 576).
const MAX_POSITIONS: u64 = 1024;
/// Maximum instances (edges + dots). Duocylinder = 1152 edges + 576 dots = 1728.
const MAX_INSTANCES: u64 = 2048;

const POSITION_STRIDE: u64 = 8; // vec2<f32>
const INSTANCE_STRIDE: u64 = 16; // EdgeInstance

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineUniforms {
    rt_width: f32,
    rt_height: f32,
    edge_half_thick: f32,
    beat: f32,
    dot_half_thick: f32,
    num_edges: u32,
    _pad: [f32; 2],
}

/// GPU pipeline for instanced anti-aliased line rendering with capsule SDF.
pub struct LinePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    positions_buffer: wgpu::Buffer,
    instances_buffer: wgpu::Buffer,
}

impl LinePipeline {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat, label: &str) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} Line Shader")),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/generator_lines.wgsl").into(),
            ),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(&format!("{label} Line BGL")),
                entries: &[
                    // Uniforms
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Positions storage buffer
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Edges/instances storage buffer
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} Line Pipeline Layout")),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{label} Line Pipeline")),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[], // No vertex buffers — all data from storage
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        // Max blend: overlapping round caps take the brighter
                        // value instead of accumulating, preventing visible
                        // bright dots at shared vertices.
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Max,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Max,
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
            label: Some(&format!("{label} Line Uniforms")),
            size: std::mem::size_of::<LineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let positions_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Line Positions")),
            size: MAX_POSITIONS * POSITION_STRIDE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let instances_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Line Instances")),
            size: MAX_INSTANCES * INSTANCE_STRIDE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            positions_buffer,
            instances_buffer,
        }
    }

    /// Draw line edges and dots via instanced rendering.
    ///
    /// `positions`: screen-space [0,1] vertex positions (aspect-corrected).
    /// `instances`: edge + dot instance data (dots appended after edges).
    /// `num_edges`: how many of the instances are edges (rest are dots).
    /// `edge_half_thick` / `dot_half_thick`: half-thickness in pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        positions: &[[f32; 2]],
        instances: &[EdgeInstance],
        num_edges: u32,
        edge_half_thick: f32,
        dot_half_thick: f32,
        beat: f32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
        profiler_label: &str,
        width: u32,
        height: u32,
    ) {
        if instances.is_empty() {
            // Still clear the target
            let ts =
                profiler.and_then(|p| p.render_timestamps(profiler_label, width, height));
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
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            return;
        }

        // Upload uniforms
        let uniforms = LineUniforms {
            rt_width: width as f32,
            rt_height: height as f32,
            edge_half_thick,
            beat,
            dot_half_thick,
            num_edges,
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // Upload positions
        let pos_bytes = bytemuck::cast_slice(positions);
        let pos_limit = (MAX_POSITIONS * POSITION_STRIDE) as usize;
        queue.write_buffer(
            &self.positions_buffer,
            0,
            &pos_bytes[..pos_bytes.len().min(pos_limit)],
        );

        // Upload instances
        let inst_bytes = bytemuck::cast_slice(instances);
        let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
        queue.write_buffer(
            &self.instances_buffer,
            0,
            &inst_bytes[..inst_bytes.len().min(inst_limit)],
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Line BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.positions_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.instances_buffer.as_entire_binding(),
                },
            ],
        });

        let instance_count = (instances.len() as u64).min(MAX_INSTANCES) as u32;
        {
            let ts =
                profiler.and_then(|p| p.render_timestamps(profiler_label, width, height));
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
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..instance_count);
        }
    }
}

// ─── LineGeneratorHelper ───

/// Shared helper for line-based generators. Manages projected vertices,
/// edge connectivity, animation state, and produces GPU-ready instance data.
pub struct LineGeneratorHelper {
    pub projected_x: Vec<f32>,
    pub projected_y: Vec<f32>,
    pub projected_z: Vec<f32>,
    pub edge_a: Vec<usize>,
    pub edge_b: Vec<usize>,
    pub anim_progress: f32,
    // GPU upload data
    positions: Vec<[f32; 2]>,
    instances: Vec<EdgeInstance>,
    // Depth sorting scratch buffers (Unity: LineMeshUtil.edgeDepth/edgeSortedIdx)
    edge_depth: Vec<f32>,
    edge_sorted_idx: Vec<usize>,
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
            positions: Vec::with_capacity(vertex_count),
            instances: Vec::with_capacity(edge_count + vertex_count),
            edge_depth: vec![0.0; edge_count],
            edge_sorted_idx: vec![0; edge_count],
        }
    }

    /// Resize projected arrays when vertex count changes.
    pub fn resize_vertices(&mut self, count: usize) {
        self.projected_x.resize(count, 0.0);
        self.projected_y.resize(count, 0.0);
        self.projected_z.resize(count, 0.0);
    }

    /// Prepare instance data for GPU upload. Returns (positions, instances, num_edges,
    /// edge_half_thick_px, dot_half_thick_px).
    ///
    /// Positions are in [0,1] screen-space with aspect correction applied.
    /// Instances contain edges first, then dots (if show_verts).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_instances(
        &mut self,
        rt_height: f32,
        aspect: f32,
        line_thickness: f32,
        show_verts: bool,
        vert_size: f32,
        animate: bool,
        speed: f32,
        window: f32,
        scale: f32,
        dot_scale: f32,
    ) -> (&[[f32; 2]], &[EdgeInstance], u32, f32, f32) {
        let vert_count = self.projected_x.len();
        let edge_count = self.edge_a.len();
        let s = if scale <= 0.0 { 1.0 } else { scale };

        // Build screen-space positions (aspect-corrected, in [0,1])
        self.positions.clear();
        for i in 0..vert_count {
            self.positions.push([
                self.projected_x[i] * s / aspect + 0.5,
                self.projected_y[i] * s + 0.5,
            ]);
        }

        // Build edge instances
        self.instances.clear();
        let edge_half_thick = line_thickness * rt_height * 0.5;

        if animate && edge_count > 0 {
            // Depth sort edges back-to-front (Unity: LineMeshUtil.BuildEdgeQuads)
            self.ensure_sort_buffers(edge_count);
            for i in 0..edge_count {
                let a = self.edge_a[i];
                let b = self.edge_b[i];
                self.edge_depth[i] =
                    (self.projected_z[a] + self.projected_z[b]) * 0.5;
                self.edge_sorted_idx[i] = i;
            }
            let depths = &self.edge_depth;
            self.edge_sorted_idx[..edge_count].sort_by(|&a, &b| {
                depths[a]
                    .partial_cmp(&depths[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            self.anim_progress += speed * (edge_count as f32 / 100.0);
            let total = edge_count as f32;
            if self.anim_progress >= total {
                self.anim_progress -= total;
            }
            let window_edges =
                ((edge_count as f32 * window).ceil() as usize).max(1);
            let window_start = (self.anim_progress
                / (edge_count as f32 / 100.0).max(1.0))
            .floor() as usize
                % edge_count;

            for offset in 0..window_edges {
                let sort_pos = (window_start + offset) % edge_count;
                let edge_idx = self.edge_sorted_idx[sort_pos];
                let fade = 1.0 - offset as f32 / window_edges as f32;
                self.instances.push(EdgeInstance {
                    a: self.edge_a[edge_idx] as u32,
                    b: self.edge_b[edge_idx] as u32,
                    alpha_bits: fade.to_bits(),
                    _pad: 0,
                });
            }
        } else {
            for i in 0..edge_count {
                self.instances.push(EdgeInstance {
                    a: self.edge_a[i] as u32,
                    b: self.edge_b[i] as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
            }
        }

        let num_edges = self.instances.len() as u32;

        // Append dot instances (same position for a and b → capsule degenerates to circle)
        let dot_half_thick = if show_verts {
            let base_radius = DEFAULT_DOT_RADIUS * rt_height * vert_size * dot_scale;
            for i in 0..vert_count {
                self.instances.push(EdgeInstance {
                    a: i as u32,
                    b: i as u32,
                    alpha_bits: 1.0_f32.to_bits(),
                    _pad: 0,
                });
            }
            base_radius
        } else {
            0.0
        };

        (
            &self.positions,
            &self.instances,
            num_edges,
            edge_half_thick,
            dot_half_thick,
        )
    }

    /// Ensure sort scratch buffers are large enough.
    fn ensure_sort_buffers(&mut self, edge_count: usize) {
        if self.edge_depth.len() < edge_count {
            self.edge_depth.resize(edge_count, 0.0);
            self.edge_sorted_idx.resize(edge_count, 0);
        }
    }
}
