// Particle Text — text that dissolves into a fluid particle simulation.
//
// Internally rasterizes text via TextRasterizer, uploads to an R8 GPU texture,
// then seeds FluidSimCore particles at bright texel positions. The fluid sim
// runs the standard scatter→field→simulate→display pipeline.
//
// Reuses FluidSimCore for all GPU work; only adds text rasterization and
// the text-seeding compute pass.

use std::collections::BTreeMap;

use super::fluid_sim_core::{FluidSimContext, FluidSimCore, FluidSimParams};
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use crate::text_rasterizer::TextRasterizer;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::PARTICLE_TEXT,
        create: |device| Box::new(ParticleTextGenerator::new(device)),
    }
}

// Parameter indices — fluid sim params first, then text-specific.
const SLOPE: usize = 0;
const BLUR: usize = 1;
const ROTATION: usize = 2;
const NOISE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
const SCALE: usize = 6;
const PARTICLES: usize = 7;
const SNAP: usize = 8;
const SNAP_MODE: usize = 9;
const SPLAT_SIZE: usize = 10;
const ANTI_CLUMP: usize = 11;
const INJECT_FORCE: usize = 12;
const TEXT_SIZE: usize = 13;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

// ── Uniform struct for text seed shader ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextSeedUniforms {
    active_count: u32,
    tex_width: u32,
    tex_height: u32,
    frame_seed: u32,
    center_x: f32,
    center_y: f32,
    text_scale: f32,
    aspect_ratio: f32,
}

pub struct ParticleTextGenerator {
    core: FluidSimCore,
    text_seed_pipeline: manifold_gpu::GpuComputePipeline,

    // Text rasterization
    rasterizer: TextRasterizer,
    text_texture: Option<manifold_gpu::GpuTexture>,
    text_tex_dims: (u32, u32),

    // Dirty checking
    cached_text: String,
    cached_pixel_size: f32,
    cached_font_family: String,

    // Pending string params
    pending_text: String,
    pending_font_family: String,

    // Track whether we need to re-seed after text change
    needs_reseed: bool,
    /// Previous trigger_count — detects clip edge when it changes.
    last_trigger_count: i32,
}

impl ParticleTextGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let text_seed_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_text_seed.wgsl"),
            "main",
            "ParticleText Seed",
        );

        Self {
            core: FluidSimCore::new(device),
            text_seed_pipeline,
            rasterizer: TextRasterizer::new(),
            text_texture: None,
            text_tex_dims: (0, 0),
            cached_text: String::new(),
            cached_pixel_size: 0.0,
            cached_font_family: String::new(),
            pending_text: "HELLO".to_string(),
            pending_font_family: String::new(),
            needs_reseed: true,
            last_trigger_count: -1, // first trigger edge always seeds
        }
    }

    fn ensure_text_texture(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.text_tex_dims == (width, height) && self.text_texture.is_some() {
            return;
        }
        let texture = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::R8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "ParticleText Bitmap",
            mip_levels: 1,
        });
        self.text_texture = Some(texture);
        self.text_tex_dims = (width, height);
    }

    /// Dispatch the text-seeding compute pass.
    fn dispatch_text_seed(
        &self,
        gpu: &mut GpuEncoder,
        text_scale: f32,
        aspect: f32,
    ) {
        let text_tex = match self.text_texture.as_ref() {
            Some(t) => t,
            None => return,
        };

        let uniforms = TextSeedUniforms {
            active_count: self.core.active_count,
            tex_width: self.text_tex_dims.0,
            tex_height: self.text_tex_dims.1,
            frame_seed: self.core.frame_count as u32,
            center_x: 0.5,
            center_y: 0.5,
            text_scale,
            aspect_ratio: aspect,
        };

        gpu.native_enc.dispatch_compute(
            &self.text_seed_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: self.core.particle_buffer(),
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: text_tex,
                },
            ],
            [self.core.active_count.div_ceil(256), 1, 1],
            "ParticleText TextSeed",
        );
    }
}

impl Generator for ParticleTextGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::PARTICLE_TEXT
    }

    fn set_string_params(&mut self, params: Option<&BTreeMap<String, String>>) {
        if let Some(map) = params {
            if let Some(text) = map.get("text") {
                self.pending_text = text.clone();
            }
            if let Some(font) = map.get("fontFamily") {
                self.pending_font_family = font.clone();
            }
        }
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let text_size = param(ctx, TEXT_SIZE, 0.25);

        // Rasterize text at output resolution
        let font_size = text_size * ctx.output_height as f32;

        let text_changed = self.pending_text != self.cached_text;
        let size_changed = (font_size - self.cached_pixel_size).abs() > 0.5;
        let font_changed = self.pending_font_family != self.cached_font_family;

        if text_changed || size_changed || font_changed {
            let font_family = if self.pending_font_family.is_empty() {
                None
            } else {
                Some(self.pending_font_family.as_str())
            };
            match self.rasterizer.rasterize(&self.pending_text, font_size, font_family) {
                Some(result) => {
                    self.ensure_text_texture(gpu.device, result.width, result.height);
                    if let Some(ref texture) = self.text_texture {
                        gpu.native_enc.upload_texture(
                            texture,
                            result.width,
                            result.height,
                            1,
                            &result.pixels,
                        );
                    }
                    self.needs_reseed = true;
                }
                None => {
                    self.text_texture = None;
                    self.text_tex_dims = (0, 0);
                }
            }
            self.cached_text = self.pending_text.clone();
            self.cached_pixel_size = font_size;
            self.cached_font_family = self.pending_font_family.clone();
        }

        // Extract fluid sim params
        let params = FluidSimParams {
            slope: param(ctx, SLOPE, -0.01),
            blur_radius: param(ctx, BLUR, 20.0),
            rotation_deg: param(ctx, ROTATION, 85.0),
            noise: param(ctx, NOISE, 0.001),
            speed: param(ctx, SPEED, 1.0),
            contrast: param(ctx, CONTRAST, 3.0),
            scale: param(ctx, SCALE, 1.0),
            particles_millions: param(ctx, PARTICLES, 2.0),
            snap: param(ctx, SNAP, 0.0),
            snap_mode: param(ctx, SNAP_MODE, 0.0),
            splat_size: param(ctx, SPLAT_SIZE, 3.0),
            anti_clump: param(ctx, ANTI_CLUMP, 20.0),
            inject_force: param(ctx, INJECT_FORCE, 0.005),
        };

        // Update active count before seeding
        let active_count = ((params.particles_millions * 1_000_000.0) as u32)
            .clamp(100_000, super::fluid_sim_core::MAX_PARTICLES);
        self.core.active_count = active_count;

        // Ensure particles are initialized
        if !self.core.initialized {
            self.core.init_particles_gpu(gpu);
            self.needs_reseed = true;
        }

        // Detect clip edge: trigger_count changes on note-on / clip start
        let trigger = ctx.trigger_count as i32;
        if trigger != self.last_trigger_count {
            self.last_trigger_count = trigger;
            self.needs_reseed = true;
        }

        // Seed particles from text bitmap on clip edge or text change
        if self.needs_reseed && self.text_texture.is_some() {
            self.dispatch_text_seed(gpu, text_size, ctx.aspect);
            self.needs_reseed = false;
        }

        let sim_ctx = FluidSimContext {
            width: ctx.width,
            height: ctx.height,
            dt: ctx.dt,
            time: ctx.time,
            trigger_count: ctx.trigger_count,
        };

        self.core.render(gpu, target, &params, &sim_ctx);
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        self.core.resize();
    }

    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.core.reset_state();
        self.needs_reseed = true;
        self.last_trigger_count = -1;
        self.cached_text.clear();
        self.cached_pixel_size = 0.0;
    }
}
