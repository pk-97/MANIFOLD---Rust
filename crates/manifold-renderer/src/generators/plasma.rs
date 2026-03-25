use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;

// Parameter indices matching Unity's PlasmaGenerator.cs
const PATTERN: usize = 0;
const COMPLEXITY: usize = 1;
const CONTRAST: usize = 2;
const SPEED: usize = 3;
const SCALE: usize = 4;
const SNAP: usize = 5;
const PATTERN_COUNT: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlasmaUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    pattern_type: f32,
    complexity: f32,
    contrast: f32,
    trigger_count: f32,
    _pad: [f32; 3],
}

/// BGL entries for the hal pipeline (dynamic offset uniform + storage texture).
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
const HAL_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 2] = [
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
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

pub struct PlasmaGenerator {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
}

impl PlasmaGenerator {
    pub fn new(
        device: &wgpu::Device,
        _target_format: wgpu::TextureFormat,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off

        let shader_source = include_str!("shaders/plasma_compute.wgsl");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Plasma Generator"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Plasma BGL"),
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
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Plasma Pipeline Layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Plasma Pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("cs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Plasma Uniforms"),
            size: std::mem::size_of::<PlasmaUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create hal pipeline for zero-overhead dispatch
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let hal_pipeline = hal_ctx.map(|ctx| {
            crate::hal_pipeline::create_compute_pipeline(
                ctx,
                shader_source,
                "cs_main",
                &HAL_BGL_ENTRIES,
                "Plasma HAL",
            )
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
        }
    }
}

impl Generator for PlasmaGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::PLASMA
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        if ctx.param_count == 0 {
            return ctx.anim_progress;
        }

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
        let snap = ctx.param_count > SNAP as u32 && ctx.params[SNAP] > 0.5;

        let pattern_type = if snap {
            (ctx.trigger_count % PATTERN_COUNT) as f32
        } else if ctx.param_count > PATTERN as u32 {
            ctx.params[PATTERN].round()
        } else {
            0.0
        };

        let uniforms = PlasmaUniforms {
            time: ctx.time,
            beat: ctx.beat,
            aspect_ratio: ctx.aspect,
            anim_speed: speed,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            pattern_type,
            complexity: if ctx.param_count > COMPLEXITY as u32 {
                ctx.params[COMPLEXITY]
            } else {
                0.5
            },
            contrast: if ctx.param_count > CONTRAST as u32 {
                ctx.params[CONTRAST]
            } else {
                0.5
            },
            trigger_count: ctx.trigger_count as f32,
            _pad: [0.0; 3],
        };

        // ── HAL dispatch path ──────────────────────────────────────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_pipeline
            && gpu.has_hal_encoder()
        {
            use wgpu::hal::{self, Device as HalDevice};
            use crate::hal_dispatch::*;

            // Push uniforms to shared-memory arena (zero staging)
            let offset = unsafe { gpu.uniform_arena_mut() }
                .expect("uniform_arena not set")
                .push(&uniforms);

            let (hal_enc, hal_ctx) =
                unsafe { gpu.hal_encoder_mut() }.unwrap();

            // Extract hal pointers
            let arena_buf_ptr = unsafe { gpu.uniform_arena_mut() }
                .unwrap()
                .hal_buffer_ptr()
                .expect("arena hal buffer not available");
            let target_ptr = unsafe { extract_hal_view(target) };

            let uniform_size = std::mem::size_of::<PlasmaUniforms>() as u64;

            // Create ephemeral hal bind group
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
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            &*arena_buf_ptr,
                            0,
                            std::num::NonZero::new(uniform_size),
                        )],
                        samplers: &[],
                        textures: &[hal::TextureBinding {
                            view: &*target_ptr,
                            usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                        }],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect("Failed to create Plasma hal bind group")
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
                    "Plasma Compute",
                );
            }

            return ctx.anim_progress;
        }

        // ── wgpu dispatch path (fallback) ──────────────────────────────
        gpu.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group =
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Plasma BG"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(target),
                    },
                ],
            });

        {
            let ts = profiler
                .and_then(|p| p.compute_timestamps("Plasma", ctx.width, ctx.height));
            let mut pass =
                gpu.encoder
                    .begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("Plasma Compute"),
                        timestamp_writes: ts,
                    });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                ctx.width.div_ceil(16),
                ctx.height.div_ceil(16),
                1,
            );
        }

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Compute generators have no resolution-dependent resources
    }
}
