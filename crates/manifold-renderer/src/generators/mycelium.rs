// Physarum agent-based compute pipeline: 500K agents with atomic scatter,
// trail diffusion via 3-pass compute blur, and HSV display output.
//
// 4 passes per frame:
//   AgentUpdate (compute) -> ResolveDeposit (compute) -> DiffuseDecay (3x compute) -> Display (compute)

use super::compute_common::{FIXED_POINT_SCALE, PhysarumAgent};
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::MYCELIUM,
        create: |device| Box::new(MyceliumGenerator::new(device)),
    }
}

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
const AGENTS: usize = 9;
const SCALE: usize = 10;
const SEEDS: usize = 11;

// Trail format: Rgba16Float (Unity: RFloat / R32Float, but R32Float is not filterable on Metal
// and trail textures need both STORAGE_BINDING and filtered sampling)
const TRAIL_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const MAX_AGENTS: u32 = 500_000;
const MIN_AGENTS: u32 = 10_000;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AgentUniforms {
    agent_count: u32,
    width: u32,
    height: u32,
    sensor_dist: f32,
    sensor_angle: f32,
    rotation_angle: f32,
    step_size: f32,
    deposit_scaled: f32,
    frame_count: u32,
    beat: f32,
    reactivity: f32,
    dt: f32,
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
struct DiffuseUniforms {
    decay: f32,
    sub_decay: f32,
    texel_x: f32,
    texel_y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    hue: f32,
    glow: f32,
    uv_scale: f32,
    time: f32,
}

pub struct MyceliumGenerator {
    // Compute pipelines
    agent_update_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_pipeline: manifold_gpu::GpuComputePipeline,
    diffuse_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init on first render)
    agent_buffer: Option<manifold_gpu::GpuBuffer>,
    accum_buffer: Option<manifold_gpu::GpuBuffer>,
    trail_a: Option<manifold_gpu::GpuTexture>,
    trail_b: Option<manifold_gpu::GpuTexture>,

    // State
    agent_count: u32,
    trail_width: u32,
    trail_height: u32,
    frame_count: u64,
    initialized: bool,
    current_seeds: u32,
}

impl MyceliumGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let agent_update_pipeline = device.create_compute_pipeline(
            include_str!("shaders/mycelium_agent_update.wgsl"),
            "main",
            "Mycelium AgentUpdate",
        );
        let resolve_pipeline = device.create_compute_pipeline(
            include_str!("shaders/mycelium_resolve.wgsl"),
            "main",
            "Mycelium Resolve",
        );
        let diffuse_pipeline = device.create_compute_pipeline(
            include_str!("shaders/mycelium_diffuse_compute.wgsl"),
            "cs_main",
            "Mycelium Diffuse",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/mycelium_display_compute.wgsl"),
            "cs_main",
            "Mycelium Display",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_v: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_w: manifold_gpu::GpuAddressMode::Repeat,
            ..Default::default()
        });

        Self {
            agent_update_pipeline,
            resolve_pipeline,
            diffuse_pipeline,
            display_pipeline,
            sampler,
            agent_buffer: None,
            accum_buffer: None,
            trail_a: None,
            trail_b: None,
            agent_count: 0,
            trail_width: 0,
            trail_height: 0,
            frame_count: 0,
            initialized: false,
            current_seeds: 0,
        }
    }

    fn init_resources(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
        agent_count: u32,
        seeds: u32,
    ) {
        let tw = (width / 2).max(1);
        let th = (height / 2).max(1);
        self.trail_width = tw;
        self.trail_height = th;
        self.agent_count = agent_count;
        self.current_seeds = seeds;
        self.frame_count = 0;

        // Agent buffer: agent_count * 16 bytes (shared for CPU seeding)
        let agent_buf_size = (agent_count as u64) * (std::mem::size_of::<PhysarumAgent>() as u64);
        let agent_buffer = device.create_buffer_shared(agent_buf_size);

        // Seed agents with random positions
        self.seed_agents(&agent_buffer, agent_count, seeds);

        // Accumulator buffer: tw * th * 4 bytes (atomic u32) — GPU-only
        let accum_size = (tw as u64) * (th as u64) * 4;
        let accum_buffer = device.create_buffer(accum_size);

        // Trail textures (Rgba16Float, half-res)
        let trail_a = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: tw,
            height: th,
            depth: 1,
            format: TRAIL_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Mycelium Trail A",
            mip_levels: 1,
        });
        let trail_b = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: tw,
            height: th,
            depth: 1,
            format: TRAIL_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Mycelium Trail B",
            mip_levels: 1,
        });

        self.agent_buffer = Some(agent_buffer);
        self.accum_buffer = Some(accum_buffer);
        self.trail_a = Some(trail_a);
        self.trail_b = Some(trail_b);
        self.initialized = true;
    }

    fn seed_agents(&self, buffer: &manifold_gpu::GpuBuffer, count: u32, seeds: u32) {
        let mut agents = Vec::with_capacity(count as usize);
        let seed_base = seeds.wrapping_mul(2654435761);
        for i in 0..count {
            let h = wang_hash_cpu(i.wrapping_add(seed_base));
            let h2 = wang_hash_cpu(h);
            let h3 = wang_hash_cpu(h2);
            let px = (h as f32) / 4294967296.0;
            let py = (h2 as f32) / 4294967296.0;
            let angle = (h3 as f32) / 4294967296.0 * std::f32::consts::TAU;
            agents.push(PhysarumAgent {
                pos: [px, py],
                angle,
                _pad: 0.0,
            });
        }
        unsafe {
            buffer.write(0, bytemuck::cast_slice(&agents));
        }
    }
}

fn wang_hash_cpu(seed_in: u32) -> u32 {
    let mut seed = seed_in;
    seed = (seed ^ 61) ^ (seed >> 16);
    seed = seed.wrapping_mul(9);
    seed = seed ^ (seed >> 4);
    seed = seed.wrapping_mul(0x27d4eb2d);
    seed = seed ^ (seed >> 15);
    seed
}

impl Generator for MyceliumGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::MYCELIUM
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let sens_dist = if ctx.param_count > SENS_DIST as u32 {
            ctx.params[SENS_DIST]
        } else {
            0.02
        };
        let sens_angle = if ctx.param_count > SENS_ANGLE as u32 {
            ctx.params[SENS_ANGLE]
        } else {
            0.8
        };
        let turn = if ctx.param_count > TURN as u32 {
            ctx.params[TURN]
        } else {
            0.4
        };
        let step = if ctx.param_count > STEP as u32 {
            ctx.params[STEP]
        } else {
            0.001
        };
        let deposit = if ctx.param_count > DEPOSIT as u32 {
            ctx.params[DEPOSIT]
        } else {
            1.5
        };
        let decay = if ctx.param_count > DECAY as u32 {
            ctx.params[DECAY]
        } else {
            0.98
        };
        let color_hue = if ctx.param_count > COLOR as u32 {
            ctx.params[COLOR]
        } else {
            0.08
        };
        let glow = if ctx.param_count > GLOW as u32 {
            ctx.params[GLOW]
        } else {
            1.0
        };
        let reactivity = if ctx.param_count > REACTIVITY as u32 {
            ctx.params[REACTIVITY]
        } else {
            0.5
        };
        let agents_param = if ctx.param_count > AGENTS as u32 {
            ctx.params[AGENTS]
        } else {
            200.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };
        let seeds = if ctx.param_count > SEEDS as u32 {
            ctx.params[SEEDS]
        } else {
            1.0
        };

        let desired_agents = ((agents_param * 1000.0) as u32).clamp(MIN_AGENTS, MAX_AGENTS);
        let seeds_int = seeds.to_bits();

        // Lazy-init or re-seed
        if !self.initialized
            || seeds_int != self.current_seeds
            || desired_agents != self.agent_count
        {
            self.init_resources(gpu.device, ctx.width, ctx.height, desired_agents, seeds_int);
        }

        let tw = self.trail_width;
        let th = self.trail_height;
        let agent_count = self.agent_count;

        // UV scale: matches Unity base class
        let uv_scale = if scale > 0.0 { scale } else { 1.0 };

        let agent_buf = self.agent_buffer.as_ref().unwrap();
        let accum_buf = self.accum_buffer.as_ref().unwrap();
        let trail_a = self.trail_a.as_ref().unwrap();
        let trail_b = self.trail_b.as_ref().unwrap();

        // Pass 1: Agent Update
        let deposit_scaled = (deposit * FIXED_POINT_SCALE * 0.01) as u32;
        let agent_uniforms = AgentUniforms {
            agent_count,
            width: tw,
            height: th,
            sensor_dist: sens_dist,
            sensor_angle: sens_angle,
            rotation_angle: turn,
            step_size: step,
            deposit_scaled: deposit_scaled as f32,
            frame_count: self.frame_count as u32,
            beat: ctx.beat as f32,
            reactivity,
            dt: ctx.dt,
        };
        gpu.native_enc.dispatch_compute(
            &self.agent_update_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: agent_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_a,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 3,
                    data: bytemuck::bytes_of(&agent_uniforms),
                },
            ],
            [agent_count.div_ceil(256), 1, 1],
            "Mycelium Agent Update",
        );

        // Pass 2: Resolve — trail_a + accum -> trail_b
        let resolve_uniforms = ResolveUniforms {
            width: tw,
            height: th,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            &self.resolve_pipeline,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: trail_a,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_b,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 3,
                    data: bytemuck::bytes_of(&resolve_uniforms),
                },
            ],
            [tw.div_ceil(16), th.div_ceil(16), 1],
            "Mycelium Resolve",
        );

        // Pass 3: Diffuse (3 blits) — trail_b -> trail_a -> trail_b -> trail_a
        // Pass 0: B->A with decay + evaporation (framerate-independent)
        // pow(decay, dt*60) so per-second decay rate is constant across framerates
        let dt_scale = ctx.dt * 60.0;
        let effective_decay = decay.powf(dt_scale);
        let effective_sub_decay = 0.003 * dt_scale;
        let diffuse0 = DiffuseUniforms {
            decay: effective_decay,
            sub_decay: effective_sub_decay,
            texel_x: 1.0 / tw as f32,
            texel_y: 1.0 / th as f32,
        };
        gpu.native_enc.dispatch_compute(
            &self.diffuse_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&diffuse0),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_b,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: trail_a,
                },
            ],
            [tw.div_ceil(16), th.div_ceil(16), 1],
            "Mycelium Diffuse 0",
        );
        // Pass 1: A->B pure blur
        let diffuse1 = DiffuseUniforms {
            decay: 1.0,
            sub_decay: 0.0,
            texel_x: 1.0 / tw as f32,
            texel_y: 1.0 / th as f32,
        };
        gpu.native_enc.dispatch_compute(
            &self.diffuse_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&diffuse1),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_a,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: trail_b,
                },
            ],
            [tw.div_ceil(16), th.div_ceil(16), 1],
            "Mycelium Diffuse 1",
        );
        // Pass 2: B->A pure blur
        gpu.native_enc.dispatch_compute(
            &self.diffuse_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&diffuse1),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_b,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: trail_a,
                },
            ],
            [tw.div_ceil(16), th.div_ceil(16), 1],
            "Mycelium Diffuse 2",
        );

        // Pass 4: Display — trail_a has final diffused result
        let display_uniforms = DisplayUniforms {
            hue: color_hue,
            glow,
            uv_scale,
            time: ctx.time as f32,
        };
        gpu.native_enc.dispatch_compute(
            &self.display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&display_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: trail_a,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "Mycelium Display",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        self.initialized = false;
    }

    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.initialized = false;
        self.frame_count = 0;
        self.agent_buffer = None;
        self.accum_buffer = None;
        self.trail_a = None;
        self.trail_b = None;
        self.trail_width = 0;
        self.trail_height = 0;
    }
}
