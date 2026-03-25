use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FeedbackUniforms {
    feedback_amount: f32,
    _pad: [f32; 3],
}

/// BGL entries for the feedback render pipeline (shared between wgpu and hal).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const FEEDBACK_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
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
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
];

/// Per-owner state: the previous frame's feedback buffer.
struct FeedbackState {
    buffer: RenderTarget,
}

/// Feedback effect — lerps current frame with previous frame's state buffer.
/// Stateful: maintains one feedback buffer per owner (clip/layer/master).
pub struct FeedbackFX {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    states: AHashMap<i64, FeedbackState>,
    width: u32,
    height: u32,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_pipeline: Option<crate::hal_pipeline::HalRenderPipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_uniform_mapped_ptr: Option<*mut u8>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    hal_uniform_buf_ptr: Option<*const crate::hal_context::MetalBuffer>,
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for FeedbackFX {}

/// Clear a RenderTarget to transparent black (all zeros).
/// Unity ref: RenderTextureUtil.Clear() — zeros texture contents.
/// Uses `clear_texture()` instead of a render pass — avoids a full TBDR
/// tile load/store cycle for what is just a memset-to-zero.
fn clear_render_target(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
    encoder.clear_texture(texture, &wgpu::ImageSubresourceRange::default());
}

impl FeedbackFX {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx;
        let format = wgpu::TextureFormat::Rgba16Float;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Feedback"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/feedback.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Feedback BGL"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Feedback Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Feedback Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Feedback Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- hal pipeline + shared-memory uniform buffer ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler, uniform_buffer, hal_uniform_mapped_ptr,
             hal_uniform_buf_ptr) =
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;
            let hal_pipe = crate::hal_pipeline::create_render_pipeline(
                ctx,
                include_str!("shaders/feedback.wgsl"),
                "vs_main", "fs_main",
                &FEEDBACK_BGL_ENTRIES,
                wgpu::TextureFormat::Rgba16Float,
                "Feedback HAL",
            );
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("Feedback HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal feedback sampler")
            };
            let buf_size = std::mem::size_of::<FeedbackUniforms>() as u64;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("Feedback HAL"),
                        size: buf_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal feedback uniform buffer")
            };
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..buf_size)
                    .expect("Failed to map hal feedback uniform buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("Feedback Uniforms"),
                        size: buf_size,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };
            let buf_hal_ptr = {
                let guard = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("uniform buffer not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };
            (Some(hal_pipe), Some(hal_samp), wgpu_buf, Some(mapped_ptr),
             Some(buf_hal_ptr))
        } else {
            let wgpu_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Feedback Uniforms"),
                size: std::mem::size_of::<FeedbackUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (None, None, wgpu_buf, None, None)
        };

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Feedback Uniforms"),
            size: std::mem::size_of::<FeedbackUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            uniform_buffer,
            states: AHashMap::new(),
            width: 0,
            height: 0,
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

    /// Create state buffer and clear to black.
    /// Unity ref: GetOrCreateState + RenderTextureUtil.Clear()
    fn ensure_state(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, owner_key: i64) {
        if !self.states.contains_key(&owner_key) && self.width > 0 && self.height > 0 {
            let format = wgpu::TextureFormat::Rgba16Float;
            let buffer = RenderTarget::new(device, self.width, self.height, format, "Feedback State");
            // Clear to black so first-frame shader reads black prev buffer,
            // producing mix(current, black, amount) — matching Unity behavior.
            clear_render_target(encoder, &buffer.texture);
            self.states.insert(owner_key, FeedbackState { buffer });
        }
    }

    /// HAL path: encode the feedback render pass via hal command encoder.
    /// Writes uniforms directly to shared-memory buffer (no API call).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn apply_hal(
        &self,
        hal_enc: &mut crate::hal_context::MetalCommandEncoder,
        hal_ctx: &crate::hal_context::HalContext,
        source_hal_view: &crate::hal_context::MetalTextureView,
        prev_hal_view: &crate::hal_context::MetalTextureView,
        target_hal_view: &crate::hal_context::MetalTextureView,
        width: u32,
        height: u32,
        feedback_amount: f32,
    ) {
        use wgpu::hal::{self as hal, CommandEncoder as _, Device as _};

        let uniforms = FeedbackUniforms {
            feedback_amount,
            _pad: [0.0; 3],
        };

        // Direct memcpy to shared-memory uniform buffer
        if let Some(mapped_ptr) = self.hal_uniform_mapped_ptr {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&uniforms).as_ptr(),
                    mapped_ptr,
                    std::mem::size_of::<FeedbackUniforms>(),
                );
            }
        }

        let hal_pipe = self.hal_pipeline.as_ref().expect("feedback hal pipeline");
        let hal_samp = self.hal_sampler.as_ref().expect("feedback hal sampler");
        let hal_buf = unsafe {
            &*self.hal_uniform_buf_ptr.expect("feedback hal uniform buf")
        };

        let hal_bg = unsafe {
            hal_ctx.device().create_bind_group(
                &hal::BindGroupDescriptor {
                    label: None,
                    layout: &hal_pipe.bind_group_layout,
                    entries: &[
                        hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 2, resource_index: 0, count: 1 },
                        hal::BindGroupEntry { binding: 3, resource_index: 1, count: 1 },
                    ],
                    buffers: &[hal::BufferBinding::new_unchecked(
                        hal_buf,
                        0,
                        std::num::NonZero::new(
                            std::mem::size_of::<FeedbackUniforms>() as u64,
                        ),
                    )],
                    samplers: &[hal_samp],
                    textures: &[
                        hal::TextureBinding {
                            view: source_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                        hal::TextureBinding {
                            view: prev_hal_view,
                            usage: wgpu::wgt::TextureUses::RESOURCE,
                        },
                    ],
                    acceleration_structures: &[],
                    external_textures: &[],
                },
            )
            .expect("Failed to create hal feedback bind group")
        };

        unsafe {
            hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                label: None,
                extent: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                sample_count: 1,
                color_attachments: &[Some(hal::ColorAttachment {
                    target: hal::Attachment {
                        view: target_hal_view,
                        usage: wgpu::wgt::TextureUses::COLOR_TARGET,
                    },
                    resolve_target: None,
                    ops: hal::AttachmentOps::LOAD_CLEAR | hal::AttachmentOps::STORE,
                    clear_value: wgpu::Color::TRANSPARENT,
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            }).expect("hal begin_render_pass failed");
            hal_enc.set_render_pipeline(&hal_pipe.pipeline);
            hal_enc.set_bind_group(
                &hal_pipe.pipeline_layout, 0, &hal_bg,
                &[],
            );
            hal_enc.draw(0, 3, 0, 1);
            hal_enc.end_render_pass();
            hal_ctx.device().destroy_bind_group(hal_bg);
        }
    }
}

impl PostProcessEffect for FeedbackFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::FEEDBACK
    }

    fn supports_hal(&self) -> bool { true }

    // ShouldSkip: default (param[0] <= 0) — matches Unity SimpleBlitEffect.ShouldSkip.

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        target_texture: &wgpu::Texture,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        self.width = ctx.width;
        self.height = ctx.height;

        // --- hal path ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if gpu.has_hal_encoder() {
            type MetalApi = wgpu::hal::api::Metal;
            use wgpu::hal::CommandEncoder as _;

            // Ensure state buffer exists (clear via hal)
            if !self.states.contains_key(&ctx.owner_key) && self.width > 0 && self.height > 0 {
                let format = wgpu::TextureFormat::Rgba16Float;
                let buffer = RenderTarget::new(
                    gpu.device, self.width, self.height, format, "Feedback State",
                );
                // Clear via hal render pass
                let view_ptr = {
                    let g = unsafe { buffer.view.as_hal::<MetalApi>() }
                        .expect("feedback state not Metal");
                    &*g as *const _
                };
                let (hal_enc, _hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
                unsafe {
                    use wgpu::hal as hal;
                    hal_enc.begin_render_pass(&hal::RenderPassDescriptor {
                        label: Some("Clear Feedback State"),
                        extent: wgpu::Extent3d {
                            width: self.width, height: self.height,
                            depth_or_array_layers: 1,
                        },
                        sample_count: 1,
                        color_attachments: &[Some(hal::ColorAttachment {
                            target: hal::Attachment {
                                view: &*view_ptr,
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
                    }).expect("hal begin_render_pass failed");
                    hal_enc.end_render_pass();
                }
                self.states.insert(ctx.owner_key, FeedbackState { buffer });
            }

            let state = self.states.get(&ctx.owner_key).unwrap();
            let feedback_amount =
                fx.param_values.first().copied().unwrap_or(0.0).min(0.98);

            // Extract hal texture view pointers (sequential snatch lock)
            let source_ptr = {
                let g = unsafe { source.as_hal::<MetalApi>() }
                    .expect("source not Metal");
                &*g as *const _
            };
            let prev_ptr = {
                let g = unsafe { state.buffer.view.as_hal::<MetalApi>() }
                    .expect("prev not Metal");
                &*g as *const _
            };
            let target_ptr = {
                let g = unsafe { target.as_hal::<MetalApi>() }
                    .expect("target not Metal");
                &*g as *const _
            };

            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            unsafe {
                self.apply_hal(
                    hal_enc, hal_ctx,
                    &*source_ptr, &*prev_ptr, &*target_ptr,
                    ctx.width, ctx.height, feedback_amount,
                );
            }

            // PostBlit copy via hal: target → state buffer
            let target_tex_ptr = {
                let g = unsafe { target_texture.as_hal::<MetalApi>() }
                    .expect("target tex not Metal");
                &*g as *const _
            };
            let state_tex_ptr = {
                let g = unsafe { state.buffer.texture.as_hal::<MetalApi>() }
                    .expect("state tex not Metal");
                &*g as *const _
            };
            let (hal_enc, _) = unsafe { gpu.hal_encoder_mut() }.unwrap();
            unsafe {
                hal_enc.copy_texture_to_texture(
                    &*target_tex_ptr,
                    wgpu::wgt::TextureUses::COPY_SRC,
                    &*state_tex_ptr,
                    std::iter::once(wgpu::hal::TextureCopy {
                        src_base: wgpu::hal::TextureCopyBase {
                            mip_level: 0,
                            array_layer: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::hal::FormatAspects::COLOR,
                        },
                        dst_base: wgpu::hal::TextureCopyBase {
                            mip_level: 0,
                            array_layer: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::hal::FormatAspects::COLOR,
                        },
                        size: wgpu::hal::CopyExtent {
                            width: ctx.width,
                            height: ctx.height,
                            depth: 1,
                        },
                    }),
                );
            }
            return;
        }

        self.ensure_state(gpu.device, gpu.encoder, ctx.owner_key);

        let state = self.states.get(&ctx.owner_key).unwrap();

        // FeedbackFX.cs:34 — Mathf.Min(fx.GetParam(0), 0.98f)
        let feedback_amount = fx.param_values.first().copied().unwrap_or(0.0).min(0.98);
        let uniforms = FeedbackUniforms {
            feedback_amount,
            _pad: [0.0; 3],
        };

        gpu.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Feedback BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&state.buffer.view),
                },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.render_timestamps("Feedback Pass", ctx.width, ctx.height));
            let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Feedback Pass"),
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
            pass.draw(0..3, 0..1);
        }

        // PostBlit: copy blended result into feedback state buffer for next frame.
        // Unity ref: Graphics.CopyTexture(result, stateBuffer) — GPU memcpy, zero
        // shader cost. Replaces the old SimpleBlitHelper render pass.
        let state = self.states.get(&ctx.owner_key).unwrap();
        gpu.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &state.buffer.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ctx.width,
                height: ctx.height,
                depth_or_array_layers: 1,
            },
        );
    }

    // FeedbackFX.cs lines 42-46 — ClearState: zeros ALL state buffer contents.
    // Without an encoder, we remove entries so they get re-created (and cleared to
    // black) on the next ensure_state call.
    fn clear_state(&mut self) {
        self.states.clear();
    }

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.states.clear();
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
}

impl StatefulEffect for FeedbackFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        // Unity: RenderTextureUtil.Clear(rt) — zeros contents.
        // Remove entry so it re-creates cleared on next ensure_state.
        self.states.remove(&owner_key);
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.states.remove(&owner_key);
    }
    fn cleanup_all_owners(&mut self, _device: &wgpu::Device) { self.states.clear(); }
}
