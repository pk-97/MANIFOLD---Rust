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
    blur_radius: f32,
    luminance_floor: f32,
    luminance_knee: f32,
    saturation_floor: f32,
    saturation_knee: f32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    vibrance: f32,
    gamma: f32,
    saturation_bias: f32,
    wb_r: f32,
    wb_g: f32,
    wb_b: f32,
    max_luminance: f32,
    black_floor: f32,
    // 17 floats = 68 bytes. WGSL uniform structs need 16-byte stride →
    // pad to 80 bytes (20 floats).
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[derive(Clone, Copy)]
pub struct ColorGrade {
    pub vibrance: f32,
    pub gamma: f32,
    pub saturation_bias: f32,
    pub wb_r: f32,
    pub wb_g: f32,
    pub wb_b: f32,
    pub max_luminance: f32,
    pub black_floor: f32,
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self {
            vibrance: 1.0,
            gamma: 1.0,
            saturation_bias: 0.0,
            wb_r: 1.0,
            wb_g: 1.0,
            wb_b: 1.0,
            max_luminance: 1.0,
            black_floor: 0.0,
        }
    }
}

/// Margins to crop off the source before slicing, as fractions of the source.
/// Defaults to all zero (no crop).
#[derive(Clone, Copy, Default)]
pub struct Crop {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
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

    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuTexture,
        blur_radius: f32,
        luminance_floor: f32,
        luminance_knee: f32,
        saturation_floor: f32,
        saturation_knee: f32,
        crop: Crop,
        grade: ColorGrade,
    ) {
        let uniforms = SlicerUniforms {
            blur_radius,
            luminance_floor,
            luminance_knee,
            saturation_floor,
            saturation_knee,
            crop_left: crop.left.clamp(0.0, 0.49),
            crop_right: crop.right.clamp(0.0, 0.49),
            crop_top: crop.top.clamp(0.0, 0.49),
            crop_bottom: crop.bottom.clamp(0.0, 0.49),
            vibrance: grade.vibrance.max(0.0),
            gamma: grade.gamma.max(0.0001),
            saturation_bias: grade.saturation_bias.max(0.0),
            wb_r: grade.wb_r.max(0.0),
            wb_g: grade.wb_g.max(0.0),
            wb_b: grade.wb_b.max(0.0),
            max_luminance: grade.max_luminance.clamp(0.0, 1.0),
            black_floor: grade.black_floor.clamp(0.0, 1.0),
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
