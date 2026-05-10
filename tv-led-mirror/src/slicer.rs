//! Edge-slicing compute pass for tv-led-mirror.
//!
//! Per frame:
//!   1. Lazily allocate (or reuse) a mip-able pyramid texture matching the
//!      source IOSurface's size + format.
//!   2. Blit IOSurface mip 0 → pyramid mip 0, then `generate_mipmaps` for the
//!      rest of the chain.
//!   3. Run the slicer compute, sampling the pyramid at a per-output LOD that
//!      matches each LED tile's coverage area (5×5 binomial taps, each one
//!      itself an area integral over a fraction of the tile).
//!   4. Write to a ping-pong output texture, blending with the previous
//!      frame's output for temporal smoothing.
//!
//! `output()` returns the most recently written ping-pong texture so the
//! downstream `LedOutputController` reads what we just produced.

use parking_lot::Mutex;

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
    hdr_peak: f32,
    smoothing_alpha: f32,
    apply_p3_to_srgb: f32,
    /// Bias kernel taps toward bright pixels so wider blur kernels don't
    /// dilute peaks. `weight *= 1 + peak_bias · luminance²`. Mirrors how
    /// `saturation_bias` biases toward saturated taps.
    peak_bias: f32,
    // 21 floats = 84 bytes; pad to 96 (next 16-byte boundary).
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Crop {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub struct ColorGrade {
    pub vibrance: f32,
    pub gamma: f32,
    pub saturation_bias: f32,
    /// Tap weight bias toward BRIGHT taps in the blur kernel. Default 4.0
    /// keeps peak brightness intact when `blur_radius` widens — without it,
    /// each LED tile averages its surroundings and bright regions get
    /// diluted by surrounding darkness. Mirrors `saturation_bias`'s role.
    pub peak_bias: f32,
    pub wb_r: f32,
    pub wb_g: f32,
    pub wb_b: f32,
    pub max_luminance: f32,
    pub black_floor: f32,
    pub hdr_peak: f32,
    /// EMA factor for the temporal blend. 1.0 = no smoothing (pass through),
    /// 0.3 ≈ heavy smoothing (3 frames of inertia). Trades a few frames of
    /// latency for flicker-free LEDs on text/UI.
    pub smoothing_alpha: f32,
    /// True when capture is in extendedLinearDisplayP3 — turns on the
    /// P3→sRGB primaries matrix in the slicer.
    pub apply_p3_to_srgb: bool,
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self {
            vibrance: 1.0,
            gamma: 1.0,
            saturation_bias: 0.0,
            peak_bias: 4.0,
            wb_r: 1.0,
            wb_g: 1.0,
            wb_b: 1.0,
            max_luminance: 1.0,
            black_floor: 0.0,
            hdr_peak: 1.0,
            smoothing_alpha: 1.0,
            apply_p3_to_srgb: false,
        }
    }
}

/// Mip pyramid sized to the captured IOSurface. Lazily (re)allocated when
/// dimensions or pixel format change.
struct MipPyramid {
    texture: GpuTexture,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
}

pub struct Slicer {
    pipeline: GpuComputePipeline,
    sampler: GpuSampler,
    /// Two strip×LED outputs. Each frame we write to one and read the other
    /// as `prev` for the temporal blend; then swap.
    outputs: [GpuTexture; 2],
    write_idx: Mutex<usize>,
    pyramid: Mutex<Option<MipPyramid>>,
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

        // Trilinear sampler — Linear mip filter blends across LOD boundaries
        // smoothly so per-tap LOD choice doesn't visibly bracket.
        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            mip_filter: GpuFilterMode::Linear,
            address_mode_u: GpuAddressMode::ClampToEdge,
            address_mode_v: GpuAddressMode::ClampToEdge,
            address_mode_w: GpuAddressMode::ClampToEdge,
            compare: None,
        });

        let make_output = |label| {
            device.create_texture(&GpuTextureDesc {
                width: strip_count,
                height: leds_per_strip,
                depth: 1,
                format: GpuTextureFormat::Rgba8Unorm,
                dimension: GpuTextureDimension::D2,
                // SHADER_READ for prev_tex sampling, SHADER_WRITE for our
                // textureStore as output_tex, COPY_SRC for the LED controller's
                // edge-extend pass to read it.
                usage: GpuTextureUsage::SHADER_READ
                    | GpuTextureUsage::SHADER_WRITE
                    | GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };

        Self {
            pipeline,
            sampler,
            outputs: [make_output("slicer_out_a"), make_output("slicer_out_b")],
            write_idx: Mutex::new(0),
            pyramid: Mutex::new(None),
            width: strip_count,
            height: leds_per_strip,
        }
    }

    /// Ensure the pyramid matches the source's dimensions + format,
    /// (re)allocating if needed.
    fn ensure_pyramid(&self, device: &GpuDevice, src: &GpuTexture) {
        let mut slot = self.pyramid.lock();
        if let Some(p) = slot.as_ref()
            && p.width == src.width
            && p.height == src.height
            && p.format == src.format
        {
            return;
        }
        let mip_levels = GpuTextureDesc::max_mip_levels(src.width, src.height);
        let pyramid_tex = device.create_texture(&GpuTextureDesc {
            width: src.width,
            height: src.height,
            depth: 1,
            format: src.format,
            dimension: GpuTextureDimension::D2,
            // SHADER_READ for sampling, COPY_DST for the per-frame blit from
            // the IOSurface, RENDER_TARGET so generate_mipmaps's blit-encoder
            // path is happy on every Metal pixel format we use.
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_DST
                | GpuTextureUsage::COPY_SRC
                | GpuTextureUsage::RENDER_TARGET,
            label: "tv-led-mirror.slicer.pyramid",
            mip_levels,
        });
        *slot = Some(MipPyramid {
            texture: pyramid_tex,
            width: src.width,
            height: src.height,
            format: src.format,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &self,
        device: &GpuDevice,
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
        // 1. Refresh pyramid (lazy).
        self.ensure_pyramid(device, source);
        let pyramid_guard = self.pyramid.lock();
        let pyramid = &pyramid_guard.as_ref().expect("ensure_pyramid").texture;

        // 2. Blit IOSurface → pyramid mip 0 → generate the rest of the chain.
        enc.copy_texture_to_texture(source, pyramid, source.width, source.height, 1);
        enc.generate_mipmaps(pyramid);

        // 3. Pick output / prev via ping-pong.
        let mut idx_guard = self.write_idx.lock();
        let write_idx = *idx_guard;
        let prev_idx = 1 - write_idx;
        *idx_guard = prev_idx;
        drop(idx_guard);
        let output = &self.outputs[write_idx];
        let prev = &self.outputs[prev_idx];

        let uniforms = SlicerUniforms {
            blur_radius: blur_radius.max(0.0),
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
            hdr_peak: grade.hdr_peak.max(1.0),
            smoothing_alpha: grade.smoothing_alpha.clamp(0.01, 1.0),
            apply_p3_to_srgb: if grade.apply_p3_to_srgb { 1.0 } else { 0.0 },
            peak_bias: grade.peak_bias.max(0.0),
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
                    texture: pyramid,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: output,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: prev,
                },
            ],
            [self.width.div_ceil(8), self.height.div_ceil(8), 1],
            "tv-led-mirror.slicer",
        );
    }

    /// Output dimensions (strip count × LEDs per strip). Used by the Hue
    /// thread to size its CPU readback buffer.
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Most recently written ping-pong output. Hand this to the LED controller.
    pub fn output(&self) -> &GpuTexture {
        // After dispatch swaps, the texture we wrote to is at the OPPOSITE
        // index from `write_idx` (because we updated write_idx to prev_idx
        // before returning). So output is at 1 - *write_idx.
        let idx = 1 - *self.write_idx.lock();
        &self.outputs[idx]
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
