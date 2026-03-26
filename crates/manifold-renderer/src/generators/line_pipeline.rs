use crate::generators::generator_math::DEFAULT_DOT_RADIUS;
use crate::gpu_encoder::GpuEncoder;

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

/// Max blend state for line rendering — overlapping round caps take the brighter
/// value instead of accumulating, preventing visible bright dots at shared vertices.
const MAX_BLEND: wgpu::BlendState = wgpu::BlendState {
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
};

/// HAL BGL entries matching the wgpu BGL for line rendering.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const HAL_LINE_BGL_ENTRIES: [wgpu::wgt::BindGroupLayoutEntry; 3] = [
    // binding 0: Uniforms (vertex + fragment)
    wgpu::wgt::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
    // binding 1: Positions storage (vertex only)
    wgpu::wgt::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
    // binding 2: Instances storage (vertex only)
    wgpu::wgt::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
];

/// GPU pipeline for instanced anti-aliased line rendering with capsule SDF.
pub struct LinePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    positions_buffer: wgpu::Buffer,
    instances_buffer: wgpu::Buffer,
    // --- native Metal pipeline (macOS) ---
    #[cfg(target_os = "macos")]
    native_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Native shared-memory buffers for zero-copy CPU→GPU writes.
    #[cfg(target_os = "macos")]
    native_positions_buf: Option<manifold_gpu::GpuBuffer>,
    #[cfg(target_os = "macos")]
    native_instances_buf: Option<manifold_gpu::GpuBuffer>,
    /// HAL render pipeline for zero-overhead encoding.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipeline: Option<crate::hal_pipeline::HalRenderPipeline>,
    /// Shared-memory mapped pointers for direct CPU writes (hal path).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    uniform_mapped_ptr: Option<*mut u8>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    positions_mapped_ptr: Option<*mut u8>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    instances_mapped_ptr: Option<*mut u8>,
    /// Cached hal buffer pointers for bind group creation.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_uniform_ptr: Option<
        *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
    >,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_positions_ptr: Option<
        *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
    >,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_instances_ptr: Option<
        *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
    >,
}

// Safety: shared-memory pointers are only written from the content thread
// which owns the LinePipeline. hal pointers point to app-lifetime objects.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for LinePipeline {}

#[cfg(all(target_os = "macos", not(feature = "hal-encoding")))]
unsafe impl Send for LinePipeline {}

impl LinePipeline {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        label: &str,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding off
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

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("{label} Line Pipeline Layout")),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("{label} Line Pipeline")),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[], // No vertex buffers — all data from storage
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(MAX_BLEND),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
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

        // --- Native Metal pipeline from manifold-gpu ---
        #[cfg(target_os = "macos")]
        let (native_pipeline, native_positions_buf, native_instances_buf) =
            if let Some(dev) = native_device {
                let blend = manifold_gpu::GpuBlendState {
                    src_factor: manifold_gpu::GpuBlendFactor::One,
                    dst_factor: manifold_gpu::GpuBlendFactor::One,
                    operation: manifold_gpu::GpuBlendOp::Max,
                    src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                    dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                    alpha_operation: manifold_gpu::GpuBlendOp::Max,
                };
                let pipe = dev.create_render_pipeline(
                    include_str!("shaders/generator_lines.wgsl"),
                    "vs_main", "fs_main",
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    Some(blend),
                    &format!("{label} Line Native"),
                );
                let pos_buf = dev.create_buffer_shared(MAX_POSITIONS * POSITION_STRIDE);
                let inst_buf = dev.create_buffer_shared(MAX_INSTANCES * INSTANCE_STRIDE);
                (Some(pipe), Some(pos_buf), Some(inst_buf))
            } else {
                (None, None, None)
            };

        // --- Create buffers ---
        // On hal path: shared-memory buffers with persistent mapped pointers.
        // On wgpu path: regular buffers with COPY_DST for queue.write_buffer().
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ctx) = hal_ctx {
            let wgsl_source = include_str!("shaders/generator_lines.wgsl");

            let hal_pipe = crate::hal_pipeline::create_render_pipeline(
                ctx,
                wgsl_source,
                "vs_main",
                "fs_main",
                &HAL_LINE_BGL_ENTRIES,
                target_format,
                Some(MAX_BLEND),
                &format!("{label} Line HAL"),
            );

            // Create shared-memory buffers via hal
            let uniform_size =
                std::mem::size_of::<LineUniforms>() as u64;
            let (uniform_buffer, uniform_mapped, hal_uni_ptr) =
                create_shared_buffer(
                    device,
                    ctx,
                    uniform_size,
                    wgpu::wgt::BufferUses::UNIFORM
                        | wgpu::wgt::BufferUses::MAP_WRITE,
                    wgpu::BufferUsages::UNIFORM
                        | wgpu::BufferUsages::COPY_DST,
                    &format!("{label} Line Uniforms"),
                );
            let pos_size = MAX_POSITIONS * POSITION_STRIDE;
            let (positions_buffer, positions_mapped, hal_pos_ptr) =
                create_shared_buffer(
                    device,
                    ctx,
                    pos_size,
                    wgpu::wgt::BufferUses::STORAGE_READ_ONLY
                        | wgpu::wgt::BufferUses::MAP_WRITE,
                    wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_DST,
                    &format!("{label} Line Positions"),
                );
            let inst_size = MAX_INSTANCES * INSTANCE_STRIDE;
            let (instances_buffer, instances_mapped, hal_inst_ptr) =
                create_shared_buffer(
                    device,
                    ctx,
                    inst_size,
                    wgpu::wgt::BufferUses::STORAGE_READ_ONLY
                        | wgpu::wgt::BufferUses::MAP_WRITE,
                    wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_DST,
                    &format!("{label} Line Instances"),
                );

            return Self {
                pipeline,
                bind_group_layout,
                uniform_buffer,
                positions_buffer,
                instances_buffer,
                #[cfg(target_os = "macos")]
                native_pipeline,
                #[cfg(target_os = "macos")]
                native_positions_buf,
                #[cfg(target_os = "macos")]
                native_instances_buf,
                hal_pipeline: Some(hal_pipe),
                uniform_mapped_ptr: Some(uniform_mapped),
                positions_mapped_ptr: Some(positions_mapped),
                instances_mapped_ptr: Some(instances_mapped),
                hal_uniform_ptr: Some(hal_uni_ptr),
                hal_positions_ptr: Some(hal_pos_ptr),
                hal_instances_ptr: Some(hal_inst_ptr),
            };
        }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Line Uniforms")),
            size: std::mem::size_of::<LineUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let positions_buffer =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Line Positions")),
                size: MAX_POSITIONS * POSITION_STRIDE,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        let instances_buffer =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Line Instances")),
                size: MAX_INSTANCES * INSTANCE_STRIDE,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            positions_buffer,
            instances_buffer,
            #[cfg(target_os = "macos")]
            native_pipeline,
            #[cfg(target_os = "macos")]
            native_positions_buf,
            #[cfg(target_os = "macos")]
            native_instances_buf,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            uniform_mapped_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            positions_mapped_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            instances_mapped_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_positions_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_instances_ptr: None,
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
        gpu: &mut GpuEncoder,
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
        // ── NATIVE METAL render path ────────────────────────────────
        #[cfg(target_os = "macos")]
        if let Some(ref native_pipe) = self.native_pipeline
            && let Some(ref native_pos) = self.native_positions_buf
            && let Some(ref native_inst) = self.native_instances_buf
            && gpu.has_native_encoder()
        {
            let native_target = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(target)
            };
            let native_enc = unsafe { gpu.native_encoder_mut() }.unwrap();

            if instances.is_empty() {
                native_enc.clear_texture(&native_target, 0.0, 0.0, 0.0, 0.0);
                return;
            }

            // Write data directly to shared-memory buffers
            let uniforms = LineUniforms {
                rt_width: width as f32,
                rt_height: height as f32,
                edge_half_thick,
                beat,
                dot_half_thick,
                num_edges,
                _pad: [0.0; 2],
            };
            let pos_bytes = bytemuck::cast_slice(positions);
            let pos_limit = (MAX_POSITIONS * POSITION_STRIDE) as usize;
            let pos_len = pos_bytes.len().min(pos_limit);
            unsafe { native_pos.write(0, &pos_bytes[..pos_len]); }

            let inst_bytes = bytemuck::cast_slice(instances);
            let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
            let inst_len = inst_bytes.len().min(inst_limit);
            unsafe { native_inst.write(0, &inst_bytes[..inst_len]); }

            let instance_count = (instances.len() as u64).min(MAX_INSTANCES) as u32;
            native_enc.draw_instanced(
                native_pipe,
                &native_target,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 1,
                        buffer: native_pos,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 2,
                        buffer: native_inst,
                        offset: 0,
                    },
                ],
                6,
                instance_count,
                true,
                profiler_label,
            );
            return;
        }

        // --- HAL render path ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder()
            && self.hal_pipeline.is_some()
            && self.uniform_mapped_ptr.is_some()
        {
            self.draw_hal(
                gpu, target, positions, instances, num_edges,
                edge_half_thick, dot_half_thick, beat, profiler_label,
                width, height,
            );
            return;
        }

        // --- wgpu fallback path ---
        if instances.is_empty() {
            let ts = profiler.and_then(|p| {
                p.render_timestamps(profiler_label, width, height)
            });
            let _pass = gpu.encoder.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("Line Clear Pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(
                                    wgpu::Color::TRANSPARENT,
                                ),
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: ts,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            );
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
        gpu.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        // Upload positions
        let pos_bytes = bytemuck::cast_slice(positions);
        let pos_limit = (MAX_POSITIONS * POSITION_STRIDE) as usize;
        gpu.queue.write_buffer(
            &self.positions_buffer,
            0,
            &pos_bytes[..pos_bytes.len().min(pos_limit)],
        );

        // Upload instances
        let inst_bytes = bytemuck::cast_slice(instances);
        let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
        gpu.queue.write_buffer(
            &self.instances_buffer,
            0,
            &inst_bytes[..inst_bytes.len().min(inst_limit)],
        );

        let bind_group =
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Line BG"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self
                            .uniform_buffer
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .positions_buffer
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self
                            .instances_buffer
                            .as_entire_binding(),
                    },
                ],
            });

        let instance_count =
            (instances.len() as u64).min(MAX_INSTANCES) as u32;
        {
            let ts = profiler.and_then(|p| {
                p.render_timestamps(profiler_label, width, height)
            });
            let mut pass = gpu.encoder.begin_render_pass(
                &wgpu::RenderPassDescriptor {
                    label: Some("Line Draw Pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(
                                    wgpu::Color::TRANSPARENT,
                                ),
                                store: wgpu::StoreOp::Store,
                            },
                        },
                    )],
                    depth_stencil_attachment: None,
                    timestamp_writes: ts,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..6, 0..instance_count);
        }
    }

    /// HAL render path — direct shared-memory writes + hal render pass.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(clippy::too_many_arguments)]
    fn draw_hal(
        &self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        positions: &[[f32; 2]],
        instances: &[EdgeInstance],
        num_edges: u32,
        edge_half_thick: f32,
        dot_half_thick: f32,
        beat: f32,
        label: &str,
        width: u32,
        height: u32,
    ) {
        use wgpu::hal::{self, CommandEncoder as HalCmdEnc, Device as HalDevice};
        type MetalApi = hal::api::Metal;

        let hal_pipe = self.hal_pipeline.as_ref().unwrap();
        let (hal_enc, hal_ctx) =
            unsafe { gpu.hal_encoder_mut() }.unwrap();

        // Extract target view pointer
        let target_ptr = {
            let g = unsafe { target.as_hal::<MetalApi>() }
                .expect("target not Metal");
            &*g as *const _
        };

        if instances.is_empty() {
            // Clear-only pass
            unsafe {
                hal_enc
                    .begin_render_pass(&hal::RenderPassDescriptor {
                        label: Some(label),
                        extent: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        sample_count: 1,
                        color_attachments: &[Some(
                            hal::ColorAttachment {
                                target: hal::Attachment {
                                    view: &*target_ptr,
                                    usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                                },
                                resolve_target: None,
                                ops: hal::AttachmentOps::LOAD_CLEAR
                                    | hal::AttachmentOps::STORE,
                                clear_value: wgpu::Color::TRANSPARENT,
                                depth_slice: None,
                            },
                        )],
                        depth_stencil_attachment: None,
                        multiview_mask: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    })
                    .expect("hal begin_render_pass failed");
                hal_enc.end_render_pass();
            }
            return;
        }

        // Write uniforms directly to shared memory
        let uniforms = LineUniforms {
            rt_width: width as f32,
            rt_height: height as f32,
            edge_half_thick,
            beat,
            dot_half_thick,
            num_edges,
            _pad: [0.0; 2],
        };
        let uni_ptr = self.uniform_mapped_ptr.unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&uniforms).as_ptr(),
                uni_ptr,
                std::mem::size_of::<LineUniforms>(),
            );
        }

        // Write positions
        let pos_bytes = bytemuck::cast_slice(positions);
        let pos_limit = (MAX_POSITIONS * POSITION_STRIDE) as usize;
        let pos_len = pos_bytes.len().min(pos_limit);
        let pos_ptr = self.positions_mapped_ptr.unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(
                pos_bytes.as_ptr(),
                pos_ptr,
                pos_len,
            );
        }

        // Write instances
        let inst_bytes = bytemuck::cast_slice(instances);
        let inst_limit = (MAX_INSTANCES * INSTANCE_STRIDE) as usize;
        let inst_len = inst_bytes.len().min(inst_limit);
        let inst_ptr = self.instances_mapped_ptr.unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(
                inst_bytes.as_ptr(),
                inst_ptr,
                inst_len,
            );
        }

        // Create hal bind group
        let hal_bg = unsafe {
            hal_ctx
                .device()
                .create_bind_group(&hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry {
                            binding: 0,
                            resource_index: 0,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 1,
                            resource_index: 1,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 2,
                            resource_index: 2,
                            count: 1,
                        },
                    ],
                    buffers: &[
                        hal::BufferBinding::new_unchecked(
                            &*self.hal_uniform_ptr.unwrap(),
                            0,
                            std::num::NonZero::new(
                                std::mem::size_of::<LineUniforms>()
                                    as u64,
                            ),
                        ),
                        hal::BufferBinding::new_unchecked(
                            &*self.hal_positions_ptr.unwrap(),
                            0,
                            std::num::NonZero::new(pos_len as u64),
                        ),
                        hal::BufferBinding::new_unchecked(
                            &*self.hal_instances_ptr.unwrap(),
                            0,
                            std::num::NonZero::new(inst_len as u64),
                        ),
                    ],
                    samplers: &[],
                    textures: &[],
                    acceleration_structures: &[],
                    external_textures: &[],
                })
                .expect("Failed to create hal line bind group")
        };

        let instance_count =
            (instances.len() as u64).min(MAX_INSTANCES) as u32;

        // Encode hal render pass with max blend
        unsafe {
            hal_enc
                .begin_render_pass(&hal::RenderPassDescriptor {
                    label: Some(label),
                    extent: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    sample_count: 1,
                    color_attachments: &[Some(
                        hal::ColorAttachment {
                            target: hal::Attachment {
                                view: &*target_ptr,
                                usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                            },
                            resolve_target: None,
                            ops: hal::AttachmentOps::LOAD_CLEAR
                                | hal::AttachmentOps::STORE,
                            clear_value: wgpu::Color::TRANSPARENT,
                            depth_slice: None,
                        },
                    )],
                    depth_stencil_attachment: None,
                    multiview_mask: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .expect("hal begin_render_pass failed");
            hal_enc.set_render_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_pipe.pipeline_layout,
                0,
                &hal_bg,
                &[],
            );
            hal_enc.draw(0, 6, 0, instance_count);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }
}

/// Create a shared-memory buffer via hal, import into wgpu.
/// Returns (wgpu_buffer, mapped_ptr, hal_buffer_ptr).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
fn create_shared_buffer(
    device: &wgpu::Device,
    hal_ctx: &crate::hal_context::HalContext,
    size: u64,
    hal_usage: wgpu::wgt::BufferUses,
    wgpu_usage: wgpu::BufferUsages,
    label: &str,
) -> (
    wgpu::Buffer,
    *mut u8,
    *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
) {
    use wgpu::hal::{self, Device as HalDevice};
    type MetalApi = hal::api::Metal;

    let hal_buf = unsafe {
        hal_ctx
            .device()
            .create_buffer(&hal::BufferDescriptor {
                label: Some(label),
                size,
                usage: hal_usage,
                memory_flags: hal::MemoryFlags::PREFER_COHERENT,
            })
            .expect("Failed to create hal shared buffer")
    };

    let mapping = unsafe {
        hal_ctx
            .device()
            .map_buffer(&hal_buf, 0..size)
            .expect("Failed to map hal shared buffer")
    };
    let mapped_ptr = mapping.ptr.as_ptr();

    let buffer = unsafe {
        device.create_buffer_from_hal::<MetalApi>(
            hal_buf,
            &wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu_usage,
                mapped_at_creation: false,
            },
        )
    };

    let hal_ptr = {
        let guard = unsafe {
            buffer
                .as_hal::<MetalApi>()
                .expect("shared buffer not Metal")
        };
        &*guard as *const <MetalApi as hal::Api>::Buffer
    };

    (buffer, mapped_ptr, hal_ptr)
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
                let fade =
                    1.0 - offset as f32 / window_edges as f32;
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
            let base_radius =
                DEFAULT_DOT_RADIUS * rt_height * vert_size * dot_scale;
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
