//! MetalFX Temporal Denoised Scaler — ML-based denoising with optional
//! upscaling (RAYTRACING_DESIGN.md section 17 DN-F).
//!
//! Wraps `MTLFXTemporalDenoisedScaler` (macOS 26+ Tahoe / Metal 4). On
//! pre-Tahoe systems the factory returns `None` and the caller falls back
//! to the existing temporal accumulator (DN1).
//!
//! The trait is Vulkan-parity shaped (DN5): the input set matches what
//! NRD/ReLAX consumes — color, depth, motion, normal, roughness, diffuse +
//! specular albedo, specular hit-distance, exposure, reactive mask, reset,
//! depth-reversed — with no MetalFX-specific knobs. jitter offsets,
//! motion-vector scale, and pre-exposure live on the Metal implementation
//! struct and are set at construction / via setters; the trait sees only
//! the cross-backend contract.
//!
//! Uses `objc2-metal-fx` typed bindings. The descriptor class is
//! weak-linked — `AnyClass::get` gates availability, same pattern as
//! `supports_spatial_scaling` in `metalfx.rs`.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLPixelFormat, MTLTexture};
use objc2_metal_fx::{
    MTLFXTemporalDenoisedScaler, MTLFXTemporalDenoisedScalerBase,
    MTLFXTemporalDenoisedScalerDescriptor,
};

use super::GpuTexture;
use crate::GpuTextureFormat;

// ─── Format mapping ───────────────────────────────────────────────────

fn to_mtl_pixel_format(fmt: GpuTextureFormat) -> MTLPixelFormat {
    match fmt {
        GpuTextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        GpuTextureFormat::Bgra8UnormSrgb => MTLPixelFormat::BGRA8Unorm_sRGB,
        GpuTextureFormat::R32Float => MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => MTLPixelFormat::R32Uint,
        GpuTextureFormat::Rgba8UnormSrgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        GpuTextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
        GpuTextureFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}

// ─── Availability ─────────────────────────────────────────────────────

/// Check if the MetalFX Temporal Denoised Scaler is available on this
/// system. Availability is determined by probing
/// `MTLFXTemporalDenoisedScalerDescriptor` — objc2-metal-fx weak-links
/// MetalFX, so on pre-Tahoe systems the class lookup returns `None`.
pub fn denoiser_available() -> bool {
    use objc2::runtime::AnyClass;
    AnyClass::get(c"MTLFXTemporalDenoisedScalerDescriptor").is_some()
}

/// Check if the denoised scaler is supported for the given device.
pub fn supports_denoiser(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    if !denoiser_available() {
        return false;
    }
    unsafe { MTLFXTemporalDenoisedScalerDescriptor::supportsDevice(device) }
}

// ─── Denoiser trait ───────────────────────────────────────────────────

/// Backend-neutral denoiser interface, Vulkan-parity shaped (DN5).
///
/// The input set matches what NRD/ReLAX consumes: color, depth, motion,
/// normal, roughness, diffuse + specular albedo, specular hit-distance.
/// Optional: exposure texture, reactive mask.
///
/// No MetalFX-specific knobs — jitter, motion-vector scale, pre-exposure
/// live on the concrete implementation, not here.
pub trait GpuDenoiser: Send + Sync {
    /// Encode the denoise (and optional upscale) into a command buffer.
    /// Caller must end any active encoder on the command buffer before
    /// calling this.
    #[allow(clippy::too_many_arguments)]
    fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        color: &GpuTexture,
        depth: &GpuTexture,
        depth_reversed: bool,
        motion: &GpuTexture,
        normal: &GpuTexture,
        roughness: &GpuTexture,
        diffuse_albedo: &GpuTexture,
        specular_albedo: &GpuTexture,
        specular_hit_distance: &GpuTexture,
        exposure: Option<&GpuTexture>,
        output: &GpuTexture,
        reset: bool,
        reactive_mask: Option<&GpuTexture>,
    );

    fn input_width(&self) -> u32;
    fn input_height(&self) -> u32;
    fn output_width(&self) -> u32;
    fn output_height(&self) -> u32;
}

// ─── Metal implementation ─────────────────────────────────────────────

/// MetalFX Temporal Denoised Scaler — wraps `MTLFXTemporalDenoisedScaler`.
///
/// Created once per (input_size, output_size, format) combination and
/// reused across frames. The scaler manages its own temporal history
/// internally; the caller controls it via the `reset` flag on
/// [`encode`](Self::encode).
///
/// Jitter offsets and motion-vector scale are MetalFX-specific controls
/// that live on this struct (not the trait). They are set at construction
/// from the descriptor dimensions and can be updated per-frame via
/// setters before [`encode`](Self::encode).
pub struct MetalFxDenoisedScaler {
    scaler: Retained<ProtocolObject<dyn MTLFXTemporalDenoisedScaler>>,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

// Safety: MetalFX scalers are thread-safe for encoding (Apple docs).
unsafe impl Send for MetalFxDenoisedScaler {}
unsafe impl Sync for MetalFxDenoisedScaler {}

impl MetalFxDenoisedScaler {
    /// Create a denoised scaler for the given input/output dimensions and
    /// formats.
    ///
    /// `color_format` is used for the color input and output textures.
    /// All auxiliary inputs use the formats described in DN4 /
    /// RAYTRACING_DESIGN.md section 17.5:
    ///   - depth: R32Float
    ///   - motion: RG16Float
    ///   - normal: RGBA16Float
    ///   - roughness: R16Float
    ///   - diffuse/specular albedo: RGBA16Float
    ///   - specular hit-distance: R16Float
    ///   - exposure: R16Float (1x1)
    ///   - reactive mask: R16Float
    ///
    /// Returns `None` if the MetalFX denoised scaler is not available on
    /// this system (pre-Tahoe) or if construction fails.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        color_format: GpuTextureFormat,
        depth_reversed: bool,
    ) -> Option<Self> {
        if !denoiser_available() {
            log::info!(
                "[MetalFX Denoiser] Not available on this system (pre-Tahoe)"
            );
            return None;
        }

        let color_pixel_format = to_mtl_pixel_format(color_format);

        let desc = unsafe {
            let desc =
                MTLFXTemporalDenoisedScalerDescriptor::init(
                    MTLFXTemporalDenoisedScalerDescriptor::alloc(),
                );
            desc.setInputWidth(input_width as usize);
            desc.setInputHeight(input_height as usize);
            desc.setOutputWidth(output_width as usize);
            desc.setOutputHeight(output_height as usize);
            desc.setColorTextureFormat(color_pixel_format);
            desc.setOutputTextureFormat(color_pixel_format);

            // Auxiliary input formats — match DN4's G-buffer outputs.
            desc.setDepthTextureFormat(MTLPixelFormat::R32Float);
            desc.setMotionTextureFormat(MTLPixelFormat::RG16Float);
            desc.setNormalTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setRoughnessTextureFormat(MTLPixelFormat::R16Float);
            desc.setDiffuseAlbedoTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setSpecularAlbedoTextureFormat(MTLPixelFormat::RGBA16Float);
            desc.setSpecularHitDistanceTextureFormat(MTLPixelFormat::R16Float);

            // Reactive mask enabled — single-channel, driven by the
            // shared TemporalResetDetector (DN3).
            desc.setReactiveMaskTextureEnabled(true);
            desc.setReactiveMaskTextureFormat(MTLPixelFormat::R16Float);

            // Auto-exposure off — the caller provides an explicit
            // exposure texture when exposure is wanted.
            desc.setAutoExposureEnabled(false);

            desc
        };

        let scaler = unsafe { desc.newTemporalDenoisedScalerWithDevice(device) };

        let Some(scaler) = scaler else {
            log::error!(
                "[MetalFX Denoiser] Failed to create denoised scaler ({}x{} -> {}x{})",
                input_width,
                input_height,
                output_width,
                output_height
            );
            return None;
        };

        // Motion vectors arrive in NDC-space `(dx, dy)` per pixel
        // (GBUFFER_DESIGN.md section 2 D5). MetalFX expects motion in
        // pixel units: multiplying by half the render-resolution
        // converts an NDC delta spanning [-1, 1] across the full input
        // width/height into pixels.
        unsafe {
            scaler.setMotionVectorScaleX(input_width as f32 * 0.5);
            scaler.setMotionVectorScaleY(input_height as f32 * 0.5);
            scaler.setDepthReversed(depth_reversed);
        }

        log::info!(
            "[MetalFX Denoiser] Created denoised scaler: {}x{} -> {}x{}",
            input_width,
            input_height,
            output_width,
            output_height
        );

        Some(Self {
            scaler,
            input_width,
            input_height,
            output_width,
            output_height,
        })
    }

    /// Set the per-frame jitter offset (in pixels). Call before
    /// [`encode`](Self::encode) each frame; set to (0, 0) when not
    /// jittering.
    pub fn set_jitter(&self, x: f32, y: f32) {
        unsafe {
            self.scaler.setJitterOffsetX(x);
            self.scaler.setJitterOffsetY(y);
        }
    }

    /// Set the motion-vector scale factors. Defaults to
    /// `(input_width * 0.5, input_height * 0.5)` (NDC→pixels) set at
    /// construction. Override if your motion vectors use a different
    /// convention.
    pub fn set_motion_vector_scale(&self, x: f32, y: f32) {
        unsafe {
            self.scaler.setMotionVectorScaleX(x);
            self.scaler.setMotionVectorScaleY(y);
        }
    }

    /// Set a pre-exposure value. The denoiser divides the input color by
    /// this value before processing. Default 1.0 (no adjustment).
    pub fn set_pre_exposure(&self, value: f32) {
        unsafe {
            self.scaler.setPreExposure(value);
        }
    }

    /// Check if this scaler matches the given dimensions (for caching).
    pub fn matches(&self, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> bool {
        self.input_width == in_w
            && self.input_height == in_h
            && self.output_width == out_w
            && self.output_height == out_h
    }
}

impl GpuDenoiser for MetalFxDenoisedScaler {
    fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        color: &GpuTexture,
        depth: &GpuTexture,
        depth_reversed: bool,
        motion: &GpuTexture,
        normal: &GpuTexture,
        roughness: &GpuTexture,
        diffuse_albedo: &GpuTexture,
        specular_albedo: &GpuTexture,
        specular_hit_distance: &GpuTexture,
        exposure: Option<&GpuTexture>,
        output: &GpuTexture,
        reset: bool,
        reactive_mask: Option<&GpuTexture>,
    ) {
        unsafe {
            self.scaler.setColorTexture(Some(&color.raw));
            self.scaler.setDepthTexture(Some(&depth.raw));
            self.scaler.setDepthReversed(depth_reversed);
            self.scaler.setMotionTexture(Some(&motion.raw));
            self.scaler.setNormalTexture(Some(&normal.raw));
            self.scaler.setRoughnessTexture(Some(&roughness.raw));
            self.scaler.setDiffuseAlbedoTexture(Some(&diffuse_albedo.raw));
            self.scaler.setSpecularAlbedoTexture(Some(&specular_albedo.raw));
            self.scaler
                .setSpecularHitDistanceTexture(Some(&specular_hit_distance.raw));
            self.scaler
                .setExposureTexture(exposure.map(|t| &*t.raw as &ProtocolObject<dyn MTLTexture>));
            self.scaler.setOutputTexture(Some(&output.raw));
            self.scaler.setShouldResetHistory(reset);
            self.scaler.setReactiveMaskTexture(
                reactive_mask.map(|t| &*t.raw as &ProtocolObject<dyn MTLTexture>),
            );
            self.scaler.encodeToCommandBuffer(cmd_buf);
        }
    }

    fn input_width(&self) -> u32 {
        self.input_width
    }

    fn input_height(&self) -> u32 {
        self.input_height
    }

    fn output_width(&self) -> u32 {
        self.output_width
    }

    fn output_height(&self) -> u32 {
        self.output_height
    }
}

// ─── Value proof ──────────────────────────────────────────────────────

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
    use crate::metal::GpuDevice;
    use crate::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    /// Upload f32 data into a texture, encoding into the target format.
    /// For f32-native formats bytes go straight through; for half-float
    /// formats each f32 is truncated to f16. Uses CPU_UPLOAD (shared
    /// storage) for synchronous replaceRegion.
    fn upload_texture(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        data: &[f32],
        label: &str,
    ) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label,
            mip_levels: 1,
        });
        let bpp = format.bytes_per_pixel();
        let row_bytes = width as u64 * bpp as u64;
        let total = (row_bytes * height as u64) as usize;
        let mut bytes: Vec<u8> = vec![0; total];

        // Number of f32 elements per pixel for this format.
        let ch = match format {
            GpuTextureFormat::Rgba32Float | GpuTextureFormat::Rgba16Float
            | GpuTextureFormat::Rgba8Unorm | GpuTextureFormat::Rgba8UnormSrgb => 4,
            GpuTextureFormat::Rg32Float | GpuTextureFormat::Rg16Float => 2,
            _ => 1,
        };

        let npixels = (width * height) as usize;
        for pixel in 0..npixels {
            let src = pixel * ch;
            let dst = pixel * bpp as usize;
            match format {
                // f32-native: direct copy
                GpuTextureFormat::Rgba32Float
                | GpuTextureFormat::R32Float
                | GpuTextureFormat::Rg32Float => {
                    for c in 0..(bpp as usize / 4) {
                        let v: f32 = data.get(src + c).copied().unwrap_or(0.0);
                        bytes[dst + c * 4..dst + c * 4 + 4]
                            .copy_from_slice(&f32::to_ne_bytes(v));
                    }
                }
                // half-float: f32 -> f16 truncation
                GpuTextureFormat::Rgba16Float
                | GpuTextureFormat::R16Float
                | GpuTextureFormat::Rg16Float => {
                    for c in 0..(bpp as usize / 2) {
                        let v: f32 = data.get(src + c).copied().unwrap_or(0.0);
                        bytes[dst + c * 2..dst + c * 2 + 2]
                            .copy_from_slice(&f32_to_f16(v));
                    }
                }
                _ => {
                    // Raw byte copy fallback
                    let src_bytes: &[u8] = unsafe {
                        std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
                    };
                    let copy = src_bytes.len().min(bytes.len() - dst);
                    bytes[dst..dst + copy].copy_from_slice(&src_bytes[..copy]);
                }
            }
        }

        let mut enc = device.create_encoder("upload");
        enc.upload_texture(&tex, width, height, 1, &bytes);
        enc.commit_and_wait_completed();
        tex
    }

    /// Truncate f32 to IEEE 754 half-precision. Returns `[le_low, le_high]`.
    fn f32_to_f16(f: f32) -> [u8; 2] {
        let bits = f32::to_bits(f);
        let sign = (bits >> 16) & 0x8000u32;
        let exp = ((bits >> 23) & 0xFF) as i32 - 127;
        let frac = (bits & 0x7FFFFF) >> 13;
        let h: u16 = if exp >= 16 {
            (sign | 0x7C00) as u16
        } else if exp >= -14 {
            (sign | (((exp + 15) as u32) << 10) | frac) as u16
        } else if exp >= -24 {
            let sub = (frac | 0x800000) >> (-(exp + 14) as u32);
            (sign | sub) as u16
        } else {
            sign as u16
        };
        h.to_le_bytes()
    }

    /// Read back a texture into a f32 slice via a shared buffer blit.
    fn readback_texture_via_buffer(
        device: &GpuDevice,
        tex: &GpuTexture,
        buf: &mut [f32],
    ) {
        let bpp = tex.format.bytes_per_pixel();
        let row_bytes = bpp * tex.width;
        let total = (row_bytes * tex.height) as u64;
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("readback");
        enc.copy_texture_to_buffer(tex, &readback, tex.width, tex.height, row_bytes);
        enc.commit_and_wait_completed();
        let ptr = readback
            .mapped_ptr()
            .expect("shared buffer mapped pointer");
        let f32_len = (total / 4) as usize;
        assert!(buf.len() >= f32_len);
        unsafe {
            std::ptr::copy_nonoverlapping(ptr as *const f32, buf.as_mut_ptr(), f32_len);
        }
    }

    /// Value proof: synthesize a noisy color buffer (CPU-generated noise
    /// over a known ramp), denoise one frame with reset=true, read back --
    /// mean abs error vs the clean ramp must be below the noisy input's
    /// error. This proves the effect runs and reduces noise, not that it's
    /// beautiful.
    ///
    /// Uses Rgba32Float for direct f32 readback (no fp16 precision issues).
    #[test]
    fn denoise_reduces_error_vs_clean_ramp() {
        let device = GpuDevice::new();

        if !denoiser_available() {
            eprintln!("[denoiser test] MTLFXTemporalDenoisedScaler not available -- skipping");
            return;
        }

        // 1:1 denoise -- no upscale.
        const W: u32 = 64;
        const H: u32 = 64;

        let scaler = MetalFxDenoisedScaler::new(
            device.raw_device(),
            W,
            H,
            W,
            H,
            GpuTextureFormat::Rgba32Float,
            false, // depth not reversed
        );

        let Some(scaler) = scaler else {
            panic!(
                "MetalFxDenoisedScaler::new returned None on a device that \
                 reported denoiser_available() -- 1:1 scale may be rejected \
                 or construction failed for another reason"
            );
        };


        // Clean ramp: R channel = (x+y)/(W+H-2), G=B=0, A=1.
        let npixels = (W * H) as usize;
        let ncomponents = npixels * 4;
        let mut clean = vec![0.0f32; ncomponents];
        let mut noisy = vec![0.0f32; ncomponents];
        let scale = 1.0 / (W + H - 2) as f32;
        let noise_amplitude: f32 = 0.15;
        // Deterministic PRNG
        let mut rng = {
            let mut state: u64 = (W as u64) << 32 | (H as u64);
            move || -> f32 {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z = z ^ (z >> 31);
                (z as f32 / u64::MAX as f32) * 2.0 - 1.0
            }
        };

        for y in 0..H {
            for x in 0..W {
                let idx = ((y * W + x) * 4) as usize;
                let v = (x + y) as f32 * scale;
                clean[idx] = v;
                clean[idx + 3] = 1.0;
                let noise = rng() * noise_amplitude;
                noisy[idx] = (v + noise).clamp(0.0, 1.0);
                noisy[idx + 3] = 1.0;
            }
        }

        // G-buffer: self-consistent with the ramp.
        // Diffuse albedo = clean ramp, normals = +Z, roughness = 0.5.
        let mut albedo_data = vec![0.0f32; ncomponents];
        let mut normal_data = vec![0.0f32; ncomponents];
        for i in 0..npixels {
            let a = i * 4;
            albedo_data[a] = clean[a];
            albedo_data[a + 3] = 1.0;
            normal_data[a + 2] = 1.0; // +Z
        }

        // Upload inputs. Color uses Rgba32Float for clean f32 readback;
        // auxiliary formats must match the denoiser descriptor exactly.
        let color_noisy = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba32Float, &noisy, "color_noisy",
        );
        let depth_tex = upload_texture(
            &device, W, H, GpuTextureFormat::R32Float,
            &vec![0.5f32; npixels], "depth",
        );
        let motion_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rg16Float,
            &vec![0.0f32; npixels * 2], "motion",
        );
        let normal_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba16Float, &normal_data, "normal",
        );
        let roughness_tex = upload_texture(
            &device, W, H, GpuTextureFormat::R16Float,
            &vec![0.5f32; npixels], "roughness",
        );
        let diffuse_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba16Float, &albedo_data, "diffuse_albedo",
        );
        let specular_tex = upload_texture(
            &device, W, H, GpuTextureFormat::Rgba16Float,
            &vec![0.0f32; ncomponents], "specular_albedo",
        );
        let hit_distance_tex = upload_texture(
            &device, W, H, GpuTextureFormat::R16Float,
            &vec![0.0f32; npixels], "specular_hit_distance",
        );

        // Output texture.
        let output_tex = device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "denoiser_output",
            mip_levels: 1,
        });

        let mut enc = device.create_encoder("denoise-proof");
        scaler.encode(
            enc.raw_cmd_buf(),
            &color_noisy,
            &depth_tex,
            false,
            &motion_tex,
            &normal_tex,
            &roughness_tex,
            &diffuse_tex,
            &specular_tex,
            &hit_distance_tex,
            None,
            &output_tex,
            true,  // reset
            None,  // no reactive mask
        );
        enc.commit_and_wait_completed();

        let mut result = vec![0.0f32; ncomponents];
        readback_texture_via_buffer(&device, &output_tex, &mut result);

        // Compute mean abs error in R channel only (the ramp channel).
        let mut noisy_error_sum: f64 = 0.0;
        let mut result_error_sum: f64 = 0.0;
        for i in 0..npixels {
            let idx = i * 4;
            let clean_r = clean[idx];
            let noisy_r = noisy[idx];
            let result_r = result[idx];
            noisy_error_sum += (noisy_r - clean_r).abs() as f64;
            result_error_sum += (result_r - clean_r).abs() as f64;
        }
        let nf = npixels as f64;
        let noisy_mae = noisy_error_sum / nf;
        let result_mae = result_error_sum / nf;

        eprintln!(
            "[denoiser proof] Noisy MAE: {:.6}, Denoised MAE: {:.6}, Reduction: {:.1}%",
            noisy_mae,
            result_mae,
            (1.0 - result_mae / noisy_mae.max(f32::EPSILON as f64)) * 100.0
        );

        assert!(
            result_mae < noisy_mae,
            "Denoised MAE ({:.6}) not below noisy MAE ({:.6}) -- denoiser did not reduce error",
            result_mae,
            noisy_mae
        );

        assert!(
            result_mae < noisy_mae * 0.5,
            "Denoised MAE ({:.6}) not below 50% of noisy MAE ({:.6}) -- \
             denoiser reduction too weak",
            result_mae,
            noisy_mae,
        );
    }
}
