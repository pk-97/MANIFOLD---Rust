//! ACES tonemapping pipeline — mechanical translation of Unity's
//! CompositorStack.ApplyTonemap() + ACESTonemap.shader.
//!
//! Owned by the compositor. Applied as the final step after master effects,
//! before the blit to the display surface.

use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;

/// BGL entries for the tonemap render pipeline (shared between wgpu and hal paths).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const TONEMAP_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 3] = [
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
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
];

/// Per-frame tonemap settings. Matches Unity CompositorStack properties:
/// TonemapExposure, HDROutputEnabled, PaperWhiteNits, MaxDisplayNits.
#[derive(Debug, Clone, Copy)]
pub struct TonemapSettings {
    /// Exposure multiplier for ACES tonemapping. 1.0 = neutral.
    /// Matches Unity CompositorStack.TonemapExposure.
    pub exposure: f32,
    /// HDR output mode. false = SDR (sRGB tonemap), true = HDR display-linear (EDR).
    /// Matches Unity CompositorStack.HDROutputEnabled.
    pub hdr_output_enabled: bool,
    /// Paper white in nits (scene 1.0 maps to this). Typical: 200 nits.
    /// Matches Unity CompositorStack.PaperWhiteNits.
    pub paper_white_nits: f32,
    /// Display maximum luminance in nits. HDR TVs: 1000, LED walls: 5000+.
    /// Matches Unity CompositorStack.MaxDisplayNits.
    pub max_display_nits: f32,
}

impl Default for TonemapSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            hdr_output_enabled: false,
            paper_white_nits: 200.0,
            max_display_nits: 1000.0,
        }
    }
}

/// Uniform buffer layout for the tonemap shader.
/// 16 bytes, naturally aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TonemapUniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32, // 0 = SDR, 1 = PQ, 2 = EDR
}

/// GPU pipeline for ACES tonemapping.
/// Follows the exact pattern of WetDryLerpPipeline.
pub struct TonemapPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    /// Tonemap output buffer. Matches Unity's tonemappedOutput RenderTexture.
    /// Separate from the compositor's main buffer so PreTonemapOutput survives.
    pub output: RenderTarget,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_pipeline: Option<crate::hal_pipeline::HalRenderPipeline>,
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
unsafe impl Send for TonemapPipeline {}

impl TonemapPipeline {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx;
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ACES Tonemap Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("effects/shaders/aces_tonemap.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Tonemap BGL"),
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Tonemap Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Tonemap Pipeline"),
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
            label: Some("Tonemap Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Tonemap Uniforms"),
            size: std::mem::size_of::<TonemapUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output = RenderTarget::new(device, width, height, format, "TonemappedOutput");

        // --- hal pipeline + shared-memory uniform buffer ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler, uniform_buffer, hal_uniform_mapped_ptr, hal_uniform_buf_ptr) =
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;
            let hal_pipe = crate::hal_pipeline::create_render_pipeline(
                ctx,
                include_str!("effects/shaders/aces_tonemap.wgsl"),
                "vs_main",
                "fs_main",
                &TONEMAP_BGL_ENTRIES,
                format,
                "Tonemap HAL",
            );
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("Tonemap Sampler HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal tonemap sampler")
            };
            // Shared-memory uniform buffer
            let ubo_size = std::mem::size_of::<TonemapUniforms>() as u64;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("Tonemap Uniforms HAL"),
                        size: ubo_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal tonemap uniform buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..ubo_size)
                    .expect("Failed to map hal tonemap uniform buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("Tonemap Uniforms"),
                        size: ubo_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let buf_hal_ptr = {
                let guard = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("tonemap ubo not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            (Some(hal_pipe), Some(hal_samp), wgpu_buf, Some(mapped_ptr), Some(buf_hal_ptr))
        } else {
            (None, None, uniform_buffer, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let uniform_buffer = uniform_buffer;

        Self {
            pipeline, bind_group_layout, sampler, uniform_buffer, output,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_mapped_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_uniform_buf_ptr,
        }
    }

    /// Apply ACES tonemapping to the HDR source buffer.
    /// Matches Unity CompositorStack.ApplyTonemap().
    ///
    /// Realtime display uses SDR (mode 0) or EDR (mode 2) depending on
    /// hdr_output_enabled. PQ (mode 1) is reserved for export pipeline.
    pub fn apply(
        &self,
        gpu: &mut GpuEncoder,
        hdr_source: &wgpu::TextureView,
        settings: &TonemapSettings,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        // Realtime HDR preview uses EDR passthrough (3) — no ACES compression,
        // linear values passed directly to macOS EDR with soft-clip at display peak.
        // Mode 2 (ACES EDR) retained for explicit use. Mode 1 reserved for PQ export.
        let mode = if settings.hdr_output_enabled { 3u32 } else { 0u32 };

        let uniforms = TonemapUniforms {
            exposure: settings.exposure,
            paper_white: settings.paper_white_nits,
            max_nits: settings.max_display_nits,
            mode,
        };
        gpu.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Tonemap BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(hdr_source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let ts = profiler.and_then(|p| {
            p.render_timestamps("Tonemap", self.output.width, self.output.height)
        });
        let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Tonemap Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.output.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
        pass.draw(0..3, 0..1);
    }

    /// Clear the tonemap output to black. Used when no clips are active
    /// to skip the full tonemap + master effect chain (Unity parity:
    /// CompositorStack returns immediately for empty playback).
    pub fn clear(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Tonemap Clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.output.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// Resize the tonemap output buffer. Matches Unity's lazy reallocation in
    /// ApplyTonemap() when hdrSource dimensions change.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.output.resize(device, width, height);
    }

    /// HAL path: encode tonemap render pass via hal command encoder.
    /// Writes uniforms directly to shared-memory buffer (no API call).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    pub(crate) unsafe fn apply_hal(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        hdr_source_hal_view: &crate::hal_context::MetalTextureView,
        output_hal_view: &crate::hal_context::MetalTextureView,
        settings: &TonemapSettings,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let mode = if settings.hdr_output_enabled { 3u32 } else { 0u32 };
        let uniforms = TonemapUniforms {
            exposure: settings.exposure,
            paper_white: settings.paper_white_nits,
            max_nits: settings.max_display_nits,
            mode,
        };

        // Direct memcpy to shared-memory uniform buffer
        if let Some(mapped_ptr) = self.hal_uniform_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&uniforms).as_ptr(),
                    mapped_ptr,
                    std::mem::size_of::<TonemapUniforms>(),
                );
            }
        }

        let hal_pipe = self.hal_pipeline.as_ref().expect("tonemap hal pipeline");
        let hal_samp = self.hal_sampler.as_ref().expect("tonemap hal sampler");
        let hal_ubo = unsafe { &*self.hal_uniform_buf_ptr.expect("tonemap hal ubo") };

        let hal_bg = unsafe {
            hal_ctx.device().create_bind_group(
                &hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 2, resource_index: 0, count: 1 },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_ubo,
                        0,
                        std::num::NonZero::new(
                            std::mem::size_of::<TonemapUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[hal::TextureBinding {
                        view: hdr_source_hal_view,
                        usage: wgpu::wgt::TextureUses::RESOURCE,
                    }],
                    acceleration_structures: &[],
                    external_textures: &[],
                },
            )
            .expect("Failed to create hal tonemap bind group")
        };

        unsafe {
            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: Some("Tonemap Pass"),
                extent: wgpu::Extent3d {
                    width: self.output.width,
                    height: self.output.height,
                    depth_or_array_layers: 1,
                },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: output_hal_view,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops: hal::AttachmentOps::STORE,
                    clear_value: wgpu::Color::BLACK,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.set_render_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(&hal_pipe.pipeline_layout, 0, &hal_bg, &[]);
            hal_enc.draw(0, 3, 0, 1);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }

    /// HAL path: clear tonemap output to black.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    pub(crate) unsafe fn clear_hal(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        output_hal_view: &crate::hal_context::MetalTextureView,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _};
        unsafe {
            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: Some("Tonemap Clear"),
                extent: wgpu::Extent3d {
                    width: self.output.width,
                    height: self.output.height,
                    depth_or_array_layers: 1,
                },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: output_hal_view,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops: hal::AttachmentOps::STORE,
                    clear_value: wgpu::Color::BLACK,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.end_render_pass();
        }
    }
}
