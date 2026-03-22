// ComputeStrangeAttractorGenerator — GPU compute port of Unity ComputeStrangeAttractorGenerator.
//
// Pipeline per frame:
//   SeedKernel (on attractor type change) → CSMain (8-step RK2 ODE integration)
//   → SplatKernel (atomic density scatter) → ResolveKernel (uint → float density)
//   → Display pass (extended Reinhard tone mapping)
//
// Particle layout (48 bytes): float3 velocity = 3D attractor state;
//                              float3 position.xy = projected UV (0-1).
// MAX_PARTICLES = 2_000_000 matching Unity.
// Active count: clamp(param * 1_000_000, 100_000, MAX_PARTICLES).

use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::render_target::RenderTarget;

// Parameter indices matching GeneratorDefinitionRegistry order
const TYPE: usize      = 0;
const CONTRAST: usize  = 1;
const CHAOS: usize     = 2;
const SPEED: usize     = 3;
const SCALE: usize     = 4;
const SNAP: usize      = 5;
const PARTICLES: usize = 6;
const DIFFUSION: usize = 7;
const TILT: usize      = 8;
const SPLAT_SIZE: usize = 9;
const INVERT: usize    = 10;

const MAX_PARTICLES: u32     = 2_000_000;
const THREAD_GROUP_SIZE: u32 = 256;
const ATTRACTOR_COUNT: u32   = 5;

/// 12 floats × 4 bytes = 48 bytes per particle.
const PARTICLE_STRIDE: u64 = 48;

/// Reference area for intensity normalization (1080p), matching Unity SCATTER_REFERENCE_AREA.
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0;

// Per-type attractor constants — matches Unity AttractorCenter/Scale/Dt lookup tables
const ATTRACTOR_CENTERS: [[f32; 3]; 5] = [
    [0.0, 0.0, 25.0],  // Lorenz
    [0.0, 0.0, 2.0],   // Rossler
    [0.0, 0.0, 0.5],   // Aizawa
    [0.0, 0.0, 0.0],   // Thomas
    [0.0, 0.0, 0.0],   // Halvorsen
];
const ATTRACTOR_SCALES: [f32; 5] = [25.0, 10.0, 1.2, 4.0, 12.0];
const ATTRACTOR_DTS: [f32; 5]    = [0.003, 0.008, 0.008, 0.03, 0.004];

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SimUniforms {
    time: f32,
    delta_time: f32,
    beat: f32,
    particle_count: u32,
    anim_speed: f32,
    uv_scale: f32,
    attractor_type: u32,
    chaos: f32,
    cam_angle: f32,
    cam_tilt: f32,
    aspect: f32,
    diffusion: f32,
    frame_count: u32,
    attractor_dt: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    attractor_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SplatUniforms {
    active_count: u32,
    width: u32,
    height: u32,
    scaled_energy: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
}

pub struct ComputeStrangeAttractorGenerator {
    // Simulate pipeline — cs_main and seed_kernel from same shader module
    sim_pipeline: wgpu::ComputePipeline,
    seed_pipeline: wgpu::ComputePipeline,
    sim_bgl: wgpu::BindGroupLayout,

    // Scatter pipelines — reuse fluid_scatter.wgsl splat_main + resolve_main
    splat_pipeline: wgpu::ComputePipeline,
    splat_bgl: wgpu::BindGroupLayout,
    resolve_pipeline: wgpu::ComputePipeline,
    resolve_bgl: wgpu::BindGroupLayout,

    // Display pipeline — fragment shader reads density texture
    display_pipeline: wgpu::RenderPipeline,
    display_bgl: wgpu::BindGroupLayout,

    // Uniform buffers
    sim_uniform_buf: wgpu::Buffer,
    splat_uniform_buf: wgpu::Buffer,
    resolve_uniform_buf: wgpu::Buffer,
    display_uniform_buf: wgpu::Buffer,

    sampler: wgpu::Sampler,

    // GPU resources (lazy-init at first render, rebuilt on resolution change)
    particle_buffer: Option<wgpu::Buffer>,
    scatter_accum: Option<wgpu::Buffer>,
    density_rt: Option<RenderTarget>,

    // State
    scatter_width: u32,
    scatter_height: u32,
    frame_count: u32,
    last_attractor_type: i32,
    last_trigger_count: i32,
}

impl ComputeStrangeAttractorGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // ── Simulate + Seed compute pipelines ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("AttractorSimulate Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/strange_attractor_simulate.wgsl").into(),
            ),
        });

        let sim_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("AttractorSimulate BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(1, wgpu::ShaderStages::COMPUTE),
            ],
        });

        let sim_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("AttractorSimulate Layout"),
            bind_group_layouts: &[&sim_bgl],
            immediate_size: 0,
        });

        let sim_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Attractor CSMain"),
            layout: Some(&sim_layout),
            module: &sim_shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let seed_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Attractor SeedKernel"),
            layout: Some(&sim_layout),
            module: &sim_shader,
            entry_point: Some("seed_kernel"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Scatter pipelines — reuse fluid_scatter.wgsl ──
        // splat_main uses @group(0), resolve_main uses @group(1).
        // Each gets a separate pipeline layout with one BGL.
        // At dispatch time both are bound at set_bind_group(0, ...) matching FluidSim pattern.
        let scatter_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("AttractorScatter Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/fluid_scatter.wgsl").into(),
            ),
        });

        let splat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("AttractorSplat BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });

        let splat_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("AttractorSplat Layout"),
            bind_group_layouts: &[&splat_bgl],
            immediate_size: 0,
        });

        let splat_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Attractor SplatKernel"),
            layout: Some(&splat_layout),
            module: &scatter_shader,
            entry_point: Some("splat_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let resolve_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("AttractorResolve BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
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
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });

        let resolve_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("AttractorResolve Layout"),
            bind_group_layouts: &[&resolve_bgl],
            immediate_size: 0,
        });

        let resolve_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Attractor ResolveKernel"),
            layout: Some(&resolve_layout),
            module: &scatter_shader,
            entry_point: Some("resolve_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ── Display pipeline ──
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("AttractorDisplay Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/compute_strange_attractor.wgsl").into(),
            ),
        });

        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("AttractorDisplay BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("AttractorDisplay Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("AttractorDisplay Pipeline"),
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

        // ── Uniform buffers ──
        let sim_uniform_buf     = create_uniform_buf(device, std::mem::size_of::<SimUniforms>(),     "Attractor Sim Uniforms");
        let splat_uniform_buf   = create_uniform_buf(device, std::mem::size_of::<SplatUniforms>(),   "Attractor Splat Uniforms");
        let resolve_uniform_buf = create_uniform_buf(device, std::mem::size_of::<ResolveUniforms>(), "Attractor Resolve Uniforms");
        let display_uniform_buf = create_uniform_buf(device, std::mem::size_of::<DisplayUniforms>(), "Attractor Display Uniforms");

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Attractor Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            sim_pipeline,
            seed_pipeline,
            sim_bgl,
            splat_pipeline,
            splat_bgl,
            resolve_pipeline,
            resolve_bgl,
            display_pipeline,
            display_bgl,
            sim_uniform_buf,
            splat_uniform_buf,
            resolve_uniform_buf,
            display_uniform_buf,
            sampler,
            particle_buffer: None,
            scatter_accum: None,
            density_rt: None,
            scatter_width: 0,
            scatter_height: 0,
            frame_count: 0,
            last_attractor_type: -1,
            last_trigger_count: -1,
        }
    }

    fn active_particle_count(ctx: &GeneratorContext) -> u32 {
        let millions = param(ctx, PARTICLES, 0.5);
        ((millions * 1_000_000.0) as u32).clamp(100_000, MAX_PARTICLES)
    }

    /// Ensure GPU resources are allocated. No-ops if dimensions unchanged.
    fn ensure_resources(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        // Scatter resolution: half of output, matching Unity InternalResolutionScale = 0.5
        let sw = (width / 2).max(16);
        let sh = (height / 2).max(16);

        // Particle buffer — allocated once, resolution-independent
        if self.particle_buffer.is_none() {
            self.particle_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Attractor Particles"),
                size: MAX_PARTICLES as u64 * PARTICLE_STRIDE,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        // Scatter resources — rebuilt on resolution change
        if self.scatter_accum.is_none() || self.scatter_width != sw || self.scatter_height != sh {
            self.scatter_accum = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Attractor ScatterAccum"),
                size: (sw * sh) as u64 * std::mem::size_of::<u32>() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.density_rt = Some(RenderTarget::new(
                device, sw, sh,
                wgpu::TextureFormat::Rgba16Float,
                "Attractor DensityRT",
            ));
            self.scatter_width  = sw;
            self.scatter_height = sh;
        }
    }

    fn write_sim_uniforms(
        &self,
        queue: &wgpu::Queue,
        time: f32,
        delta_time: f32,
        beat: f32,
        particle_count: u32,
        anim_speed: f32,
        uv_scale: f32,
        attractor_type: u32,
        chaos: f32,
        cam_angle: f32,
        cam_tilt: f32,
        aspect: f32,
        diffusion: f32,
        frame_count: u32,
        attractor_dt: f32,
        center: [f32; 3],
        attractor_scale: f32,
    ) {
        let u = SimUniforms {
            time, delta_time, beat, particle_count, anim_speed, uv_scale,
            attractor_type, chaos, cam_angle, cam_tilt, aspect, diffusion,
            frame_count, attractor_dt,
            center_x: center[0], center_y: center[1], center_z: center[2],
            attractor_scale,
        };
        queue.write_buffer(&self.sim_uniform_buf, 0, bytemuck::bytes_of(&u));
    }
}

impl Generator for ComputeStrangeAttractorGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::ComputeStrangeAttractor
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
        self.ensure_resources(device, ctx.width, ctx.height);

        // ── Resolve parameters ──
        let snap     = param(ctx, SNAP, 0.0);
        let trigger  = ctx.trigger_count as i32;

        // SNAP cycling: attractor type advances on each trigger (Unity lines 90-111)
        let attractor_type = if snap > 0.5 {
            (trigger as u32) % ATTRACTOR_COUNT
        } else {
            let raw = param(ctx, TYPE, 0.0).round() as i32;
            (raw.clamp(0, ATTRACTOR_COUNT as i32 - 1)) as u32
        };

        let chaos      = param(ctx, CHAOS, 0.0);
        let anim_speed = param(ctx, SPEED, 1.0);
        let scale      = param(ctx, SCALE, 1.0);
        let diffusion  = param(ctx, DIFFUSION, 0.0);
        let tilt       = param(ctx, TILT, 0.3);
        let splat_size = param(ctx, SPLAT_SIZE, 3.0);
        let contrast   = param(ctx, CONTRAST, 3.5);
        let invert     = param(ctx, INVERT, 0.0);

        let active_count  = Self::active_particle_count(ctx);
        let idx           = (attractor_type as usize).min(4);
        let center        = ATTRACTOR_CENTERS[idx];
        let att_scale     = ATTRACTOR_SCALES[idx];
        let att_dt        = ATTRACTOR_DTS[idx] * anim_speed;
        let uv_scale      = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let aspect        = ctx.width as f32 / ctx.height.max(1) as f32;
        // cam_angle = time * animSpeed * 0.25 (Unity line 123)
        let cam_angle     = ctx.time * anim_speed * 0.25;
        let sw            = self.scatter_width;
        let sh            = self.scatter_height;

        let particle_buffer = self.particle_buffer.as_ref().unwrap();
        let scatter_accum   = self.scatter_accum.as_ref().unwrap();
        let density_rt      = self.density_rt.as_ref().unwrap();

        // ── SeedKernel on attractor type change (Unity lines 133-137) ──
        if attractor_type as i32 != self.last_attractor_type {
            // Seed with cam_angle=0, cam_tilt=0.3, uv_scale=1.0, chaos=0.0 (Unity DispatchSeedKernel)
            self.write_sim_uniforms(
                queue,
                ctx.time, ctx.dt, ctx.beat,
                active_count,
                anim_speed, 1.0,           // uv_scale = 1.0 for seed
                attractor_type,
                0.0, 0.0, 0.3,             // chaos=0, cam_angle=0, cam_tilt=0.3
                aspect, 0.0,               // diffusion=0
                self.frame_count,
                ATTRACTOR_DTS[idx],        // no animSpeed multiplier for seed
                center, att_scale,
            );

            let seed_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Attractor Seed BG"),
                layout: &self.sim_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.sim_uniform_buf.as_entire_binding() },
                ],
            });

            {
                let ts = profiler.and_then(|p| p.compute_timestamps("Attractor SeedKernel", active_count, 1));
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("Attractor SeedKernel"),
                    timestamp_writes: ts,
                });
                pass.set_pipeline(&self.seed_pipeline);
                pass.set_bind_group(0, &seed_bg, &[]);
                let groups = active_count.div_ceil(THREAD_GROUP_SIZE);
                pass.dispatch_workgroups(groups, 1, 1);
            }

            self.last_attractor_type = attractor_type as i32;
        }

        if trigger != self.last_trigger_count {
            self.last_trigger_count = trigger;
        }

        // ── Phase 1: Simulate (CSMain) ──
        self.write_sim_uniforms(
            queue,
            ctx.time, ctx.dt, ctx.beat,
            active_count,
            anim_speed, uv_scale,
            attractor_type,
            chaos, cam_angle, tilt,
            aspect, diffusion,
            self.frame_count,
            att_dt,
            center, att_scale,
        );

        let sim_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Attractor Sim BG"),
            layout: &self.sim_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.sim_uniform_buf.as_entire_binding() },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.compute_timestamps("Attractor CSMain", active_count, 1));
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Attractor CSMain"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.sim_pipeline);
            pass.set_bind_group(0, &sim_bg, &[]);
            let groups = active_count.div_ceil(THREAD_GROUP_SIZE);
            pass.dispatch_workgroups(groups, 1, 1);
        }

        // ── Phase 2: Scatter (SplatKernel) ──
        // Energy normalized by particle count (1M reference) — Unity DispatchScatter lines 473-474
        let energy        = 0.005 * splat_size / 3.0 * (1_000_000.0 / active_count as f32);
        let scaled_energy = (energy * 4096.0 + 0.5) as u32;

        queue.write_buffer(&self.splat_uniform_buf, 0, bytemuck::bytes_of(&SplatUniforms {
            active_count,
            width: sw,
            height: sh,
            scaled_energy,
        }));

        let splat_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Attractor Splat BG"),
            layout: &self.splat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.splat_uniform_buf.as_entire_binding() },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.compute_timestamps("Attractor SplatKernel", sw, sh));
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Attractor SplatKernel"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.splat_pipeline);
            pass.set_bind_group(0, &splat_bg, &[]);
            let groups = active_count.div_ceil(THREAD_GROUP_SIZE);
            pass.dispatch_workgroups(groups, 1, 1);
        }

        // ── Phase 3: Resolve (ResolveKernel) ──
        queue.write_buffer(&self.resolve_uniform_buf, 0, bytemuck::bytes_of(&ResolveUniforms {
            width: sw, height: sh, _pad0: 0, _pad1: 0,
        }));

        let resolve_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Attractor Resolve BG"),
            layout: &self.resolve_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: scatter_accum.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: self.resolve_uniform_buf.as_entire_binding() },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.compute_timestamps("Attractor ResolveKernel", sw, sh));
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("Attractor ResolveKernel"),
                timestamp_writes: ts,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &resolve_bg, &[]);
            let gx = sw.div_ceil(16);
            let gy = sh.div_ceil(16);
            pass.dispatch_workgroups(gx, gy, 1);
        }

        // ── Phase 4: Display pass — extended Reinhard tone mapping ──
        // Normalize intensity by density buffer area (Unity lines 156-157)
        let area_scale = (sw * sh) as f32 / SCATTER_REFERENCE_AREA;
        let intensity  = 3.0 * area_scale;

        queue.write_buffer(&self.display_uniform_buf, 0, bytemuck::bytes_of(&DisplayUniforms {
            intensity,
            contrast,
            invert,
            uv_scale: scale,  // display uses raw scale, not 1/scale
        }));

        let display_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Attractor Display BG"),
            layout: &self.display_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.display_uniform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_rt.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });

        {
            let ts = profiler.and_then(|p| p.render_timestamps("Attractor Display", ctx.width, ctx.height));
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Attractor Display"),
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

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Force scatter resource rebuild on next render
        self.scatter_width  = 0;
        self.scatter_height = 0;
    }
}

// ── BGL helpers — matching fluid_simulation.rs pattern ──

fn bgl_uniform(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_storage_rw(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_storage_ro(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn bgl_texture_filterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn bgl_sampler(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn create_uniform_buf(device: &wgpu::Device, size: usize, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
