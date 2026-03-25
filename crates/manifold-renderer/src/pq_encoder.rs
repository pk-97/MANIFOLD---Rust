//! Linear EDR → ST.2084 PQ encoder pipeline for HDR export.
//!
//! Takes the final compositor output (post-tonemap, post-effects in EDR
//! display-linear space) and encodes to PQ for HDR10 HEVC delivery.
//! The result matches what the user sees on their HDR display — bloom,
//! halation, and all master effects are preserved.
//!
//! When `hal-encoding` is enabled, uses a compute dispatch instead of a render
//! pass. This eliminates Metal TBDR tile alloc/load/store overhead. The compute
//! shader produces identical output — same PQ math, same uniform struct.

use crate::render_target::RenderTarget;

/// BGL entries for the PQ encoder compute pipeline.
/// 4 bindings: uniform, source texture, sampler, output storage texture.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const PQ_COMPUTE_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

/// Uniform buffer layout for the PQ encoder shader. 16-byte aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PqUniforms {
    paper_white: f32,
    max_nits: f32,
    _pad0: f32,
    _pad1: f32,
}

/// GPU pipeline for EDR → PQ transfer function encoding.
pub struct PqEncoder {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// PQ-encoded output buffer for the Metal encoder to read.
    pub output: RenderTarget,
    /// Compute pipeline (eliminates TBDR tile overhead).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_pipeline: Option<wgpu::ComputePipeline>,
    /// BGL for the compute pipeline (4 bindings: uniform, texture, sampler, storage).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_bind_group_layout: Option<wgpu::BindGroupLayout>,
    /// HAL compute pipeline for zero-overhead encoding.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_compute_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
    /// Persistent mapped pointer to shared-memory uniform buffer.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_uniform_mapped_ptr: Option<*mut u8>,
    /// Cached hal pointer to uniform buffer for bind groups.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_uniform_buf_ptr: Option<*const crate::hal_context::MetalBuffer>,
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for PqEncoder {}

impl PqEncoder {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx;
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Linear-to-PQ Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("effects/shaders/linear_to_pq.wgsl").into(),
            ),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("PQ Encoder BGL"),
                entries: &[
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PQ Encoder Layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("PQ Encoder Pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("PQ Encoder Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PQ Encoder Uniforms"),
            size: std::mem::size_of::<PqUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output =
            RenderTarget::new(device, width, height, format, "PQ Export Output");

        // --- hal compute pipeline + shared-memory uniform buffer ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (
            compute_pipeline,
            compute_bind_group_layout,
            hal_compute_pipeline,
            hal_sampler,
            uniform_buffer,
            hal_uniform_mapped_ptr,
            hal_uniform_buf_ptr,
        ) = if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            let compute_shader =
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("Linear-to-PQ Compute Shader"),
                    source: wgpu::ShaderSource::Wgsl(
                        include_str!(
                            "effects/shaders/linear_to_pq_compute.wgsl"
                        )
                        .into(),
                    ),
                });

            let compute_bgl = device.create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some("PQ Encoder Compute BGL"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        PQ_COMPUTE_BGL_ENTRIES[1],
                        PQ_COMPUTE_BGL_ENTRIES[2],
                        PQ_COMPUTE_BGL_ENTRIES[3],
                    ],
                },
            );

            let compute_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("PQ Encoder Compute Layout"),
                    bind_group_layouts: &[&compute_bgl],
                    immediate_size: 0,
                });

            let compute_pipe = device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("PQ Encoder Compute Pipeline"),
                    layout: Some(&compute_layout),
                    module: &compute_shader,
                    entry_point: Some("cs_main"),
                    compilation_options:
                        wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });

            // hal compute pipeline
            let hal_comp_pipe = crate::hal_pipeline::create_compute_pipeline(
                ctx,
                include_str!("effects/shaders/linear_to_pq_compute.wgsl"),
                "cs_main",
                &PQ_COMPUTE_BGL_ENTRIES,
                "PQ Encoder Compute HAL",
            );

            // hal sampler
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("PQ Encoder Sampler HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal PQ encoder sampler")
            };

            // Shared-memory uniform buffer
            let ubo_size = std::mem::size_of::<PqUniforms>() as u64;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("PQ Encoder Uniforms HAL"),
                        size: ubo_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags:
                            wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal PQ uniform buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..ubo_size)
                    .expect("Failed to map hal PQ uniform buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("PQ Encoder Uniforms"),
                        size: ubo_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let buf_hal_ptr = {
                let guard = unsafe {
                    wgpu_buf.as_hal::<wgpu::hal::api::Metal>()
                }
                .expect("pq ubo not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            (
                Some(compute_pipe),
                Some(compute_bgl),
                Some(hal_comp_pipe),
                Some(hal_samp),
                wgpu_buf,
                Some(mapped_ptr),
                Some(buf_hal_ptr),
            )
        } else {
            (None, None, None, None, uniform_buffer, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let uniform_buffer = uniform_buffer;

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            output,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_bind_group_layout,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_mapped_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_buf_ptr,
        }
    }

    /// Encode EDR display-linear input to PQ output for HDR10 export.
    ///
    /// `edr_source`: the final compositor output (post-tonemap, post-effects)
    /// `paper_white_nits`: EDR 1.0 = this many nits (typically 200)
    /// `max_nits`: PQ ceiling (typically 10000)
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        edr_source: &wgpu::TextureView,
        paper_white_nits: f32,
        max_nits: f32,
    ) {
        let uniforms = PqUniforms {
            paper_white: paper_white_nits,
            max_nits,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        // When compute pipeline is available (hal-encoding feature), use
        // wgpu compute dispatch. Eliminates Metal TBDR tile overhead.
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let (Some(compute_pipe), Some(compute_bgl)) =
            (&self.compute_pipeline, &self.compute_bind_group_layout)
        {
            let bind_group =
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("PQ Encoder Compute BG"),
                    layout: compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self
                                .uniform_buffer
                                .as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource:
                                wgpu::BindingResource::TextureView(
                                    edr_source,
                                ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource:
                                wgpu::BindingResource::TextureView(
                                    &self.output.view,
                                ),
                        },
                    ],
                });

            let mut pass = encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("PQ Encode Compute Pass"),
                    timestamp_writes: None,
                });
            pass.set_pipeline(compute_pipe);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                self.output.width.div_ceil(16),
                self.output.height.div_ceil(16),
                1,
            );
            return;
        }

        // --- render pass fallback (non-hal-encoding builds) ---
        let bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PQ Encoder BG"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            edr_source,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                ],
            });

        let mut pass =
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PQ Encode Pass"),
                color_attachments: &[Some(
                    wgpu::RenderPassColorAttachment {
                        view: &self.output.view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    },
                )],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// HAL path: encode PQ as compute dispatch via hal command encoder.
    /// Writes uniforms directly to shared-memory buffer (no API call).
    /// Eliminates TBDR tile overhead vs the render pass path.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    pub(crate) unsafe fn encode_hal_compute(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        edr_source_hal_view: &crate::hal_context::MetalTextureView,
        output_hal_view: &crate::hal_context::MetalTextureView,
        paper_white_nits: f32,
        max_nits: f32,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let uniforms = PqUniforms {
            paper_white: paper_white_nits,
            max_nits,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        if let Some(mapped_ptr) = self.hal_uniform_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&uniforms).as_ptr(),
                    mapped_ptr,
                    std::mem::size_of::<PqUniforms>(),
                );
            }
        }

        let hal_pipe = self
            .hal_compute_pipeline
            .as_ref()
            .expect("pq hal compute pipeline");
        let hal_samp =
            self.hal_sampler.as_ref().expect("pq hal sampler");
        let hal_ubo = unsafe {
            &*self.hal_uniform_buf_ptr.expect("pq hal ubo")
        };

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
                            resource_index: 0,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 2,
                            resource_index: 0,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 3,
                            resource_index: 1,
                            count: 1,
                        },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_ubo,
                        0,
                        std::num::NonZero::new(
                            std::mem::size_of::<PqUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding {
                            view: edr_source_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: output_hal_view,
                            usage:
                                wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                        },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                })
                .expect("Failed to create hal PQ compute bind group")
        };

        unsafe {
            hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                label: Some("PQ Encode Compute"),
                timestamp_writes: None,
            });
            hal_enc.set_compute_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_pipe.pipeline_layout,
                0,
                &hal_bg,
                &[],
            );
            hal_enc.dispatch([
                self.output.width.div_ceil(16),
                self.output.height.div_ceil(16),
                1,
            ]);
            hal_enc.end_compute_pass();
        }

        unsafe {
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }

    /// Resize the PQ output buffer.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) {
        self.output.resize(device, width, height);
    }
}
