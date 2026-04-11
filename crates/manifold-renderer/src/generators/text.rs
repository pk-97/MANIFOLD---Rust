use std::collections::BTreeMap;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use crate::text_rasterizer::TextRasterizer;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::TEXT,
        create: |device| Box::new(TextGenerator::new(device)),
    }
}

// Parameter indices
const SIZE: usize = 0; // fraction of output height (0.25 = 25%)
const POS_X: usize = 1;
const POS_Y: usize = 2;
const SCALE: usize = 3;

const TEXT_WGSL: &str = include_str!("shaders/text_compute.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextUniforms {
    pos_x: f32,
    pos_y: f32,
    scale: f32,
    aspect_ratio: f32,
    // -- 16-byte boundary --
    tex_width: f32,
    tex_height: f32,
    output_width: f32,
    output_height: f32,
}

pub struct TextGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
    rasterizer: TextRasterizer,
    // Cached text texture (R8Unorm, CPU-uploaded)
    text_texture: Option<manifold_gpu::GpuTexture>,
    text_tex_dims: (u32, u32),
    // Dirty checking
    cached_text: String,
    cached_pixel_size: f32,
    // Pending text from set_string_params (consumed in render)
    pending_text: String,
}

impl TextGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            TEXT_WGSL,
            "cs_main",
            "Text",
        );
        Self {
            pipeline,
            rasterizer: TextRasterizer::new(),
            text_texture: None,
            text_tex_dims: (0, 0),
            cached_text: String::new(),
            cached_pixel_size: 0.0,
            pending_text: "HELLO".to_string(),
        }
    }

    fn ensure_texture(
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
            label: "Text Generator",
            mip_levels: 1,
        });
        self.text_texture = Some(texture);
        self.text_tex_dims = (width, height);
    }
}

impl Generator for TextGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::TEXT
    }

    fn set_string_params(&mut self, params: Option<&BTreeMap<String, String>>) {
        if let Some(map) = params
            && let Some(text) = map.get("text")
        {
            self.pending_text = text.clone();
        }
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let size_frac = if ctx.param_count > SIZE as u32 {
            ctx.params[SIZE].clamp(0.02, 1.0)
        } else {
            0.25
        };
        // Rasterize at output-resolution pixel density for crisp text
        let font_size = size_frac * ctx.output_height as f32;
        let pos_x = if ctx.param_count > POS_X as u32 {
            ctx.params[POS_X]
        } else {
            0.0
        };
        let pos_y = if ctx.param_count > POS_Y as u32 {
            ctx.params[POS_Y]
        } else {
            0.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE].max(0.01)
        } else {
            1.0
        };

        // Dirty check: re-rasterize only when text or pixel size changes
        let text_changed = self.pending_text != self.cached_text;
        let size_changed = (font_size - self.cached_pixel_size).abs() > 0.5;

        if text_changed || size_changed {
            match self.rasterizer.rasterize(&self.pending_text, font_size) {
                Some(result) => {
                    self.ensure_texture(gpu.device, result.width, result.height);
                    if let Some(ref texture) = self.text_texture {
                        gpu.native_enc.upload_texture(
                            texture,
                            result.width,
                            result.height,
                            1,
                            &result.pixels,
                        );
                    }
                }
                None => {
                    // Empty text — drop the texture
                    self.text_texture = None;
                    self.text_tex_dims = (0, 0);
                }
            }
            self.cached_text = self.pending_text.clone();
            self.cached_pixel_size = font_size;
        }

        // If no text texture, clear and return
        let Some(ref text_tex) = self.text_texture else {
            gpu.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
            return ctx.anim_progress;
        };

        let uniforms = TextUniforms {
            pos_x,
            pos_y,
            scale,
            aspect_ratio: ctx.aspect,
            tex_width: self.text_tex_dims.0 as f32,
            tex_height: self.text_tex_dims.1 as f32,
            output_width: ctx.width as f32,
            output_height: ctx.height as f32,
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: text_tex,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "Text",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Text texture dimensions are font-size dependent, not output-size.
    }
}
