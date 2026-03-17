use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;

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

pub struct ConcentricTunnelGenerator {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
}

impl ConcentricTunnelGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ConcentricTunnel Generator"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/concentric_tunnel.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ConcentricTunnel BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ConcentricTunnel Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ConcentricTunnel Pipeline"),
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

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ConcentricTunnel Uniforms"),
            size: std::mem::size_of::<ConcentricTunnelUniforms>() as u64,
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

impl Generator for ConcentricTunnelGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::ConcentricTunnel
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
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
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ConcentricTunnel BG"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.uniform_buffer.as_entire_binding(),
            }],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ConcentricTunnel Pass"),
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
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Fragment shader generators have no resolution-dependent resources
    }
}
