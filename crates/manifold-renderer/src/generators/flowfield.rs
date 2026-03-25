use super::stateful_base::StatefulState;
use crate::blit::BlitPipeline;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices matching types.rs param_defs
const NOISE: usize = 0;
const CURL: usize = 1;
const DECAY: usize = 2;
const SPEED: usize = 3;
const SCALE: usize = 4;
const SNAP: usize = 5;

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// Snap presets: cycled by trigger_count
const PRESET_NOISE: [f32; 6] = [2.0, 4.0, 7.0, 1.5, 8.0, 10.0];
const PRESET_CURL: [f32; 6] = [0.8, 0.4, 1.6, 0.3, 1.0, 1.8];

/// BGL entries for the hal pipeline:
///   binding 0: uniform (dynamic offset)
///   binding 1: texture_2d filterable (read state)
///   binding 2: sampler (filtering)
///   binding 3: storage_texture (write state)
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const HAL_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
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
            format: STATE_FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FlowfieldUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    noise_scale: f32,
    curl_intensity: f32,
    decay: f32,
    texel_x: f32,
    texel_y: f32,
    _pad: [f32; 2],
}

pub struct FlowfieldGenerator {
    state: Option<StatefulState>,
    #[allow(dead_code)] // render fallback for non-hal builds
    pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    bgl: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blit: BlitPipeline,
    // Compute sim pipeline (macOS + hal-encoding)
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    compute_bgl: wgpu::BindGroupLayout,
    // HAL pipeline for zero-overhead dispatch
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_sampler: Option<crate::hal_context::MetalSampler>,
}

impl FlowfieldGenerator {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Flowfield Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Flowfield Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/flowfield.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flowfield BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
            label: Some("Flowfield Pipeline Layout"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        // Simulation renders into STATE_FORMAT (Rgba16Float)
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Flowfield Pipeline"),
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
                    format: STATE_FORMAT,
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

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Flowfield Uniforms"),
            size: std::mem::size_of::<FlowfieldUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Compute simulation pipeline (macOS + hal-encoding) ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (compute_pipeline, compute_bgl) = {
            let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Flowfield Compute Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/flowfield_compute.wgsl").into(),
                ),
            });

            let cbgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Flowfield Compute BGL"),
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
                            format: STATE_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });

            let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Flowfield Compute Layout"),
                bind_group_layouts: &[&cbgl],
                immediate_size: 0,
            });

            let cpipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Flowfield Compute Pipeline"),
                layout: Some(&compute_layout),
                module: &compute_shader,
                entry_point: Some("cs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });

            (cpipeline, cbgl)
        };

        // ── HAL pipeline for zero-overhead dispatch ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler) = if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            let hal_pipe = crate::hal_pipeline::create_compute_pipeline(
                ctx,
                include_str!("shaders/flowfield_compute.wgsl"),
                "cs_main",
                &HAL_BGL_ENTRIES,
                "Flowfield HAL",
            );

            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some("Flowfield Sampler HAL"),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create Flowfield hal sampler")
            };

            (Some(hal_pipe), Some(hal_samp))
        } else {
            (None, None)
        };

        // Blit pipeline to upscale half-res state to full-res output
        let blit = BlitPipeline::new(device, target_format);

        Self {
            state: None,
            pipeline,
            bgl,
            uniform_buffer,
            sampler,
            blit,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            compute_bgl,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if self.state.is_none() {
            self.state = Some(StatefulState::new(
                device,
                iw,
                ih,
                STATE_FORMAT,
                "Flowfield",
            ));
        }
    }
}

impl Generator for FlowfieldGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FLOWFIELD
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        let iw = (ctx.width / 2).max(1);
        let ih = (ctx.height / 2).max(1);
        self.ensure_state(gpu.device, iw, ih);
        let state = self.state.as_mut().unwrap();

        let mut noise_scale = if ctx.param_count > NOISE as u32 {
            ctx.params[NOISE]
        } else {
            1.5
        };
        let mut curl_intensity = if ctx.param_count > CURL as u32 {
            ctx.params[CURL]
        } else {
            0.3
        };
        let decay = if ctx.param_count > DECAY as u32 {
            ctx.params[DECAY]
        } else {
            0.97
        };
        let speed = if ctx.param_count > SPEED as u32 {
            ctx.params[SPEED]
        } else {
            1.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };
        let snap = if ctx.param_count > SNAP as u32 {
            ctx.params[SNAP]
        } else {
            1.0
        };

        // Snap presets override noise and curl based on trigger_count
        if snap > 0.5 {
            let idx = (ctx.trigger_count as usize) % PRESET_NOISE.len();
            noise_scale = PRESET_NOISE[idx];
            curl_intensity = PRESET_CURL[idx];
        }

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let texel_x = 1.0 / iw as f32;
        let texel_y = 1.0 / ih as f32;

        let uniforms = FlowfieldUniforms {
            time: ctx.time,
            beat: ctx.beat,
            aspect_ratio: ctx.aspect,
            anim_speed: speed,
            uv_scale,
            noise_scale,
            curl_intensity,
            decay,
            texel_x,
            texel_y,
            _pad: [0.0; 2],
        };

        // ── HAL dispatch path ──────────────────────────────────────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_pipeline
            && let Some(ref hal_samp) = self.hal_sampler
            && gpu.has_hal_encoder()
        {
            use wgpu::hal::{self, Device as HalDevice};
            use crate::hal_dispatch::*;

            let offset = unsafe { gpu.uniform_arena_mut() }
                .expect("uniform_arena not set")
                .push(&uniforms);

            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();

            let arena_buf_ptr = unsafe { gpu.uniform_arena_mut() }
                .unwrap()
                .hal_buffer_ptr()
                .expect("arena hal buffer not available");
            let read_ptr = unsafe { extract_hal_view(state.read_view()) };
            let write_ptr = unsafe { extract_hal_view(state.write_view()) };

            let uniform_size = std::mem::size_of::<FlowfieldUniforms>() as u64;

            let bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
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
                            &*arena_buf_ptr,
                            0,
                            std::num::NonZero::new(uniform_size),
                        )],
                        samplers: &[hal_samp],
                        textures: &[
                            hal::TextureBinding {
                                view: &*read_ptr,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: &*write_ptr,
                                usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                            },
                        ],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect("Failed to create Flowfield hal bind group")
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [iw.div_ceil(16), ih.div_ceil(16), 1],
                    "Flowfield Sim Compute",
                );
            }

            state.swap();

            // Blit half-res state to full-res output (with bilinear upscale)
            self.blit.blit(
                gpu.device,
                gpu.encoder,
                state.read_view(),
                target,
                ctx.width,
                ctx.height,
            );

            return ctx.anim_progress;
        }

        // ── wgpu compute path (macOS fallback) ─────────────────────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            gpu.queue
                .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flowfield Compute BG"),
                layout: &self.compute_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(state.read_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(state.write_view()),
                    },
                ],
            });

            {
                let ts =
                    profiler.and_then(|p| p.compute_timestamps("Flowfield Sim Compute", iw, ih));
                let mut pass = gpu
                    .encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("Flowfield Sim Compute Pass"),
                        timestamp_writes: ts,
                    });
                pass.set_pipeline(&self.compute_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups(iw.div_ceil(16), ih.div_ceil(16), 1);
            }

            state.swap();
        }

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            gpu.queue
                .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            // Render pass fallback
            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flowfield BG"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(state.read_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            {
                let ts = profiler.and_then(|p| p.render_timestamps("Flowfield Sim", iw, ih));
                let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Flowfield Sim Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: state.write_view(),
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
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

            state.swap();
        }

        // Blit half-res state to full-res output (with bilinear upscale)
        self.blit.blit(
            gpu.device,
            gpu.encoder,
            state.read_view(),
            target,
            ctx.width,
            ctx.height,
        );

        ctx.anim_progress
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if let Some(ref mut state) = self.state {
            state.resize(device, iw, ih);
        }
    }
}
