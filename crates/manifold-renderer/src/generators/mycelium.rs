// Simplified physarum approximation via stateful ping-pong fragment shader.
// Full agent-based compute pipeline with 500K agents deferred to later pass.

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::stateful_base::StatefulState;

// Parameter indices matching types.rs param_defs
const SENS_DIST: usize = 0;
const SENS_ANGLE: usize = 1;
const TURN: usize = 2;
const STEP: usize = 3;
const DEPOSIT: usize = 4;
const DECAY: usize = 5;
const COLOR: usize = 6;
const GLOW: usize = 7;
const REACTIVITY: usize = 8;
// AGENTS (index 9) — affects visual density in fragment approximation
const SCALE: usize = 10;
const SEEDS: usize = 11;

const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MyceliumUniforms {
    time_val: f32,
    sens_dist: f32,
    sens_angle: f32,
    turn: f32,
    step_size: f32,
    deposit: f32,
    decay: f32,
    color_hue: f32,
    glow: f32,
    reactivity: f32,
    scale: f32,
    seeds: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    color_hue: f32,
    glow: f32,
    uv_scale: f32,
    _pad: f32,
}

pub struct MyceliumGenerator {
    state: Option<StatefulState>,
    sim_pipeline: wgpu::RenderPipeline,
    sim_bgl: wgpu::BindGroupLayout,
    sim_uniform_buffer: wgpu::Buffer,
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,
    display_uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

impl MyceliumGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Mycelium Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // ── Simulation pipeline ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Sim Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mycelium.wgsl").into(),
            ),
        });

        let sim_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Sim BGL"),
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
            label: Some("Mycelium Sim Layout"),
            bind_group_layouts: &[&sim_bgl],
            immediate_size: 0,
        });

        let sim_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mycelium Sim Pipeline"),
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
            label: Some("Mycelium Sim Uniforms"),
            size: std::mem::size_of::<MyceliumUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Display pipeline (state → output with HSV coloring) ──
        let display_shader_src = r#"
struct Uniforms {
    color_hue: f32,
    glow: f32,
    uv_scale: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let hp = h * 6.0;
    let x = c * (1.0 - abs(hp % 2.0 - 1.0));
    var rgb = vec3<f32>(0.0);
    if hp < 1.0 { rgb = vec3<f32>(c, x, 0.0); }
    else if hp < 2.0 { rgb = vec3<f32>(x, c, 0.0); }
    else if hp < 3.0 { rgb = vec3<f32>(0.0, c, x); }
    else if hp < 4.0 { rgb = vec3<f32>(0.0, x, c); }
    else if hp < 5.0 { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    return rgb + vec3<f32>(v - c);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = (in.uv - vec2<f32>(0.5)) * u.uv_scale + vec2<f32>(0.5);
    let c = textureSample(state_tex, state_sampler, uv);
    let trail = c.r;
    let display_lum = pow(max(trail, 0.001), 1.0 / max(u.glow, 0.1));
    let col = hsv2rgb(u.color_hue, 0.7 * trail, display_lum);
    let lum = col.r * 0.3 + col.g * 0.59 + col.b * 0.11;
    return vec4<f32>(lum, lum, lum, lum);
}
"#;

        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mycelium Display Shader"),
            source: wgpu::ShaderSource::Wgsl(display_shader_src.into()),
        });

        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mycelium Display BGL"),
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
            label: Some("Mycelium Display Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mycelium Display Pipeline"),
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
            label: Some("Mycelium Display Uniforms"),
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
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if self.state.is_none() {
            self.state = Some(StatefulState::new(
                device, iw, ih, STATE_FORMAT, "Mycelium",
            ));
        }
    }
}

impl Generator for MyceliumGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::Mycelium
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        let iw = (ctx.width / 2).max(1);
        let ih = (ctx.height / 2).max(1);
        self.ensure_state(device, iw, ih);

        let sens_dist = if ctx.param_count > SENS_DIST as u32 { ctx.params[SENS_DIST] } else { 0.02 };
        let sens_angle = if ctx.param_count > SENS_ANGLE as u32 { ctx.params[SENS_ANGLE] } else { 0.8 };
        let turn = if ctx.param_count > TURN as u32 { ctx.params[TURN] } else { 0.4 };
        let step = if ctx.param_count > STEP as u32 { ctx.params[STEP] } else { 0.001 };
        let deposit = if ctx.param_count > DEPOSIT as u32 { ctx.params[DEPOSIT] } else { 1.5 };
        let decay = if ctx.param_count > DECAY as u32 { ctx.params[DECAY] } else { 0.98 };
        let color_hue = if ctx.param_count > COLOR as u32 { ctx.params[COLOR] } else { 0.08 };
        let glow = if ctx.param_count > GLOW as u32 { ctx.params[GLOW] } else { 1.0 };
        let reactivity = if ctx.param_count > REACTIVITY as u32 { ctx.params[REACTIVITY] } else { 0.5 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let seeds = if ctx.param_count > SEEDS as u32 { ctx.params[SEEDS] } else { 1.0 };

        let texel_x = 1.0 / iw as f32;
        let texel_y = 1.0 / ih as f32;

        let uniforms = MyceliumUniforms {
            time_val: ctx.time,
            sens_dist,
            sens_angle,
            turn,
            step_size: step,
            deposit,
            decay,
            color_hue,
            glow,
            reactivity,
            scale,
            seeds,
            texel_x,
            texel_y,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(&self.sim_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let state = self.state.as_mut().unwrap();

        // Simulation pass
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Sim BG"),
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
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mycelium Sim Pass"),
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
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.sim_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        state.swap();

        // Display pass
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let display_uniforms = DisplayUniforms {
            color_hue,
            glow,
            uv_scale,
            _pad: 0.0,
        };
        queue.write_buffer(&self.display_uniform_buffer, 0, bytemuck::bytes_of(&display_uniforms));

        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mycelium Display BG"),
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
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mycelium Display Pass"),
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
            pass.set_pipeline(&self.display_pipeline);
            pass.set_bind_group(0, &display_bg, &[]);
            pass.draw(0..3, 0..1);
        }

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
