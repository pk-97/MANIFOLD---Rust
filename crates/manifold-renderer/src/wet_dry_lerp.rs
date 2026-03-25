//! GPU pipeline for wet/dry lerp blending in effect groups.
//!
//! Matches Unity's GroupWetDryLerp.shader: `lerp(dry, wet, wetDry)`.
//! - dry = snapshot taken before group effects ran
//! - wet = buffer after group effects ran
//! - wetDry = 1.0 -> fully wet (all effects), 0.0 -> fully dry (bypass)
//!
//! When `hal-encoding` is enabled, uses a compute dispatch instead of a render
//! pass. This eliminates Metal TBDR tile alloc/load/store overhead. The compute
//! shader produces identical output — same lerp math, same uniform struct.

use crate::gpu_encoder::GpuEncoder;

/// BGL entries for the wet/dry lerp render pipeline (shared between wgpu and hal paths).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const WET_DRY_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
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
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
];

/// BGL entries for the wet/dry lerp compute pipeline (hal path).
/// 5 bindings: uniform, dry texture, wet texture, sampler, output storage texture.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const WET_DRY_COMPUTE_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 5] = [
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
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WetDryUniforms {
    wet_dry: f32,
    _pad: [f32; 3],
}

pub struct WetDryLerpPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_pipeline: Option<crate::hal_pipeline::HalRenderPipeline>,
    /// Compute pipeline for hal-encoding path (eliminates TBDR tile overhead).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_pipeline: Option<wgpu::ComputePipeline>,
    /// BGL for the compute pipeline (5 bindings: uniform, dry, wet, sampler, storage).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_bind_group_layout: Option<wgpu::BindGroupLayout>,
    /// HAL compute pipeline for zero-overhead encoding.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_compute_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
    /// Persistent mapped pointer to shared-memory uniform buffer.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_uniform_mapped_ptr: Option<*mut u8>,
    /// Cached hal pointer to uniform buffer for bind groups.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_uniform_buf_ptr: Option<*const crate::hal_context::MetalBuffer>,
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for WetDryLerpPipeline {}

impl WetDryLerpPipeline {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx;
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("WetDry Lerp Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("effects/shaders/wet_dry_lerp.wgsl").into(),
            ),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("WetDry Lerp BGL"),
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
                        binding: 3,
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
                label: Some("WetDry Lerp Layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("WetDry Lerp Pipeline"),
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
            label: Some("WetDry Lerp Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WetDry Lerp Uniforms"),
            size: std::mem::size_of::<WetDryUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- hal compute pipeline + shared-memory uniform buffer ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (
            hal_pipeline,
            compute_pipeline,
            compute_bind_group_layout,
            hal_compute_pipeline,
            hal_sampler,
            uniform_buffer,
            hal_uniform_mapped_ptr,
            hal_uniform_buf_ptr,
        ) = if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            // Render pipeline (kept for legacy apply_hal path)
            let hal_pipe = crate::hal_pipeline::create_render_pipeline(
                ctx,
                include_str!("effects/shaders/wet_dry_lerp.wgsl"),
                "vs_main",
                "fs_main",
                &WET_DRY_BGL_ENTRIES,
                format,
                "WetDry Lerp HAL",
            );

            // wgpu compute pipeline (fallback when hal encoder isn't active)
            let compute_shader =
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("WetDry Lerp Compute Shader"),
                    source: wgpu::ShaderSource::Wgsl(
                        include_str!(
                            "effects/shaders/wet_dry_lerp_compute.wgsl"
                        )
                        .into(),
                    ),
                });

            let compute_bgl = device.create_bind_group_layout(
                &wgpu::BindGroupLayoutDescriptor {
                    label: Some("WetDry Lerp Compute BGL"),
                    entries: &[
                        WET_DRY_COMPUTE_BGL_ENTRIES[0],
                        WET_DRY_COMPUTE_BGL_ENTRIES[1],
                        WET_DRY_COMPUTE_BGL_ENTRIES[2],
                        WET_DRY_COMPUTE_BGL_ENTRIES[3],
                        WET_DRY_COMPUTE_BGL_ENTRIES[4],
                    ],
                },
            );

            let compute_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("WetDry Lerp Compute Layout"),
                    bind_group_layouts: &[&compute_bgl],
                    immediate_size: 0,
                });

            let compute_pipe =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("WetDry Lerp Compute Pipeline"),
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
                include_str!("effects/shaders/wet_dry_lerp_compute.wgsl"),
                "cs_main",
                &WET_DRY_COMPUTE_BGL_ENTRIES,
                "WetDry Lerp Compute HAL",
            );

            // hal sampler (shared between render and compute paths)
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("WetDry Lerp Sampler HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal wet/dry sampler")
            };

            // Shared-memory uniform buffer
            let ubo_size = std::mem::size_of::<WetDryUniforms>() as u64;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("WetDry Lerp Uniforms HAL"),
                        size: ubo_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal wet/dry uniform buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..ubo_size)
                    .expect("Failed to map hal wet/dry uniform buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("WetDry Lerp Uniforms"),
                        size: ubo_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let buf_hal_ptr = {
                let guard =
                    unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                        .expect("wet/dry ubo not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            (
                Some(hal_pipe),
                Some(compute_pipe),
                Some(compute_bgl),
                Some(hal_comp_pipe),
                Some(hal_samp),
                wgpu_buf,
                Some(mapped_ptr),
                Some(buf_hal_ptr),
            )
        } else {
            (None, None, None, None, None, uniform_buffer, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let uniform_buffer = uniform_buffer;

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
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

    /// Blend dry and wet textures into the target.
    /// wet_dry = 1.0 means fully wet (effects applied), 0.0 means fully dry (bypass).
    #[allow(unused_variables)]
    pub fn apply(
        &self,
        gpu: &mut GpuEncoder,
        dry_view: &wgpu::TextureView,
        wet_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        wet_dry: f32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // --- hal path: compute dispatch via hal command encoder ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            type MetalApi = wgpu::hal::api::Metal;
            let dry_ptr = {
                let g = unsafe { dry_view.as_hal::<MetalApi>() }
                    .expect("dry_view not Metal");
                &*g as *const _
            };
            let wet_ptr = {
                let g = unsafe { wet_view.as_hal::<MetalApi>() }
                    .expect("wet_view not Metal");
                &*g as *const _
            };
            let target_ptr = {
                let g = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target_view not Metal");
                &*g as *const _
            };
            let (hal_enc, hal_ctx) =
                unsafe { gpu.hal_encoder_mut() }.unwrap();
            unsafe {
                self.apply_hal_compute(
                    hal_enc,
                    hal_ctx,
                    &*dry_ptr,
                    &*wet_ptr,
                    &*target_ptr,
                    target_width,
                    target_height,
                    wet_dry,
                );
            }
            return;
        }

        let uniforms = WetDryUniforms {
            wet_dry,
            _pad: [0.0; 3],
        };
        gpu.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        // When compute pipeline is available (hal-encoding feature) and no hal
        // encoder is active, use wgpu compute dispatch as fallback.
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let (Some(compute_pipe), Some(compute_bgl)) =
            (&self.compute_pipeline, &self.compute_bind_group_layout)
        {
            let bind_group =
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("WetDry Lerp Compute BG"),
                    layout: compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(
                                dry_view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(
                                wet_view,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(
                                &self.sampler,
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(
                                target_view,
                            ),
                        },
                    ],
                });

            let ts = profiler.and_then(|p| {
                p.compute_timestamps(
                    "WetDry Lerp",
                    target_width,
                    target_height,
                )
            });
            let mut pass = gpu.encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor {
                    label: Some("WetDry Lerp Compute Pass"),
                    timestamp_writes: ts,
                },
            );
            pass.set_pipeline(compute_pipe);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                target_width.div_ceil(16),
                target_height.div_ceil(16),
                1,
            );
            return;
        }

        // --- render pass fallback (non-hal-encoding builds) ---
        let bind_group =
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("WetDry Lerp BG"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            dry_view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(
                            wet_view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                ],
            });

        let ts = profiler
            .and_then(|p| p.render_timestamps("WetDry Lerp", 0, 0));
        let mut pass =
            gpu.encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("WetDry Lerp Pass"),
                    color_attachments: &[Some(
                        wgpu::RenderPassColorAttachment {
                            view: target_view,
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
                });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// HAL path: encode wet/dry lerp render pass via hal command encoder.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    pub(crate) unsafe fn apply_hal(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        dry_hal_view: &crate::hal_context::MetalTextureView,
        wet_hal_view: &crate::hal_context::MetalTextureView,
        target_hal_view: &crate::hal_context::MetalTextureView,
        target_width: u32,
        target_height: u32,
        wet_dry: f32,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let uniforms = WetDryUniforms {
            wet_dry,
            _pad: [0.0; 3],
        };

        // Direct memcpy to shared-memory uniform buffer
        if let Some(mapped_ptr) = self.hal_uniform_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&uniforms).as_ptr(),
                    mapped_ptr,
                    std::mem::size_of::<WetDryUniforms>(),
                );
            }
        }

        let hal_pipe =
            self.hal_pipeline.as_ref().expect("wet/dry hal pipeline");
        let hal_samp =
            self.hal_sampler.as_ref().expect("wet/dry hal sampler");
        let hal_ubo = unsafe {
            &*self.hal_uniform_buf_ptr.expect("wet/dry hal ubo")
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
                            resource_index: 1,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 3,
                            resource_index: 0,
                            count: 1,
                        },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_ubo,
                        0,
                        std::num::NonZero::new(
                            std::mem::size_of::<WetDryUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding {
                            view: dry_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: wet_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                })
                .expect("Failed to create hal wet/dry bind group")
        };

        unsafe {
            hal_enc
                .begin_render_pass(&hal::RenderPassDescriptor {
                    label: Some("WetDry Lerp Pass"),
                    extent: wgpu::Extent3d {
                        width: target_width,
                        height: target_height,
                        depth_or_array_layers: 1,
                    },
                    sample_count: 1,
                    color_attachments: &[Some(hal::ColorAttachment {
                        target: hal::Attachment {
                            view: target_hal_view,
                            usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                        },
                        resolve_target: None,
                        ops: hal::AttachmentOps::LOAD_CLEAR
                            | hal::AttachmentOps::STORE,
                        clear_value: wgpu::Color::TRANSPARENT,
                        depth_slice: None,
                    })],
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
            hal_enc.draw(0, 3, 0, 1);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }

    /// HAL path: encode wet/dry lerp as compute dispatch via hal command encoder.
    /// Writes uniforms directly to shared-memory buffer (no API call).
    /// Eliminates TBDR tile overhead vs the render pass path.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub(crate) unsafe fn apply_hal_compute(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        dry_hal_view: &crate::hal_context::MetalTextureView,
        wet_hal_view: &crate::hal_context::MetalTextureView,
        target_hal_view: &crate::hal_context::MetalTextureView,
        target_width: u32,
        target_height: u32,
        wet_dry: f32,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let uniforms = WetDryUniforms {
            wet_dry,
            _pad: [0.0; 3],
        };

        // Direct memcpy to shared-memory uniform buffer
        if let Some(mapped_ptr) = self.hal_uniform_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&uniforms).as_ptr(),
                    mapped_ptr,
                    std::mem::size_of::<WetDryUniforms>(),
                );
            }
        }

        let hal_pipe = self
            .hal_compute_pipeline
            .as_ref()
            .expect("wet/dry hal compute pipeline");
        let hal_samp =
            self.hal_sampler.as_ref().expect("wet/dry hal sampler");
        let hal_ubo = unsafe {
            &*self.hal_uniform_buf_ptr.expect("wet/dry hal ubo")
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
                            resource_index: 1,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 3,
                            resource_index: 0,
                            count: 1,
                        },
                        hal::BindGroupEntry {
                            binding: 4,
                            resource_index: 2,
                            count: 1,
                        },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_ubo,
                        0,
                        std::num::NonZero::new(
                            std::mem::size_of::<WetDryUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding {
                            view: dry_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: wet_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: target_hal_view,
                            usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                        },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                })
                .expect("Failed to create hal wet/dry compute bind group")
        };

        unsafe {
            hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                label: Some("WetDry Lerp Compute"),
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
                target_width.div_ceil(16),
                target_height.div_ceil(16),
                1,
            ]);
            hal_enc.end_compute_pass();
        }

        unsafe {
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }
}
