use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;

// Parameter indices matching Unity ComputeParametricSurfaceGenerator
const SHAPE: usize = 0;
const MORPH: usize = 1;
const SPEED: usize = 2;
const SCALE: usize = 3;
const SNAP: usize = 4;
const SURFACE_COUNT: u32 = 5;

const VOL_SIZE: u32 = 128;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BakeUniforms {
    shape: f32,
    morph: f32,
    vol_res: f32,
    _pad0: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RaymarchUniforms {
    time_val: f32,
    speed: f32,
    aspect_ratio: f32,
    uv_scale: f32,
}

pub struct ParametricSurfaceGenerator {
    // Compute bake pipeline
    compute_pipeline: wgpu::ComputePipeline,
    compute_bgl: wgpu::BindGroupLayout,
    bake_uniform_buffer: wgpu::Buffer,
    // 3D volume texture (keep texture alive for GPU lifetime; view is used for binding)
    #[allow(dead_code)]
    volume_texture: wgpu::Texture,
    volume_view: wgpu::TextureView,
    // Raymarch fragment pipeline
    raymarch_pipeline: wgpu::RenderPipeline,
    raymarch_bgl: wgpu::BindGroupLayout,
    raymarch_uniform_buffer: wgpu::Buffer,
    volume_sampler: wgpu::Sampler,
    // Dirty tracking: only re-bake when shape or morph changes (matches Unity ShouldRebake)
    last_shape: f32,
    last_morph: f32,
}

impl ParametricSurfaceGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // ── 3D Volume Texture ──
        // Unity: RenderTextureFormat.RHalf. Use Rgba16Float for filterable storage on all backends.
        // Bake writes vec4(d, 0, 0, 0); raymarch reads .r channel only.
        let volume_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ParametricSurface Volume"),
            size: wgpu::Extent3d {
                width: VOL_SIZE,
                height: VOL_SIZE,
                depth_or_array_layers: VOL_SIZE,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let volume_view = volume_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // ── Compute Bake Pipeline ──
        let bake_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ParametricSurface Bake Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/parametric_surface_bake.wgsl").into(),
            ),
        });

        let compute_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ParametricSurface Compute BGL"),
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
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
            ],
        });

        let compute_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ParametricSurface Compute Layout"),
            bind_group_layouts: &[&compute_bgl],
            immediate_size: 0,
        });

        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ParametricSurface Compute Pipeline"),
            layout: Some(&compute_layout),
            module: &bake_shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let bake_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ParametricSurface Bake Uniforms"),
            size: std::mem::size_of::<BakeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Raymarch Fragment Pipeline ──
        let raymarch_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ParametricSurface Raymarch Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/parametric_surface_raymarch.wgsl").into(),
            ),
        });

        let volume_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ParametricSurface Volume Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let raymarch_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ParametricSurface Raymarch BGL"),
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
                        view_dimension: wgpu::TextureViewDimension::D3,
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

        let raymarch_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ParametricSurface Raymarch Layout"),
            bind_group_layouts: &[&raymarch_bgl],
            immediate_size: 0,
        });

        let raymarch_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ParametricSurface Raymarch Pipeline"),
            layout: Some(&raymarch_layout),
            vertex: wgpu::VertexState {
                module: &raymarch_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &raymarch_shader,
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

        let raymarch_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ParametricSurface Raymarch Uniforms"),
            size: std::mem::size_of::<RaymarchUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            compute_pipeline,
            compute_bgl,
            bake_uniform_buffer,
            volume_texture,
            volume_view,
            raymarch_pipeline,
            raymarch_bgl,
            raymarch_uniform_buffer,
            volume_sampler,
            last_shape: f32::MIN,
            last_morph: f32::MIN,
        }
    }

    // Matches Unity: Mathf.Approximately uses ~0.00001 epsilon
    fn needs_rebake(&self, shape: f32, morph: f32) -> bool {
        (self.last_shape - shape).abs() > 0.00001
            || (self.last_morph - morph).abs() > 0.00001
    }

    // Matches Unity ResolveShape: when snap > 0.5, shape = trigger_count % SURFACE_COUNT
    fn resolve_shape(ctx: &GeneratorContext) -> f32 {
        let snap = if ctx.param_count > SNAP as u32 { ctx.params[SNAP] } else { 0.0 };
        if snap > 0.5 {
            (ctx.trigger_count % SURFACE_COUNT) as f32
        } else {
            if ctx.param_count > SHAPE as u32 { ctx.params[SHAPE] } else { 0.0 }
        }
    }
}

impl Generator for ParametricSurfaceGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::ParametricSurface
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let shape = Self::resolve_shape(ctx);
        let morph = if ctx.param_count > MORPH as u32 { ctx.params[MORPH] } else { 0.0 };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };

        // UV scale: matches Unity base class — rawScale > 0 ? 1/rawScale : 1
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        // Compute bake pass — only when shape or morph change (static per shape/morph)
        if self.needs_rebake(shape, morph) {
            let bake_uniforms = BakeUniforms {
                shape: shape.clamp(0.0, 4.0),
                morph: morph.clamp(0.0, 1.0),
                vol_res: VOL_SIZE as f32,
                _pad0: 0.0,
            };
            queue.write_buffer(&self.bake_uniform_buffer, 0, bytemuck::bytes_of(&bake_uniforms));

            let compute_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ParametricSurface Compute BG"),
                layout: &self.compute_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.bake_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.volume_view),
                    },
                ],
            });

            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("ParametricSurface Bake Pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.compute_pipeline);
                pass.set_bind_group(0, &compute_bg, &[]);
                // 128 / 4 workgroup_size = 32 workgroups per axis
                pass.dispatch_workgroups(32, 32, 32);
            }

            self.last_shape = shape;
            self.last_morph = morph;
        }

        // Raymarch pass — every frame (camera orbits with time)
        let raymarch_uniforms = RaymarchUniforms {
            time_val: ctx.time,
            speed,
            aspect_ratio: ctx.aspect,
            uv_scale,
        };
        queue.write_buffer(&self.raymarch_uniform_buffer, 0, bytemuck::bytes_of(&raymarch_uniforms));

        let raymarch_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ParametricSurface Raymarch BG"),
            layout: &self.raymarch_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.raymarch_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.volume_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.volume_sampler),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ParametricSurface Raymarch Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            pass.set_pipeline(&self.raymarch_pipeline);
            pass.set_bind_group(0, &raymarch_bg, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Volume texture is fixed at 128^3; no resize needed
    }
}
