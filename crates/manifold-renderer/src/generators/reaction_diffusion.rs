use super::stateful_base::StatefulState;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices matching types.rs param_defs
const FEED: usize = 0;
const KILL: usize = 1;
const SPEED: usize = 2;
const SCALE: usize = 3;

const STEPS_PER_FRAME: u32 = 8;
// Unity: ARGBFloat (Rgba32Float), but Rgba32Float is NOT filterable on Metal.
// textureSample requires filterable; Rgba16Float is the approved Metal fallback.
// See docs/KNOWN_DIVERGENCES.md.
const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimUniforms {
    time: f32,
    feed: f32,
    kill: f32,
    anim_speed: f32,
    uv_scale: f32,
    texel_x: f32,
    texel_y: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    uv_scale: f32,
    _pad: [f32; 3],
}

pub struct ReactionDiffusionGenerator {
    state: Option<StatefulState>,
    #[allow(dead_code)] // render fallback for non-hal builds
    sim_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    sim_bgl: wgpu::BindGroupLayout,
    sim_uniform_buffer: wgpu::Buffer,
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,
    display_uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    // Compute sim pipeline (macOS + hal-encoding)
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    sim_compute_pipeline: wgpu::ComputePipeline,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    sim_compute_bgl: wgpu::BindGroupLayout,
}

impl ReactionDiffusionGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("RD Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // ── Simulation pipeline ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RD Sim Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/reaction_diffusion_sim.wgsl").into(),
            ),
        });

        let sim_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RD Sim BGL"),
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

        let sim_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RD Sim Pipeline Layout"),
            bind_group_layouts: &[&sim_bgl],
            immediate_size: 0,
        });

        let sim_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RD Sim Pipeline"),
            layout: Some(&sim_layout),
            vertex: wgpu::VertexState {
                module: &sim_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sim_shader,
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

        let sim_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RD Sim Uniforms"),
            size: std::mem::size_of::<SimUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Compute simulation pipeline (macOS + hal-encoding) ──
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (sim_compute_pipeline, sim_compute_bgl) = {
            let compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("RD Sim Compute Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("shaders/reaction_diffusion_sim_compute.wgsl").into(),
                ),
            });

            let compute_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("RD Sim Compute BGL"),
                entries: &[
                    // binding 0: uniforms
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
                    // binding 1: source texture (filterable)
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
                    // binding 2: sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // binding 3: output storage texture
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
                label: Some("RD Sim Compute Layout"),
                bind_group_layouts: &[&compute_bgl],
                immediate_size: 0,
            });

            let compute_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("RD Sim Compute Pipeline"),
                    layout: Some(&compute_layout),
                    module: &compute_shader,
                    entry_point: Some("cs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                });

            (compute_pipeline, compute_bgl)
        };

        // ── Display pipeline ──
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RD Display Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/reaction_diffusion_display.wgsl").into(),
            ),
        });

        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RD Display BGL"),
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

        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RD Display Pipeline Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("RD Display Pipeline"),
            layout: Some(&display_layout),
            vertex: wgpu::VertexState {
                module: &display_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &display_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
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

        let display_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RD Display Uniforms"),
            size: std::mem::size_of::<DisplayUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            state: None,
            sim_pipeline,
            sim_bgl,
            sim_uniform_buffer,
            display_pipeline,
            display_bgl,
            display_uniform_buffer,
            sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            sim_compute_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            sim_compute_bgl,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.state.is_none() {
            self.state = Some(StatefulState::new(
                device,
                width,
                height,
                STATE_FORMAT,
                "RD",
            ));
        }
    }
}

impl Generator for ReactionDiffusionGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::REACTION_DIFFUSION
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        // Internal resolution: full res (1.0)
        let w = ctx.width;
        let h = ctx.height;
        self.ensure_state(gpu.device, w, h);
        let state = self.state.as_mut().unwrap();

        let feed = if ctx.param_count > FEED as u32 {
            ctx.params[FEED]
        } else {
            0.055
        };
        let kill = if ctx.param_count > KILL as u32 {
            ctx.params[KILL]
        } else {
            0.062
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
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let texel_x = 1.0 / w as f32;
        let texel_y = 1.0 / h as f32;

        // Use time=0 for seeding on first frame
        let effective_time = if state.frame_count() == 0 {
            0.0
        } else {
            ctx.time
        };

        let sim_uniforms = SimUniforms {
            time: effective_time,
            feed,
            kill,
            anim_speed: speed,
            uv_scale,
            texel_x,
            texel_y,
            _pad: 0.0,
        };
        gpu.queue.write_buffer(
            &self.sim_uniform_buffer,
            0,
            bytemuck::bytes_of(&sim_uniforms),
        );

        // Run N simulation steps
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            // Compute dispatch path — eliminates TBDR tile overhead per sim step
            for _ in 0..STEPS_PER_FRAME {
                let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("RD Sim Compute BG"),
                    layout: &self.sim_compute_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.sim_uniform_buffer.as_entire_binding(),
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
                    let ts = profiler.and_then(|p| p.compute_timestamps("RD Sim Compute", w, h));
                    let mut pass = gpu
                        .encoder
                        .begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("RD Sim Compute Pass"),
                            timestamp_writes: ts,
                        });
                    pass.set_pipeline(&self.sim_compute_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
                }

                state.swap();
            }
        }

        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        {
            for _ in 0..STEPS_PER_FRAME {
                let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("RD Sim BG"),
                    layout: &self.sim_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.sim_uniform_buffer.as_entire_binding(),
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
                    let ts = profiler.and_then(|p| p.render_timestamps("RD Sim", w, h));
                    let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("RD Sim Pass"),
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
                    pass.set_pipeline(&self.sim_pipeline);
                    pass.set_bind_group(0, &bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }

                state.swap();
            }
        }

        // Display pass: read final state → write to output target
        let display_uniforms = DisplayUniforms {
            uv_scale,
            _pad: [0.0; 3],
        };
        gpu.queue.write_buffer(
            &self.display_uniform_buffer,
            0,
            bytemuck::bytes_of(&display_uniforms),
        );

        let display_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RD Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.display_uniform_buffer.as_entire_binding(),
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
            let ts =
                profiler.and_then(|p| p.render_timestamps("RD Display", ctx.width, ctx.height));
            let mut pass = gpu.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RD Display Pass"),
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
            pass.set_pipeline(&self.display_pipeline);
            pass.set_bind_group(0, &display_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.anim_progress
    }

    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if let Some(ref mut state) = self.state {
            state.resize(device, width, height);
        }
    }
}
