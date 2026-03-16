use manifold_core::GeneratorType;
use crate::blit::BlitPipeline;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::stateful_base::StatefulState;

// Parameter indices matching types.rs param_defs
const TYPE: usize = 0;
const TRAIL: usize = 1;
const BRIGHT: usize = 2;
const CHAOS: usize = 3;
const SIZE: usize = 4;
const SPEED: usize = 5;
const SCALE: usize = 6;
// SNAP (index 7) handled at app layer via trigger_count

const PARTICLE_COUNT: usize = 384;
const RK2_STEPS: usize = 8;
const STATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// Per-attractor constants
const CENTERS: [[f32; 3]; 5] = [
    [0.0, 0.0, 25.0],
    [0.0, 0.0, 2.0],
    [0.0, 0.0, 0.5],
    [0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0],
];
const SCALES: [f32; 5] = [25.0, 10.0, 1.2, 4.0, 12.0];
const DTS: [f32; 5] = [0.003, 0.008, 0.008, 0.03, 0.004];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AttractorUniforms {
    decay: f32,
    brightness: f32,
    particle_size: f32,
    particle_count: f32,
    texel_x: f32,
    texel_y: f32,
    _pad: [f32; 2],
}

pub struct StrangeAttractorGenerator {
    state: Option<StatefulState>,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blit: BlitPipeline,
    // CPU-side trajectory state
    trajectories: Vec<[f32; 3]>,
    // GPU texture for projected 2D positions (384x1, Rg32Float)
    position_texture: Option<wgpu::Texture>,
    position_view: Option<wgpu::TextureView>,
    // CPU buffer for position upload (384 * 2 floats = RG per pixel)
    position_data: Vec<f32>,
    // Track current attractor type to reinit on change
    current_type: i32,
}

impl StrangeAttractorGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Attractor Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("StrangeAttractor Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/strange_attractor.wgsl").into(),
            ),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Attractor BGL"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Attractor Pipeline Layout"),
            bind_group_layouts: &[&bgl],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Attractor Pipeline"),
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
            label: Some("Attractor Uniforms"),
            size: std::mem::size_of::<AttractorUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let blit = BlitPipeline::new(device, target_format);

        // Initialize trajectories with small random perturbations around center
        let mut trajectories = Vec::with_capacity(PARTICLE_COUNT);
        for i in 0..PARTICLE_COUNT {
            let fi = i as f32;
            // Deterministic pseudo-random spread
            let hash = ((fi * 127.1).sin() * 43758.5453).fract();
            let hash2 = ((fi * 269.5).sin() * 43758.5453).fract();
            let hash3 = ((fi * 419.2).sin() * 43758.5453).fract();
            trajectories.push([
                CENTERS[0][0] + (hash - 0.5) * 0.1,
                CENTERS[0][1] + (hash2 - 0.5) * 0.1,
                CENTERS[0][2] + (hash3 - 0.5) * 0.1,
            ]);
        }

        let position_data = vec![0.0f32; PARTICLE_COUNT * 4]; // RGBA per pixel

        Self {
            state: None,
            pipeline,
            bgl,
            uniform_buffer,
            sampler,
            blit,
            trajectories,
            position_texture: None,
            position_view: None,
            position_data,
            current_type: 0,
        }
    }

    fn ensure_state(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let iw = (width / 2).max(1);
        let ih = (height / 2).max(1);
        if self.state.is_none() {
            self.state = Some(StatefulState::new(
                device, iw, ih, STATE_FORMAT, "Attractor",
            ));
        }
    }

    fn ensure_position_texture(&mut self, device: &wgpu::Device) {
        if self.position_texture.is_some() {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Attractor Positions"),
            size: wgpu::Extent3d {
                width: PARTICLE_COUNT as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.position_texture = Some(tex);
        self.position_view = Some(view);
    }

    fn reinit_trajectories(&mut self, attractor_type: i32) {
        let idx = (attractor_type as usize).min(4);
        let center = CENTERS[idx];
        for i in 0..PARTICLE_COUNT {
            let fi = i as f32;
            let hash = ((fi * 127.1).sin() * 43758.5453).fract();
            let hash2 = ((fi * 269.5).sin() * 43758.5453).fract();
            let hash3 = ((fi * 419.2).sin() * 43758.5453).fract();
            self.trajectories[i] = [
                center[0] + (hash - 0.5) * 0.1,
                center[1] + (hash2 - 0.5) * 0.1,
                center[2] + (hash3 - 0.5) * 0.1,
            ];
        }
    }

    fn advance_trajectories(&mut self, attractor_type: i32, chaos: f32, speed: f32) {
        let idx = (attractor_type as usize).min(4);
        let base_dt = DTS[idx] * speed;

        for i in 0..PARTICLE_COUNT {
            let mut p = self.trajectories[i];

            for _ in 0..RK2_STEPS {
                let dp = ode(attractor_type, p, chaos);
                let mid = [
                    p[0] + dp[0] * base_dt * 0.5,
                    p[1] + dp[1] * base_dt * 0.5,
                    p[2] + dp[2] * base_dt * 0.5,
                ];
                let dp2 = ode(attractor_type, mid, chaos);
                p = [
                    p[0] + dp2[0] * base_dt,
                    p[1] + dp2[1] * base_dt,
                    p[2] + dp2[2] * base_dt,
                ];

                // Clamp to prevent divergence
                let mag2 = p[0] * p[0] + p[1] * p[1] + p[2] * p[2];
                if mag2 > 10000.0 {
                    let scale = 100.0 / mag2.sqrt();
                    p[0] *= scale;
                    p[1] *= scale;
                    p[2] *= scale;
                }
            }

            self.trajectories[i] = p;
        }
    }

    fn project_and_upload(
        &mut self,
        queue: &wgpu::Queue,
        attractor_type: i32,
        time: f32,
        scale_param: f32,
    ) {
        let idx = (attractor_type as usize).min(4);
        let center = CENTERS[idx];
        let att_scale = SCALES[idx];
        let uv_scale = if scale_param > 0.0 { 1.0 / scale_param } else { 1.0 };

        // Orbiting perspective camera
        let cam_angle = time * 0.3;
        let cam_dist = att_scale * 2.5;
        let cam_x = cam_dist * cam_angle.cos();
        let cam_z_offset = cam_dist * cam_angle.sin();
        let cam_y = att_scale * 0.5;

        let cam_pos = [cam_x + center[0], cam_y + center[1], cam_z_offset + center[2]];

        // Look-at direction
        let fwd = [
            center[0] - cam_pos[0],
            center[1] - cam_pos[1],
            center[2] - cam_pos[2],
        ];
        let fwd_len = (fwd[0] * fwd[0] + fwd[1] * fwd[1] + fwd[2] * fwd[2]).sqrt();
        let fwd = [fwd[0] / fwd_len, fwd[1] / fwd_len, fwd[2] / fwd_len];

        // Right = fwd cross up
        let up = [0.0f32, 1.0, 0.0];
        let right = [
            fwd[1] * up[2] - fwd[2] * up[1],
            fwd[2] * up[0] - fwd[0] * up[2],
            fwd[0] * up[1] - fwd[1] * up[0],
        ];
        let right_len = (right[0] * right[0] + right[1] * right[1] + right[2] * right[2]).sqrt();
        let right = if right_len > 0.001 {
            [right[0] / right_len, right[1] / right_len, right[2] / right_len]
        } else {
            [1.0, 0.0, 0.0]
        };

        // Actual up = right cross fwd
        let actual_up = [
            right[1] * fwd[2] - right[2] * fwd[1],
            right[2] * fwd[0] - right[0] * fwd[2],
            right[0] * fwd[1] - right[1] * fwd[0],
        ];

        let fov_scale = 1.0 / (0.5f32).tan(); // ~45 degree FOV

        for i in 0..PARTICLE_COUNT {
            let p = self.trajectories[i];
            let rel = [
                p[0] - cam_pos[0],
                p[1] - cam_pos[1],
                p[2] - cam_pos[2],
            ];

            // Project onto camera axes
            let z = rel[0] * fwd[0] + rel[1] * fwd[1] + rel[2] * fwd[2];
            if z < 0.01 {
                // Behind camera
                self.position_data[i * 4] = -10.0;
                self.position_data[i * 4 + 1] = -10.0;
                self.position_data[i * 4 + 2] = 0.0;
                self.position_data[i * 4 + 3] = 0.0;
                continue;
            }

            let x = rel[0] * right[0] + rel[1] * right[1] + rel[2] * right[2];
            let y = rel[0] * actual_up[0] + rel[1] * actual_up[1] + rel[2] * actual_up[2];

            let proj_x = (x / z) * fov_scale * uv_scale;
            let proj_y = (y / z) * fov_scale * uv_scale;

            // Map to [0,1] UV space
            self.position_data[i * 4] = proj_x * 0.5 + 0.5;
            self.position_data[i * 4 + 1] = 0.5 - proj_y * 0.5; // Flip Y
            self.position_data[i * 4 + 2] = 0.0;
            self.position_data[i * 4 + 3] = 0.0;
        }

        // Upload to position texture
        if let Some(ref tex) = self.position_texture {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&self.position_data),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(PARTICLE_COUNT as u32 * 16), // 4 floats * 4 bytes
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: PARTICLE_COUNT as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

// ── ODE systems ──

fn ode(attractor_type: i32, p: [f32; 3], chaos: f32) -> [f32; 3] {
    match attractor_type {
        0 => lorenz(p, chaos),
        1 => rossler(p, chaos),
        2 => aizawa(p, chaos),
        3 => thomas(p, chaos),
        4 => halvorsen(p, chaos),
        _ => lorenz(p, chaos),
    }
}

fn lorenz(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let sigma = 10.0 + chaos * 5.0;
    let rho = 28.0 + chaos * 10.0;
    let beta = 8.0 / 3.0;
    [
        sigma * (p[1] - p[0]),
        p[0] * (rho - p[2]) - p[1],
        p[0] * p[1] - beta * p[2],
    ]
}

fn rossler(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let a = 0.2 + chaos * 0.15;
    let b = 0.2;
    let c = 5.7 + chaos * 3.0;
    [
        -(p[1] + p[2]),
        p[0] + a * p[1],
        b + p[2] * (p[0] - c),
    ]
}

fn aizawa(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let a = 0.95;
    let b = 0.7;
    let c = 0.6;
    let d = 3.5 + chaos * 1.5;
    let e = 0.25;
    let f = 0.1;
    [
        (p[2] - b) * p[0] - d * p[1],
        d * p[0] + (p[2] - b) * p[1],
        c + a * p[2] - p[2].powi(3) / 3.0
            - (p[0] * p[0] + p[1] * p[1]) * (1.0 + e * p[2])
            + f * p[2] * p[0].powi(3),
    ]
}

fn thomas(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let b = 0.208186 + chaos * 0.1;
    [
        p[1].sin() - b * p[0],
        p[2].sin() - b * p[1],
        p[0].sin() - b * p[2],
    ]
}

fn halvorsen(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let a = 1.89 + chaos * 0.5;
    [
        -a * p[0] - 4.0 * p[1] - 4.0 * p[2] - p[1] * p[1],
        -a * p[1] - 4.0 * p[2] - 4.0 * p[0] - p[2] * p[2],
        -a * p[2] - 4.0 * p[0] - 4.0 * p[1] - p[0] * p[0],
    ]
}

impl Generator for StrangeAttractorGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::StrangeAttractor
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
        self.ensure_position_texture(device);

        let attractor_type = if ctx.param_count > TYPE as u32 {
            ctx.params[TYPE].round() as i32
        } else {
            0
        };
        let trail = if ctx.param_count > TRAIL as u32 { ctx.params[TRAIL] } else { 0.98 };
        let brightness = if ctx.param_count > BRIGHT as u32 { ctx.params[BRIGHT] } else { 2.0 };
        let chaos = if ctx.param_count > CHAOS as u32 { ctx.params[CHAOS] } else { 0.0 };
        let size = if ctx.param_count > SIZE as u32 { ctx.params[SIZE] } else { 1.5 };
        let speed = if ctx.param_count > SPEED as u32 { ctx.params[SPEED] } else { 1.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        // SNAP param is at index 7 — handled at app layer (trigger_count cycling)

        // Reinitialize trajectories on type change
        if attractor_type != self.current_type {
            self.reinit_trajectories(attractor_type);
            self.current_type = attractor_type;
        }

        // CPU: advance trajectories via RK2
        self.advance_trajectories(attractor_type, chaos, speed);

        // CPU: project 3D positions to 2D and upload to position texture
        self.project_and_upload(queue, attractor_type, ctx.time, scale);

        let state = self.state.as_mut().unwrap();
        let texel_x = 1.0 / iw as f32;
        let texel_y = 1.0 / ih as f32;

        let uniforms = AttractorUniforms {
            decay: trail,
            brightness,
            particle_size: size,
            particle_count: PARTICLE_COUNT as f32,
            texel_x,
            texel_y,
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // GPU pass: decay + splat
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Attractor BG"),
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        self.position_view.as_ref().unwrap(),
                    ),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Attractor Splat Pass"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        state.swap();

        // Blit half-res state to full-res output
        self.blit.blit(device, encoder, state.read_view(), target);

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
