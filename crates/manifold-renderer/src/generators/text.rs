use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use crate::text_rasterizer::{HAlign, RasterizeOptions, TextRasterizer};
use manifold_core::GeneratorTypeId;
use std::collections::BTreeMap;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::TEXT,
        create: |device| Box::new(TextGenerator::new(device)),
    }
}

// Parameter indices (must match metadata order in generator_metadata_submissions.rs)
const SIZE: usize = 0;
const POS_X: usize = 1;
const POS_Y: usize = 2;
const SCALE: usize = 3;
const H_ALIGN: usize = 4;
const V_ALIGN: usize = 5;
const LETTER_SPACING: usize = 6;
const LINE_SPACING: usize = 7;

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
    // -- 16-byte boundary --
    v_align: f32, // 0=Top, 1=Center, 2=Bottom
    _pad: [f32; 3],
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
    cached_font_family: String,
    cached_h_align: f32,
    cached_letter_spacing: f32,
    cached_line_spacing: f32,
    // Pending values from set_string_params (consumed in render)
    pending_text: String,
    pending_font_family: String,
}

impl TextGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(TEXT_WGSL, "cs_main", "Text");
        Self {
            pipeline,
            rasterizer: TextRasterizer::new(),
            text_texture: None,
            text_tex_dims: (0, 0),
            cached_text: String::new(),
            cached_pixel_size: 0.0,
            cached_font_family: String::new(),
            cached_h_align: -1.0,
            cached_letter_spacing: f32::NAN,
            cached_line_spacing: f32::NAN,
            pending_text: "HELLO".to_string(),
            pending_font_family: String::new(),
        }
    }

    fn ensure_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
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
        if let Some(map) = params {
            if let Some(text) = map.get("text") {
                self.pending_text = text.clone();
            }
            if let Some(font) = map.get("fontFamily") {
                if *font != self.pending_font_family {
                    self.rasterizer.prewarm_font(font);
                }
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
        let param = |idx: usize, default: f32| -> f32 {
            if ctx.param_count > idx as u32 {
                ctx.params[idx]
            } else {
                default
            }
        };

        let size_frac = param(SIZE, 0.25).clamp(0.02, 1.0);
        let font_size = size_frac * ctx.output_height as f32;
        let pos_x = param(POS_X, 0.0);
        let pos_y = param(POS_Y, 0.0);
        let scale = param(SCALE, 1.0).max(0.01);
        let h_align = param(H_ALIGN, 1.0);
        let v_align = param(V_ALIGN, 1.0);
        let letter_spacing = param(LETTER_SPACING, 0.0);
        let line_spacing = param(LINE_SPACING, 1.2);

        // Dirty check: re-rasterize when text, font, size, or styling changes
        let text_changed = self.pending_text != self.cached_text;
        let size_changed = (font_size - self.cached_pixel_size).abs() > 0.5;
        let font_changed = self.pending_font_family != self.cached_font_family;
        let style_changed = (h_align - self.cached_h_align).abs() > 0.01
            || (letter_spacing - self.cached_letter_spacing).abs() > 0.001
            || (line_spacing - self.cached_line_spacing).abs() > 0.01;

        if text_changed || size_changed || font_changed || style_changed {
            let opts = RasterizeOptions {
                font_family: if self.pending_font_family.is_empty() {
                    None
                } else {
                    Some(self.pending_font_family.as_str())
                },
                h_align: HAlign::from_param(h_align),
                letter_spacing,
                line_spacing,
            };
            match self
                .rasterizer
                .rasterize(&self.pending_text, font_size, &opts)
            {
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
                    self.text_texture = None;
                    self.text_tex_dims = (0, 0);
                }
            }
            self.cached_text = self.pending_text.clone();
            self.cached_pixel_size = font_size;
            self.cached_font_family = self.pending_font_family.clone();
            self.cached_h_align = h_align;
            self.cached_letter_spacing = letter_spacing;
            self.cached_line_spacing = line_spacing;
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
            v_align,
            _pad: [0.0; 3],
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
