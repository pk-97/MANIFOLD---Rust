//! Native Metal compute pipeline for the LED edge-extend shader.
//! Self-contained — does not depend on manifold-renderer.
//! Creates a tiny Rgba8Unorm output texture (strip_count × leds_per_strip)
//! and a compute pipeline with the edge-extend shader.

use manifold_gpu::{
    GpuAddressMode, GpuBinding, GpuComputePipeline, GpuDevice, GpuFilterMode, GpuSampler,
    GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

/// GPU resources for the LED edge-extend compute pass.
pub struct EdgeExtendBlit {
    pipeline: GpuComputePipeline,
    sampler: GpuSampler,
    pub(crate) output: GpuTexture,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeExtendUniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    blur_radius: f32,
    led_gain: f32,
}

impl EdgeExtendBlit {
    /// Create the edge-extend compute pipeline and tiny output texture.
    /// `strip_count` = width, `leds_per_strip` = height.
    pub fn new(device: &GpuDevice, strip_count: u32, leds_per_strip: u32) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/led_edge_extend_compute.wgsl"),
            "cs_main",
            "LEDEdgeExtend Compute",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            mip_filter: GpuFilterMode::Nearest,
            address_mode_u: GpuAddressMode::ClampToEdge,
            address_mode_v: GpuAddressMode::ClampToEdge,
            address_mode_w: GpuAddressMode::ClampToEdge,
            compare: None,
            ..Default::default()
        });

        let output = device.create_texture(&GpuTextureDesc {
            width: strip_count,
            height: leds_per_strip,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "LED_SampleRT",
            mip_levels: 1,
        });

        Self {
            pipeline,
            sampler,
            output,
            width: strip_count,
            height: leds_per_strip,
        }
    }

    /// Dispatch the edge-extend compute shader: source → tiny output texture.
    pub fn blit(
        &self,
        enc: &mut manifold_gpu::GpuEncoder,
        source: &GpuTexture,
        left_edge_width: f32,
        right_edge_width: f32,
        blur_radius: f32,
        led_gain: f32,
    ) {
        let uniforms = EdgeExtendUniforms {
            left_edge_width,
            right_edge_width,
            blur_radius,
            led_gain,
        };

        enc.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &self.output,
                },
            ],
            [self.width.div_ceil(16), self.height.div_ceil(16), 1],
            "LEDEdgeExtend",
        );
    }

    /// The tiny output texture (Rgba8Unorm, strip_count × leds_per_strip).
    pub fn output_texture(&self) -> &GpuTexture {
        &self.output
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn led_edge_extend_compute_wgsl_parses() {
        let source = include_str!("shaders/led_edge_extend_compute.wgsl");
        naga::front::wgsl::parse_str(source).expect("WGSL parse failed");
    }
}
