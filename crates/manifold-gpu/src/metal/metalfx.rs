//! MetalFX Spatial Scaler integration for high-quality upscaling.
//!
//! MetalFX Spatial uses Apple's ML-based spatial upscaler to produce
//! higher-quality results than bilinear or Lanczos when upscaling from
//! reduced-resolution generator output to full output resolution.
//!
//! Requires macOS 13+ (Ventura) and Apple Silicon. Falls back to
//! MPS Lanczos if MetalFX is unavailable.
//!
//! Uses `objc2-metal-fx` typed bindings. Public API still accepts
//! `metal::DeviceRef` / `metal::CommandBufferRef` so downstream crates
//! don't need to change — bridging to `objc2-metal` happens at call
//! boundaries via `objc2_bridge`.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_metal::MTLPixelFormat;
use objc2_metal_fx::{
    MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerColorProcessingMode,
    MTLFXSpatialScalerDescriptor,
};

use super::GpuTexture;
use super::objc2_bridge::{cmd_buf_as_objc2, device_as_objc2, texture_as_objc2};
use crate::GpuTextureFormat;

/// Check if MetalFX Spatial Scaler is available on this system.
///
/// Availability is determined by trying to resolve `MTLFXSpatialScalerDescriptor`.
/// objc2-metal-fx weak-links the framework, so on macOS < 13 the class lookup
/// returns None and this function returns false.
pub fn metalfx_available() -> bool {
    // The class may not exist on older macOS. objc2 handles weak linking
    // transparently — if the framework loader couldn't resolve the class,
    // any call into it panics. We probe by catching via device support check
    // in `supports_spatial_scaling` instead; here we just assume availability
    // is tied to macOS 13+. The downstream helper still gates on
    // supportsDevice:, which is the authoritative check.
    // For a cheaper probe, use runtime class lookup:
    use objc2::runtime::AnyClass;
    AnyClass::get(c"MTLFXSpatialScalerDescriptor").is_some()
}

/// Map GpuTextureFormat to MTLPixelFormat.
fn to_mtl_pixel_format(fmt: GpuTextureFormat) -> MTLPixelFormat {
    match fmt {
        GpuTextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
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

/// MetalFX Spatial Scaler — ML-based single-frame spatial upscaling.
///
/// Created once per (input_size, output_size, format) combination.
/// Reused across frames. The scaler is a stateful ObjC object that
/// encodes directly into MTLCommandBuffer.
pub struct MetalFxSpatialScaler {
    scaler: Retained<objc2::runtime::ProtocolObject<dyn MTLFXSpatialScaler>>,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

// Safety: MetalFX scalers are thread-safe for encoding (Apple docs).
unsafe impl Send for MetalFxSpatialScaler {}
unsafe impl Sync for MetalFxSpatialScaler {}

impl MetalFxSpatialScaler {
    /// Create a spatial scaler for the given input/output dimensions and format.
    /// Returns None if MetalFX is not available on this system.
    pub fn new(
        device: &metal::DeviceRef,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        format: GpuTextureFormat,
    ) -> Option<Self> {
        if !metalfx_available() {
            return None;
        }

        let pixel_format = to_mtl_pixel_format(format);

        let desc = unsafe {
            let desc = MTLFXSpatialScalerDescriptor::init(MTLFXSpatialScalerDescriptor::alloc());
            desc.setInputWidth(input_width as usize);
            desc.setInputHeight(input_height as usize);
            desc.setOutputWidth(output_width as usize);
            desc.setOutputHeight(output_height as usize);
            desc.setColorTextureFormat(pixel_format);
            desc.setOutputTextureFormat(pixel_format);
            // Linear color processing (our content is linear HDR).
            desc.setColorProcessingMode(MTLFXSpatialScalerColorProcessingMode::Linear);
            desc
        };

        let device_obj = unsafe { device_as_objc2(device) };
        let scaler = unsafe { desc.newSpatialScalerWithDevice(device_obj) };

        let Some(scaler) = scaler else {
            log::error!(
                "[MetalFX] Failed to create spatial scaler ({}x{} -> {}x{})",
                input_width,
                input_height,
                output_width,
                output_height
            );
            return None;
        };

        log::info!(
            "[MetalFX] Created spatial scaler: {}x{} -> {}x{}",
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

    /// Encode the upscale operation into a command buffer.
    /// The source texture must match input dimensions, dst must match output dimensions.
    /// Caller must end any active encoder on the command buffer before calling this.
    pub fn encode(&self, cmd_buf: &metal::CommandBufferRef, src: &GpuTexture, dst: &GpuTexture) {
        unsafe {
            let src_obj = texture_as_objc2(&src.raw);
            let dst_obj = texture_as_objc2(&dst.raw);
            let cb_obj = cmd_buf_as_objc2(cmd_buf);
            self.scaler.setColorTexture(Some(src_obj));
            self.scaler.setOutputTexture(Some(dst_obj));
            self.scaler.encodeToCommandBuffer(cb_obj);
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

/// Check if MetalFX spatial scaling is supported for the given device.
/// Returns true if the MTLFXSpatialScalerDescriptor class exists AND
/// the device supports the required features.
pub fn supports_spatial_scaling(device: &metal::DeviceRef) -> bool {
    if !metalfx_available() {
        return false;
    }
    let device_obj = unsafe { device_as_objc2(device) };
    unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(device_obj) }
}

// ─── TextureUpscaler ─────────────────────────────────────────────────

use super::GpuDevice;
use super::mps;

/// Upscale mode for generator internal resolution scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpscaleMode {
    /// MetalFX Spatial (ML-based, best quality). Requires macOS 13+.
    MetalFxSpatial,
    /// MPS Lanczos (high-quality resampling, always available on Apple Silicon).
    MpsLanczos,
}

/// Cached texture upscaler that tries MetalFX Spatial first,
/// falling back to MPS Lanczos. Manages per-dimension-pair MetalFX scalers.
///
/// Created once per GeneratorRenderer, reused across frames.
pub struct TextureUpscaler {
    mps_lanczos: mps::MpsLanczosScale,
    metalfx_scalers: Vec<MetalFxSpatialScaler>,
    mode: UpscaleMode,
    format: GpuTextureFormat,
}

// Safety: All inner types are Send+Sync (MPS kernels and MetalFX scalers).
unsafe impl Send for TextureUpscaler {}
unsafe impl Sync for TextureUpscaler {}

impl TextureUpscaler {
    /// Create a new upscaler. Probes MetalFX availability and falls back to MPS.
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let mps_lanczos = mps::MpsLanczosScale::new(device.raw_device());
        let mode = if supports_spatial_scaling(device.raw_device()) {
            log::info!("[TextureUpscaler] MetalFX Spatial available — using ML upscaling");
            UpscaleMode::MetalFxSpatial
        } else {
            log::warn!("[TextureUpscaler] MetalFX unavailable — using MPS Lanczos");
            UpscaleMode::MpsLanczos
        };
        Self {
            mps_lanczos,
            metalfx_scalers: Vec::with_capacity(4),
            mode,
            format,
        }
    }

    /// Get current upscale mode.
    pub fn mode(&self) -> UpscaleMode {
        self.mode
    }

    /// Override the upscale mode (e.g., from a user setting).
    pub fn set_mode(&mut self, mode: UpscaleMode) {
        self.mode = mode;
    }

    /// Upscale src → dst using the best available method.
    /// Caller must ensure src and dst have different dimensions (src smaller).
    pub fn upscale(
        &mut self,
        enc: &mut super::GpuEncoder,
        device: &GpuDevice,
        src: &super::GpuTexture,
        dst: &super::GpuTexture,
    ) {
        if self.mode == UpscaleMode::MetalFxSpatial {
            // Find or create a MetalFX scaler for this dimension pair
            let scaler_idx =
                self.ensure_metalfx_scaler(device, src.width, src.height, dst.width, dst.height);
            if let Some(idx) = scaler_idx {
                let scaler = &self.metalfx_scalers[idx];
                enc.end_current();
                scaler.encode(enc.cmd_buf(), src, dst);
                return;
            }
            // MetalFX creation failed — fall through to MPS
        }

        // MPS Lanczos fallback
        enc.end_current();
        self.mps_lanczos.set_transform(&mps::MpsScaleTransform {
            scale_x: dst.width as f64 / src.width as f64,
            scale_y: dst.height as f64 / src.height as f64,
            translate_x: 0.0,
            translate_y: 0.0,
        });
        self.mps_lanczos.encode(enc.cmd_buf(), &src.raw, &dst.raw);
    }

    /// Find or create a MetalFX scaler for the given dimensions.
    fn ensure_metalfx_scaler(
        &mut self,
        device: &GpuDevice,
        in_w: u32,
        in_h: u32,
        out_w: u32,
        out_h: u32,
    ) -> Option<usize> {
        // Check cache
        for (i, scaler) in self.metalfx_scalers.iter().enumerate() {
            if scaler.matches(in_w, in_h, out_w, out_h) {
                return Some(i);
            }
        }
        // Create new
        let scaler =
            MetalFxSpatialScaler::new(device.raw_device(), in_w, in_h, out_w, out_h, self.format)?;
        self.metalfx_scalers.push(scaler);
        Some(self.metalfx_scalers.len() - 1)
    }

    /// Invalidate all cached MetalFX scalers (e.g., on output resize).
    pub fn invalidate(&mut self) {
        self.metalfx_scalers.clear();
    }
}
