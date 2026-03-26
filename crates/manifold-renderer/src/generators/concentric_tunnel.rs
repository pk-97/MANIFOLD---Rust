use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;

// Parameter indices matching Unity's ConcentricTunnelGenerator.cs
const SHAPE: usize = 0;
const LINE: usize = 1;
const SPEED: usize = 2;
const SCALE: usize = 3;
const SNAP: usize = 4;
const SNAP_MODE: usize = 5;
const SHAPE_COUNT: u32 = 6;

const MODE_SHAPE: i32 = 0;
const MODE_SPAWN: i32 = 1;
const MODE_BOTH: i32 = 2;

// Speed param (0-4 integer) maps to beat fractions
const BEAT_VALUES: [f32; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConcentricTunnelUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    line_thickness: f32,
    anim_speed: f32,
    uv_scale: f32,
    shape_type: f32,
    snap_mode: f32,
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

pub struct ConcentricTunnelGenerator {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(target_os = "macos")]
    native_pipeline: Option<manifold_gpu::GpuComputePipeline>,
}

impl ConcentricTunnelGenerator {
    pub fn new(
        device: &wgpu::Device,
        _target_format: wgpu::TextureFormat,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off

        let shader_source = include_str!("shaders/concentric_tunnel_compute.wgsl");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ConcentricTunnel Compute Generator"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ConcentricTunnel BGL"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ConcentricTunnel Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ConcentricTunnel Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ConcentricTunnel Uniforms"),
            size: std::mem::size_of::<ConcentricTunnelUniforms>() as u64,
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
                "ConcentricTunnel HAL",
            )
        });

        #[cfg(target_os = "macos")]
        let native_pipeline = native_device.map(|dev| {
            dev.create_compute_pipeline(shader_source, "cs_main", "ConcentricTunnel Native")
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(target_os = "macos")]
            native_pipeline,
        }
    }
}

impl Generator for ConcentricTunnelGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::CONCENTRIC_TUNNEL
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

        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.008 };
        let speed_idx = if ctx.param_count > SPEED as u32 {
            (ctx.params[SPEED].round() as usize).min(BEAT_VALUES.len() - 1)
        } else { 2 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let snap_on = ctx.param_count > SNAP as u32 && ctx.params[SNAP] > 0.5;
        let mode = if ctx.param_count > SNAP_MODE as u32 {
            (ctx.params[SNAP_MODE].round() as i32).clamp(MODE_SHAPE, MODE_BOTH)
        } else { MODE_SHAPE };

        let anim_speed = BEAT_VALUES[speed_idx];

        // SNAP logic (Unity: ConcentricTunnelGenerator.cs lines 46-68)
        let mut snap_mode_shader = 0.0_f32;
        let shape = if snap_on {
            let cycle_shape = mode == MODE_SHAPE || mode == MODE_BOTH;
            let spawn_rings = mode == MODE_SPAWN || mode == MODE_BOTH;

            if spawn_rings {
                snap_mode_shader = if mode == MODE_BOTH { 2.0 } else { 1.0 };
            }

            if cycle_shape {
                (ctx.trigger_count % SHAPE_COUNT) as f32
            } else {
                if ctx.param_count > SHAPE as u32 { ctx.params[SHAPE].round() } else { 0.0 }
            }
        } else {
            if ctx.param_count > SHAPE as u32 { ctx.params[SHAPE].round() } else { 0.0 }
        };

        let uniforms = ConcentricTunnelUniforms {
            time: ctx.time,
            beat: ctx.beat,
            aspect_ratio: ctx.aspect,
            line_thickness: line,
            anim_speed,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            shape_type: shape,
            snap_mode: snap_mode_shader,
            trigger_count: ctx.trigger_count as f32,
            _pad: [0.0; 3],
        };

        // ── NATIVE METAL dispatch path ─────────────────────────────────
        #[cfg(target_os = "macos")]
        if let Some(ref native_pipe) = self.native_pipeline
            && gpu.has_native_encoder()
            && let Some(native_target_ptr) = ctx.native_target
        {
            let native_target = unsafe { &*native_target_ptr };
            let native_enc = unsafe { gpu.native_encoder_mut() }.unwrap();
            native_enc.dispatch_compute(
                native_pipe,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: native_target,
                    },
                ],
                [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
                "ConcentricTunnel Compute",
            );
            return ctx.anim_progress;
        }

        // ── HAL dispatch path ──────────────────────────────────────────
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_pipe) = self.hal_pipeline
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
            let target_ptr = unsafe { extract_hal_view(target) };

            let uniform_size = std::mem::size_of::<ConcentricTunnelUniforms>() as u64;

            let bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
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
                .expect("Failed to create ConcentricTunnel hal bind group")
            };

            unsafe {
                dispatch_hal_compute(
                    hal_enc,
                    hal_ctx,
                    hal_pipe,
                    bg,
                    &[offset as u32],
                    [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
                    "ConcentricTunnel Compute",
                );
            }

            return ctx.anim_progress;
        }

        // ── wgpu dispatch path (fallback) ──────────────────────────────
        gpu.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ConcentricTunnel BG"),
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
            let ts = profiler.and_then(|p| {
                p.compute_timestamps("ConcentricTunnel", ctx.width, ctx.height)
            });
            let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ConcentricTunnel Pass"),
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
