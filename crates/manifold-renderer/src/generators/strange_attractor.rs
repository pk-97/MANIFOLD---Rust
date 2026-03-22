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
const WARMUP_STEPS: usize = 50;
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

        // Initialize trajectories — will be re-seeded properly on first render
        let trajectories = vec![[0.0f32; 3]; PARTICLE_COUNT];

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
            current_type: -1, // Force reinit on first render
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

    /// Seed trajectories and run warmup steps.
    /// Unity ref: StrangeAttractorGenerator.cs SeedTrajectories()
    fn reinit_trajectories(&mut self, attractor_type: i32) {
        let idx = (attractor_type as usize).min(4);
        let center = CENTERS[idx];
        let att_scale = SCALES[idx];
        let dt = DTS[idx] * 2.0; // Unity uses dt * 2 for warmup

        for i in 0..PARTICLE_COUNT {
            // Hash seeding matching Unity's Hash31 function
            let seed = hash31(i as f32 * 7.13 + 0.5);
            let mut p = [
                center[0] + seed[0] * att_scale * 0.15,
                center[1] + seed[1] * att_scale * 0.15,
                center[2] + seed[2] * att_scale * 0.15,
            ];

            // Warmup: 50 steps to escape transient (Unity: WARMUP_STEPS = 50)
            for _ in 0..WARMUP_STEPS {
                let dp = ode(attractor_type, p, 0.0);
                let mid = [
                    p[0] + dp[0] * dt * 0.5,
                    p[1] + dp[1] * dt * 0.5,
                    p[2] + dp[2] * dt * 0.5,
                ];
                let dp2 = ode(attractor_type, mid, 0.0);
                p = [
                    p[0] + dp2[0] * dt,
                    p[1] + dp2[1] * dt,
                    p[2] + dp2[2] * dt,
                ];
            }

            self.trajectories[i] = p;
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

                // Clamp to prevent blow-up (Unity: Clamp(p.x, -1000, 1000))
                p[0] = p[0].clamp(-1000.0, 1000.0);
                p[1] = p[1].clamp(-1000.0, 1000.0);
                p[2] = p[2].clamp(-1000.0, 1000.0);
            }

            self.trajectories[i] = p;
        }
    }

    /// Project trajectories to 2D and upload to position texture.
    /// Uses Unity's simple orbiting camera with Y-axis rotation + tilt.
    /// Unity ref: StrangeAttractorGenerator.cs ProjectPoint()
    fn project_and_upload(
        &mut self,
        queue: &wgpu::Queue,
        attractor_type: i32,
        cam_angle: f32,
        scale_param: f32,
        aspect: f32,
    ) {
        let idx = (attractor_type as usize).min(4);
        let center = CENTERS[idx];
        let att_scale = SCALES[idx];
        let uv_scale = if scale_param > 0.0 { 1.0 / scale_param } else { 1.0 };

        // Tilt constant (Unity: const float tilt = 0.3f)
        let tilt = 0.3f32;
        let ct = tilt.cos();
        let st = tilt.sin();

        let ca = cam_angle.cos();
        let sa = cam_angle.sin();

        for i in 0..PARTICLE_COUNT {
            let p = self.trajectories[i];

            // Normalize to attractor scale
            let qx = (p[0] - center[0]) / att_scale;
            let qy = (p[1] - center[1]) / att_scale;
            let qz = (p[2] - center[2]) / att_scale;

            // Rotate around Y axis
            let rx = qx * ca - qz * sa;
            let mut rz = qx * sa + qz * ca;

            // Tilt slightly for better 3D view
            let ry = qy * ct - rz * st;
            rz = qy * st + rz * ct;

            // Perspective projection
            let depth = rz + 2.5;
            let persp_scale = 2.0 / (uv_scale * depth.max(0.3));

            let sx = rx * persp_scale / aspect;
            let sy = ry * persp_scale;

            // Map to [0,1] UV space
            self.position_data[i * 4] = sx * 0.5 + 0.5;
            self.position_data[i * 4 + 1] = sy * 0.5 + 0.5;
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

/// Hash function matching Unity's StrangeAttractorGenerator.Hash31 / Frac
fn hash31(p: f32) -> [f32; 3] {
    let frac = |x: f32| x - x.floor();

    let mut px = frac(p * 0.1031);
    let mut py = frac(p * 0.1030);
    let mut pz = frac(p * 0.0973);

    let d = px * (py + 33.33) + py * (pz + 33.33) + pz * (px + 33.33);
    px += d;
    py += d;
    pz += d;

    [
        frac((px * px + py * pz) * pz) * 2.0 - 1.0,
        frac((px * py + py * py) * px) * 2.0 - 1.0,
        frac((px * pz + pz * pz) * py) * 2.0 - 1.0,
    ]
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

// ODE constants matching Unity StrangeAttractorGenerator.cs EXACTLY
fn lorenz(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let sigma = 10.0 + chaos * 4.0;    // Unity: 10 + c * 4
    let rho = 28.0 + chaos * 8.0;      // Unity: 28 + c * 8
    let beta = 8.0 / 3.0 + chaos * 0.5; // Unity: 8/3 + c * 0.5
    [
        sigma * (p[1] - p[0]),
        p[0] * (rho - p[2]) - p[1],
        p[0] * p[1] - beta * p[2],
    ]
}

fn rossler(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let a = 0.2 + chaos * 0.15;       // Unity: 0.2 + c * 0.15
    let b = 0.2 + chaos * 0.1;        // Unity: 0.2 + c * 0.1
    let c = 5.7 + chaos * 3.0;        // Unity: 5.7 + c * 3
    [
        -(p[1] + p[2]),
        p[0] + a * p[1],
        b + p[2] * (p[0] - c),
    ]
}

fn aizawa(p: [f32; 3], chaos: f32) -> [f32; 3] {
    let a = 0.95 + chaos * 0.1;       // Unity: 0.95 + c * 0.1
    let b = 0.7 + chaos * 0.2;        // Unity: 0.7 + c * 0.2
    let c = 0.6;
    let d = 3.5 + chaos * 1.0;        // Unity: 3.5 + c * 1 (NOT 1.5)
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
    let b = 0.208186 - chaos * 0.05;   // Unity: 0.208186 - c * 0.05 (SUBTRACT, not add)
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
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
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
        // Camera angle matches Unity: time * animSpeed * 0.25
        let cam_angle = ctx.time * speed * 0.25;
        let aspect = ctx.width as f32 / ctx.height.max(1) as f32;
        self.project_and_upload(queue, attractor_type, cam_angle, scale, aspect);

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
            let ts = profiler.and_then(|p| p.render_timestamps("Attractor Splat", iw, ih));
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
                timestamp_writes: ts,
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
