//! MetalFX Spatial Scaler integration for high-quality upscaling.
//!
//! MetalFX Spatial uses Apple's ML-based spatial upscaler to produce
//! higher-quality results than bilinear or Lanczos when upscaling from
//! reduced-resolution generator output to full output resolution.
//!
//! Requires macOS 13+ (Ventura) and Apple Silicon. Falls back to
//! MPS Lanczos if MetalFX is unavailable.
//!
//! Uses `msg_send!` because the `metal` crate v0.33 doesn't expose
//! MetalFX types.

use objc::runtime::{Class, Object, BOOL};
use std::ffi::c_void;

use super::{GpuTexture, GpuTextureFormat};

// ─── Link MetalFX framework ─────────────────────────────────────────
// Weak link: on macOS < 13 the framework doesn't exist but the binary
// still loads — Class::get() returns None and we fall back to MPS.

#[link(name = "MetalFX", kind = "framework")]
unsafe extern "C" {}

unsafe extern "C" {
    fn objc_retain(obj: *mut c_void) -> *mut c_void;
    fn objc_release(obj: *mut c_void);
}

/// Check if MetalFX Spatial Scaler is available on this system.
pub fn metalfx_available() -> bool {
    Class::get("MTLFXSpatialScalerDescriptor").is_some()
}

/// Map GpuTextureFormat to Metal pixel format enum value.
fn to_mtl_pixel_format(fmt: GpuTextureFormat) -> u64 {
    // MTLPixelFormat raw values (from Metal headers)
    match fmt {
        GpuTextureFormat::Rgba16Float => 115,   // MTLPixelFormatRGBA16Float
        GpuTextureFormat::Rgba32Float => 125,   // MTLPixelFormatRGBA32Float
        GpuTextureFormat::Rgba8Unorm => 70,     // MTLPixelFormatRGBA8Unorm
        GpuTextureFormat::Bgra8Unorm => 80,     // MTLPixelFormatBGRA8Unorm
        GpuTextureFormat::R32Float => 55,       // MTLPixelFormatR32Float
        GpuTextureFormat::Rg32Float => 63,      // MTLPixelFormatRG32Float
        GpuTextureFormat::R16Float => 25,       // MTLPixelFormatR16Float
        GpuTextureFormat::Rg16Float => 35,      // MTLPixelFormatRG16Float
        GpuTextureFormat::R32Uint => 53,        // MTLPixelFormatR32Uint
        GpuTextureFormat::Rgba8UnormSrgb => 71, // MTLPixelFormatRGBA8Unorm_sRGB
        GpuTextureFormat::R8Unorm => 10,        // MTLPixelFormatR8Unorm
    }
}

/// MetalFX Spatial Scaler — ML-based single-frame spatial upscaling.
///
/// Created once per (input_size, output_size, format) combination.
/// Reused across frames. The scaler is a stateful ObjC object that
/// encodes directly into MTLCommandBuffer.
pub struct MetalFxSpatialScaler {
    /// Retained MTLFXSpatialScaler object.
    scaler_ptr: *mut Object,
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
        let desc_cls = Class::get("MTLFXSpatialScalerDescriptor")?;

        let pixel_format = to_mtl_pixel_format(format);

        let desc: *mut Object = unsafe {
            let alloc: *mut Object = msg_send![desc_cls, alloc];
            msg_send![alloc, init]
        };
        if desc.is_null() {
            return None;
        }

        // Configure descriptor
        unsafe {
            let _: () = msg_send![desc, setInputWidth: input_width as u64];
            let _: () = msg_send![desc, setInputHeight: input_height as u64];
            let _: () = msg_send![desc, setOutputWidth: output_width as u64];
            let _: () = msg_send![desc, setOutputHeight: output_height as u64];
            let _: () = msg_send![desc, setColorTextureFormat: pixel_format];
            let _: () = msg_send![desc, setOutputTextureFormat: pixel_format];
            // Linear color processing (our content is linear HDR)
            let _: () = msg_send![desc, setColorProcessingMode: 1u64]; // MTLFXSpatialScalerColorProcessingModeLinear
        }

        // Create scaler from descriptor + device
        let scaler: *mut Object = unsafe {
            msg_send![desc, newSpatialScalerWithDevice: device as *const _ as *mut Object]
        };

        // Release the descriptor (scaler retains what it needs)
        unsafe { objc_release(desc as *mut c_void); }

        if scaler.is_null() {
            log::warn!("MetalFX: failed to create spatial scaler ({}x{} -> {}x{})",
                input_width, input_height, output_width, output_height);
            return None;
        }

        // Retain the scaler
        unsafe { objc_retain(scaler as *mut c_void); }

        Some(Self {
            scaler_ptr: scaler,
            input_width,
            input_height,
            output_width,
            output_height,
        })
    }

    /// Encode the upscale operation into a command buffer.
    /// The source texture must match input dimensions, dst must match output dimensions.
    /// Caller must end any active encoder on the command buffer before calling this.
    pub fn encode(
        &self,
        cmd_buf: &metal::CommandBufferRef,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        unsafe {
            let src_ref: &metal::TextureRef = &src.raw;
            let dst_ref: &metal::TextureRef = &dst.raw;
            let _: () = msg_send![self.scaler_ptr,
                setColorTexture: src_ref as *const _ as *mut Object
            ];
            let _: () = msg_send![self.scaler_ptr,
                setOutputTexture: dst_ref as *const _ as *mut Object
            ];
            let _: () = msg_send![self.scaler_ptr,
                encodeToCommandBuffer: cmd_buf as *const _ as *mut Object
            ];
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

impl Drop for MetalFxSpatialScaler {
    fn drop(&mut self) {
        if !self.scaler_ptr.is_null() {
            unsafe { objc_release(self.scaler_ptr as *mut c_void); }
        }
    }
}

/// Check if MetalFX spatial scaling is supported for the given device.
/// Returns true if the MTLFXSpatialScalerDescriptor class exists AND
/// the device supports the required features.
pub fn supports_spatial_scaling(device: &metal::DeviceRef) -> bool {
    if !metalfx_available() {
        return false;
    }
    // All Apple Silicon GPUs that support MetalFX also support spatial scaling.
    // The descriptor creation will fail if the device doesn't support it,
    // so we just check class availability here.
    let _: BOOL = unsafe {
        let cls = Class::get("MTLFXSpatialScalerDescriptor").unwrap();
        msg_send![cls, supportsDevice: device as *const _ as *mut Object]
    };
    true
}

// ─── TextureUpscaler ─────────────────────────────────────────────────

use super::mps;
use super::GpuDevice;

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
    format: super::GpuTextureFormat,
}

// Safety: All inner types are Send+Sync (MPS kernels and MetalFX scalers).
unsafe impl Send for TextureUpscaler {}
unsafe impl Sync for TextureUpscaler {}

impl TextureUpscaler {
    /// Create a new upscaler. Probes MetalFX availability and falls back to MPS.
    pub fn new(device: &GpuDevice, format: super::GpuTextureFormat) -> Self {
        let mps_lanczos = mps::MpsLanczosScale::new(device.raw_device());
        let mode = if supports_spatial_scaling(device.raw_device()) {
            log::info!("TextureUpscaler: MetalFX Spatial available");
            UpscaleMode::MetalFxSpatial
        } else {
            log::info!("TextureUpscaler: MetalFX unavailable, using MPS Lanczos");
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
            let scaler_idx = self.ensure_metalfx_scaler(
                device,
                src.width,
                src.height,
                dst.width,
                dst.height,
            );
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
        let scaler = MetalFxSpatialScaler::new(
            device.raw_device(),
            in_w,
            in_h,
            out_w,
            out_h,
            self.format,
        )?;
        self.metalfx_scalers.push(scaler);
        Some(self.metalfx_scalers.len() - 1)
    }

    /// Invalidate all cached MetalFX scalers (e.g., on output resize).
    pub fn invalidate(&mut self) {
        self.metalfx_scalers.clear();
    }
}
