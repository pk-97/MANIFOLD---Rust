//! Edge-slicing compute pass for tv-led-mirror.
//!
//! `LedOutputController` hardcodes its edge-extend widths to 0.5/0.5 because
//! Manifold itself feeds it source textures that are already at strip×LED
//! resolution (per-layer LED routing pre-slices the edges in the compositor).
//! Our screen-capture flow doesn't have that pre-slice — we hand the
//! controller a full-screen IOSurface — so we run our own slicer first that:
//!   1. samples the leftmost `left_edge_width` and rightmost `right_edge_width`
//!      vertical bands of the source per the user's CLI flags;
//!   2. blurs over `blur_radius` source texels to suppress single-pixel
//!      flicker on physical LEDs;
//!   3. decodes sRGB→linear so that "perceptual mid-grey on the TV" becomes
//!      "perceptual mid-grey on the LEDs" instead of "75% photon output".
//!
//! Output is a persistent strip×LED Rgba8Unorm texture that we hand to
//! `LedOutputController::process_frame`. The controller's hardcoded 0.5/0.5
//! widths then act as identity at strip-aligned input resolution.

use manifold_gpu::{
    GpuAddressMode, GpuBinding, GpuComputePipeline, GpuDevice, GpuEncoder, GpuFilterMode,
    GpuSampler, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SlicerUniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    blur_radius: f32,
    luminance_floor: f32,
    luminance_knee: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct Slicer {
    pipeline: GpuComputePipeline,
    sampler: GpuSampler,
    output: GpuTexture,
    width: u32,
    height: u32,
}

impl Slicer {
    pub fn new(device: &GpuDevice, strip_count: u32, leds_per_strip: u32) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/sample_and_slice.wgsl"),
            "cs_main",
            "tv-led-mirror.slicer",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            mip_filter: GpuFilterMode::Nearest,
            address_mode_u: GpuAddressMode::ClampToEdge,
            address_mode_v: GpuAddressMode::ClampToEdge,
            address_mode_w: GpuAddressMode::ClampToEdge,
            compare: None,
        });

        let output = device.create_texture(&GpuTextureDesc {
            width: strip_count,
            height: leds_per_strip,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            // SHADER_WRITE: we write to it from our compute pass.
            // SHADER_READ: LedOutputController's edge-extend samples it.
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            label: "tv-led-mirror.slicer_out",
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

    pub fn dispatch(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuTexture,
        left_edge_width: f32,
        right_edge_width: f32,
        blur_radius: f32,
        luminance_floor: f32,
        luminance_knee: f32,
    ) {
        let uniforms = SlicerUniforms {
            left_edge_width,
            right_edge_width,
            blur_radius,
            luminance_floor,
            luminance_knee,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
            [self.width.div_ceil(8), self.height.div_ceil(8), 1],
            "tv-led-mirror.slicer",
        );
    }

    pub fn output(&self) -> &GpuTexture {
        &self.output
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slicer_wgsl_parses() {
        let source = include_str!("shaders/sample_and_slice.wgsl");
        naga::front::wgsl::parse_str(source).expect("WGSL parse failed");
    }
}
