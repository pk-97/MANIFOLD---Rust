//! MetalFX behind backend-neutral types: the spatial upscaler, the temporal
//! upscaler and the temporal denoiser. Callers above manifold-gpu see only
//! these and manifold-gpu's own resource types; the MetalFX scalers and the
//! raw command buffer stay here.
//!
//! The temporal types prefer the Metal 4 scalers (GPU-side sync, no CPU
//! stalls) and fall back to classic MTLFX (RAYTRACING_DESIGN.md section 17.7
//! DN-K).

use super::denoiser::{GpuDenoiser, MetalFxDenoisedScaler};
use super::metalfx::{
    MetalFxSpatialScaler, MetalFxTemporalScaler, supports_spatial_scaling,
    supports_temporal_scaling,
};
use super::metalfx_m4::{Metal4FxDenoisedScaler, Metal4FxTemporalScaler, supports_m4_temporal};
use super::{GpuDevice, GpuEncoder, GpuTexture};
use crate::GpuTextureFormat;

/// Every scaler here reads and writes this format.
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Single-frame upscaling from `src` to `dst` dimensions.
pub struct GpuSpatialUpscaler {
    scaler: MetalFxSpatialScaler,
}

impl GpuSpatialUpscaler {
    /// `None` when MetalFX Spatial cannot be built for these dimensions.
    pub fn new(device: &GpuDevice, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Option<Self> {
        MetalFxSpatialScaler::new(device.raw_device(), src_w, src_h, dst_w, dst_h, FORMAT)
            .map(|scaler| Self { scaler })
    }

    /// Whether this device can run the spatial upscaler at all.
    pub fn is_available(device: &GpuDevice) -> bool {
        supports_spatial_scaling(device.raw_device())
    }

    /// Upscale `source` into `output` (at the `dst` dimensions). Ends the
    /// encoder's open pass.
    pub fn encode(&self, enc: &mut GpuEncoder, source: &GpuTexture, output: &GpuTexture) {
        self.scaler.encode(enc.raw_cmd_buf(), source, output);
    }
}

/// Motion-vector-fed upscaling that accumulates history across frames.
pub struct GpuTemporalUpscaler {
    classic: Option<MetalFxTemporalScaler>,
    m4: Option<Metal4FxTemporalScaler>,
}

impl GpuTemporalUpscaler {
    /// `None` when neither the Metal 4 nor the classic scaler can be built.
    pub fn new(device: &GpuDevice, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Option<Self> {
        let m4 = device.mtl4_bridge().and_then(|bridge| {
            Metal4FxTemporalScaler::new(
                device.raw_device(),
                bridge,
                src_w,
                src_h,
                dst_w,
                dst_h,
                FORMAT,
            )
        });
        let classic = if m4.is_none() {
            MetalFxTemporalScaler::new(device.raw_device(), src_w, src_h, dst_w, dst_h, FORMAT)
        } else {
            None
        };
        if m4.is_none() && classic.is_none() {
            return None;
        }
        Some(Self { classic, m4 })
    }

    /// Whether this device can run the temporal upscaler at all.
    pub fn is_available(device: &GpuDevice) -> bool {
        supports_m4_temporal(device.raw_device()) || supports_temporal_scaling(device.raw_device())
    }

    /// Upscale `color`/`depth`/`motion` into `output`. The jitter is the
    /// subpixel offset the camera rendered `color` with; `reset` discards
    /// history. `false` when the Metal 4 allocator ring was full and the
    /// frame was skipped: `output` keeps the last upscaled frame.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        output: &GpuTexture,
        jitter_offset_x: f32,
        jitter_offset_y: f32,
        reset: bool,
    ) -> bool {
        let cmd_buf = enc.raw_cmd_buf();
        if let Some(ref m4) = self.m4 {
            return m4.encode(
                cmd_buf,
                color,
                depth,
                motion,
                output,
                jitter_offset_x,
                jitter_offset_y,
                reset,
            );
        }
        let classic = self.classic.as_ref().expect("new() guarantees one scaler");
        classic.encode(
            cmd_buf,
            color,
            depth,
            motion,
            output,
            jitter_offset_x,
            jitter_offset_y,
            reset,
        );
        true
    }

    /// Rebuild for new dimensions, keeping the scaler generation that was
    /// live; a Metal 4 rebuild that fails falls back to classic. `false`
    /// when no scaler could be built.
    pub fn resize(
        &mut self,
        device: &GpuDevice,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> bool {
        if self.m4.is_some() {
            self.m4 = device.mtl4_bridge().and_then(|bridge| {
                Metal4FxTemporalScaler::new(
                    device.raw_device(),
                    bridge,
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    FORMAT,
                )
            });
            if self.m4.is_none() {
                self.classic = MetalFxTemporalScaler::new(
                    device.raw_device(),
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    FORMAT,
                );
            }
        } else {
            self.classic =
                MetalFxTemporalScaler::new(device.raw_device(), src_w, src_h, dst_w, dst_h, FORMAT);
        }
        self.m4.is_some() || self.classic.is_some()
    }
}

/// Fused temporal denoising and optional upscaling of a ray-traced frame.
pub struct GpuTemporalDenoiser {
    classic: Option<MetalFxDenoisedScaler>,
    m4: Option<Metal4FxDenoisedScaler>,
    depth_reversed: bool,
}

impl GpuTemporalDenoiser {
    /// `None` when neither the Metal 4 nor the classic denoiser can be built
    /// (before macOS Tahoe).
    pub fn new(
        device: &GpuDevice,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        depth_reversed: bool,
    ) -> Option<Self> {
        let m4 = device.mtl4_bridge().and_then(|bridge| {
            Metal4FxDenoisedScaler::new(
                device.raw_device(),
                bridge,
                input_width,
                input_height,
                output_width,
                output_height,
                FORMAT,
                depth_reversed,
            )
        });
        let classic = if m4.is_none() {
            MetalFxDenoisedScaler::new(
                device.raw_device(),
                input_width,
                input_height,
                output_width,
                output_height,
                FORMAT,
                depth_reversed,
            )
        } else {
            None
        };
        if m4.is_none() && classic.is_none() {
            return None;
        }
        Some(Self {
            classic,
            m4,
            depth_reversed,
        })
    }

    /// This frame's camera jitter in pixels; (0, 0) when not jittering.
    pub fn set_jitter(&self, x: f32, y: f32) {
        if let Some(ref m4) = self.m4 {
            m4.set_jitter(x, y);
        } else if let Some(ref classic) = self.classic {
            classic.set_jitter(x, y);
        }
    }

    /// Denoise into `output`. `false` when the Metal 4 allocator ring was
    /// full and the frame was skipped: `output` is untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        normal: &GpuTexture,
        roughness: &GpuTexture,
        diffuse_albedo: &GpuTexture,
        specular_albedo: &GpuTexture,
        specular_hit_distance: &GpuTexture,
        output: &GpuTexture,
        reset: bool,
        reactive_mask: Option<&GpuTexture>,
    ) -> bool {
        let cmd_buf = enc.raw_cmd_buf();
        if let Some(ref m4) = self.m4 {
            m4.encode(
                cmd_buf,
                color,
                depth,
                self.depth_reversed,
                motion,
                normal,
                roughness,
                diffuse_albedo,
                specular_albedo,
                specular_hit_distance,
                None, // exposure — the denoiser normalises internally
                output,
                reset,
                reactive_mask,
            )
        } else if let Some(ref classic) = self.classic {
            classic.encode(
                cmd_buf,
                color,
                depth,
                self.depth_reversed,
                motion,
                normal,
                roughness,
                diffuse_albedo,
                specular_albedo,
                specular_hit_distance,
                None,
                output,
                reset,
                reactive_mask,
            );
            true
        } else {
            false
        }
    }
}
