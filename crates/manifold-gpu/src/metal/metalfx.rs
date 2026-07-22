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
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLPixelFormat};
use objc2_metal_fx::{
    MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerColorProcessingMode,
    MTLFXSpatialScalerDescriptor, MTLFXTemporalScaler, MTLFXTemporalScalerBase,
    MTLFXTemporalScalerDescriptor,
};

use super::GpuTexture;
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
        device: &ProtocolObject<dyn MTLDevice>,
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

        let scaler = unsafe { desc.newSpatialScalerWithDevice(device) };

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
    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        unsafe {
            self.scaler.setColorTexture(Some(&src.raw));
            self.scaler.setOutputTexture(Some(&dst.raw));
            self.scaler.encodeToCommandBuffer(cmd_buf);
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
pub fn supports_spatial_scaling(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    if !metalfx_available() {
        return false;
    }
    unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(device) }
}

/// Check if MetalFX Temporal Scaler is available on this system.
/// Mirrors [`metalfx_available`] but probes the temporal descriptor class —
/// separately weak-linked, so it can be present/absent independently of the
/// spatial variant on older SDKs.
pub fn metalfx_temporal_available() -> bool {
    use objc2::runtime::AnyClass;
    AnyClass::get(c"MTLFXTemporalScalerDescriptor").is_some()
}

/// MetalFX Temporal Scaler — motion-vector-fed upscaling with history
/// accumulation (RAYTRACING_DESIGN.md §5.2 P4). Unlike the spatial scaler,
/// it consumes depth + motion vectors alongside color and blends across
/// frames; callers must supply a camera-jitter sequence on the color source
/// and drive [`Self::encode`]'s `reset` flag on scene cuts (D3) — the reset
/// signal itself lives above this seam (manifold-renderer), never here.
///
/// Created once per (input_size, output_size, format) combination, same
/// lifecycle contract as [`MetalFxSpatialScaler`].
pub struct MetalFxTemporalScaler {
    scaler: Retained<objc2::runtime::ProtocolObject<dyn MTLFXTemporalScaler>>,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}

// Safety: MetalFX scalers are thread-safe for encoding (Apple docs).
unsafe impl Send for MetalFxTemporalScaler {}
unsafe impl Sync for MetalFxTemporalScaler {}

impl MetalFxTemporalScaler {
    /// Create a temporal scaler for the given input/output dimensions.
    /// `color_format` is used for both the color input and output textures
    /// (matches the spatial scaler's convention). Depth is fixed to
    /// `R32Float` and motion to `Rg16Float` — the exact formats W0 stores
    /// per-scene for RT-enabled scenes (RAYTRACING_DESIGN.md D14).
    /// Returns `None` if MetalFX Temporal is not available on this system.
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        color_format: GpuTextureFormat,
    ) -> Option<Self> {
        if !metalfx_temporal_available() {
            return None;
        }

        let color_pixel_format = to_mtl_pixel_format(color_format);

        let desc = unsafe {
            let desc = MTLFXTemporalScalerDescriptor::init(MTLFXTemporalScalerDescriptor::alloc());
            desc.setInputWidth(input_width as usize);
            desc.setInputHeight(input_height as usize);
            desc.setOutputWidth(output_width as usize);
            desc.setOutputHeight(output_height as usize);
            desc.setColorTextureFormat(color_pixel_format);
            desc.setDepthTextureFormat(MTLPixelFormat::R32Float);
            desc.setMotionTextureFormat(MTLPixelFormat::RG16Float);
            desc.setOutputTextureFormat(color_pixel_format);
            desc
        };

        let scaler = unsafe { desc.newTemporalScalerWithDevice(device) };

        let Some(scaler) = scaler else {
            log::error!(
                "[MetalFX] Failed to create temporal scaler ({}x{} -> {}x{})",
                input_width,
                input_height,
                output_width,
                output_height
            );
            return None;
        };

        unsafe {
            scaler.setInputContentWidth(input_width as usize);
            scaler.setInputContentHeight(input_height as usize);
            // Motion vectors arrive in NDC-space `(dx, dy)` per pixel
            // (GBUFFER_DESIGN.md §2 D5 — the format W0 stores). MetalFX
            // expects motion in pixel units: multiplying by half the
            // render-resolution converts an NDC delta spanning [-1, 1]
            // across the full input width/height into pixels.
            scaler.setMotionVectorScaleX(input_width as f32 * 0.5);
            scaler.setMotionVectorScaleY(input_height as f32 * 0.5);
        }

        log::info!(
            "[MetalFX] Created temporal scaler: {}x{} -> {}x{}",
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

    /// Encode the temporal upscale into a command buffer.
    /// `color`/`depth`/`motion` must match input dimensions, `dst` must
    /// match output dimensions. `jitter_offset_{x,y}` are the SAME subpixel
    /// offsets (in pixels) applied to the camera's projection when `color`
    /// was rendered. `reset` discards history — the caller drives this from
    /// the D3 cut-reset signal; this type has no opinion on when a cut
    /// happened. Caller must end any active encoder on the command buffer
    /// before calling this.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        dst: &GpuTexture,
        jitter_offset_x: f32,
        jitter_offset_y: f32,
        reset: bool,
    ) {
        unsafe {
            self.scaler.setColorTexture(Some(&color.raw));
            self.scaler.setDepthTexture(Some(&depth.raw));
            self.scaler.setMotionTexture(Some(&motion.raw));
            self.scaler.setOutputTexture(Some(&dst.raw));
            self.scaler.setJitterOffsetX(jitter_offset_x);
            self.scaler.setJitterOffsetY(jitter_offset_y);
            self.scaler.setReset(reset);
            self.scaler.encodeToCommandBuffer(cmd_buf);
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

/// Check if MetalFX temporal scaling is supported for the given device.
/// Returns true if the MTLFXTemporalScalerDescriptor class exists AND
/// the device supports the required features.
pub fn supports_temporal_scaling(device: &ProtocolObject<dyn MTLDevice>) -> bool {
    if !metalfx_temporal_available() {
        return false;
    }
    unsafe { MTLFXTemporalScalerDescriptor::supportsDevice(device) }
}

