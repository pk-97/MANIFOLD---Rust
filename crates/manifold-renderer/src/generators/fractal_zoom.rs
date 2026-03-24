use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;

// Parameter indices matching Unity's FractalZoomGenerator.cs
const SPEED: usize = 0;
const SCALE: usize = 1;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FractalZoomUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    trigger_count: f32,
    _pad: [f32; 2],
}

pub struct FractalZoomGenerator {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
}

impl FractalZoomGenerator {
    pub fn new(device: &wgpu::Device, _target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FractalZoom Compute Generator"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fractal_zoom_compute.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FractalZoom BGL"),
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
            label: Some("FractalZoom Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("FractalZoom Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FractalZoom Uniforms"),
            size: std::mem::size_of::<FractalZoomUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            uniform_buffer,
        }
    }
}

impl Generator for FractalZoomGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FRACTAL_ZOOM
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };

        let uniforms = FractalZoomUniforms {
            time: ctx.time,
            beat: ctx.beat,
            aspect_ratio: ctx.aspect,
            anim_speed: speed,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            trigger_count: ctx.trigger_count as f32,
            _pad: [0.0; 2],
        };
        gpu.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("FractalZoom BG"),
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
                p.compute_timestamps("FractalZoom", ctx.width, ctx.height)
            });
            let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("FractalZoom Pass"),
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
