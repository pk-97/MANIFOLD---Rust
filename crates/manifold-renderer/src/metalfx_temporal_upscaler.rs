//! MetalFX Temporal Scaler integration — motion-vector-fed,
//! history-accumulating upscaling (RAYTRACING_DESIGN.md section 5.2 P4).
//!
//! Companion to `metalfx_upscaler.rs` (the spatial, single-frame variant,
//! the template this module follows). The temporal variant additionally
//! consumes depth + motion vectors (the exact formats W0 stores per-scene
//! for RT-enabled scenes, GBUFFER_DESIGN.md section 2 D2/D5) and blends across
//! frames, so callers must:
//!   1. Apply [`jitter_offset`] to the scene camera's projection BEFORE
//!      rendering color/depth/velocity at `src_w x src_h` — this is what
//!      lets the temporal history accumulate more detail than a single
//!      frame's samples carry.
//!   2. Drive `reset` from history-reset detection — see
//!      `crate::node_graph::temporal_reset::TemporalResetDetector`, the
//!      SHARED reset-detection path this and P2's accumulator both wire
//!      onto. This module has no opinion on cut detection; it only takes
//!      the resulting `bool`.
//!
//! Usage:
//! ```ignore
//! let mut upscaler = MetalFxTemporalUpscaler::new(device, src_w, src_h, dst_w, dst_h)?;
//! let mut resets = TemporalResetDetector::new();
//! // each frame:
//! let reset = resets.detect_reset(owner_key, &frame_time);
//! let (jx, jy) = jitter_offset(frame_index, 8);
//! // ... apply (jx, jy) to the camera projection before rendering color/depth/velocity ...
//! upscaler.upscale(&mut gpu, &color, &depth, &velocity, jx, jy, reset);
//! // read from upscaler.output.texture (at dst_w × dst_h)
//! ```

#[cfg(target_os = "macos")]
mod imp {
    use crate::gpu_encoder::GpuEncoder;
    use crate::render_target::RenderTarget;

    /// GPU temporal upscaler: MetalFX Temporal. Created once per
    /// (src_dims, dst_dims); on a dimension change the caller retires the
    /// whole object (BUG-rnnr — an in-place resize would drop the old
    /// scaler and its output target while in-flight frames still sample
    /// the scaler-owned temporal-history textures) and constructs a new
    /// one. Same lifecycle contract as `MetalFxFullFrameUpscaler`.
    ///
    /// Internally prefers `Metal4FxTemporalScaler` (MTL4, GPU-side sync)
    /// and falls back to `MetalFxTemporalScaler` (classic MTLFX) when
    /// unavailable — same DN-K preference as `crate::denoiser::Denoiser`.
    pub struct MetalFxTemporalUpscaler {
        /// Classic MTLFX scaler (fallback).
        classic: Option<manifold_gpu::metalfx::MetalFxTemporalScaler>,
        /// MTL4 scaler with GPU-side sync (preferred).
        m4: Option<manifold_gpu::metalfx_m4::Metal4FxTemporalScaler>,
        /// Output at dst_w × dst_h. Blit this to IOSurface / downstream.
        pub output: RenderTarget,
        pub src_w: u32,
        pub src_h: u32,
        pub dst_w: u32,
        pub dst_h: u32,
    }

    impl MetalFxTemporalUpscaler {
        /// Create an upscaler for the given dimensions.
        /// Returns `None` if MetalFX Temporal is not available on this device.
        /// Prefers MTL4 when available, falls back to classic MTLFX.
        pub fn new(
            device: &manifold_gpu::GpuDevice,
            src_w: u32,
            src_h: u32,
            dst_w: u32,
            dst_h: u32,
        ) -> Option<Self> {
            let fmt = manifold_gpu::GpuTextureFormat::Rgba16Float;
            let m4 = device.mtl4_bridge().and_then(|bridge| {
                manifold_gpu::metalfx_m4::Metal4FxTemporalScaler::new(
                    device.raw_device(),
                    bridge,
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    fmt,
                )
            });
            let classic = if m4.is_none() {
                manifold_gpu::metalfx::MetalFxTemporalScaler::new(
                    device.raw_device(),
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    fmt,
                )
            } else {
                None
            };
            if m4.is_none() && classic.is_none() {
                return None;
            }
            let output = RenderTarget::new(device, dst_w, dst_h, fmt, "MetalFX Temporal Output");
            Some(Self {
                classic,
                m4,
                output,
                src_w,
                src_h,
                dst_w,
                dst_h,
            })
        }

        /// Returns `true` if MetalFX Temporal Scaler is supported on this device.
        pub fn is_available(device: &manifold_gpu::GpuDevice) -> bool {
            manifold_gpu::metalfx_m4::supports_m4_temporal(device.raw_device())
                || manifold_gpu::metalfx::supports_temporal_scaling(device.raw_device())
        }

        /// Upscale `color`/`depth`/`motion` (all at src_w × src_h) →
        /// `self.output` (at dst_w × dst_h). `jitter_offset_{x,y}` are the
        /// SAME subpixel pixel offsets applied to the camera when `color`
        /// was rendered ([`super::jitter_offset`]). `reset` discards
        /// history for this frame — the caller decides when, this type
        /// just encodes it.
        ///
        /// Returns `true` if the scaler wrote to `self.output`. On the MTL4
        /// path, `false` means the per-device allocator ring was saturated
        /// and the frame was skipped — `self.output` still holds the last
        /// upscaled frame, and the caller should present it as-is (one-frame
        /// freeze under extreme backpressure, recovers when the ring drains).
        /// The classic path always returns `true`.
        #[allow(clippy::too_many_arguments)]
        pub fn upscale(
            &self,
            gpu: &mut GpuEncoder,
            color: &manifold_gpu::GpuTexture,
            depth: &manifold_gpu::GpuTexture,
            motion: &manifold_gpu::GpuTexture,
            jitter_offset_x: f32,
            jitter_offset_y: f32,
            reset: bool,
        ) -> bool {
            let cmd_buf = gpu.native_enc.raw_cmd_buf();
            if let Some(ref m4) = self.m4 {
                return m4.encode(
                    cmd_buf,
                    color,
                    depth,
                    motion,
                    &self.output.texture,
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
                &self.output.texture,
                jitter_offset_x,
                jitter_offset_y,
                reset,
            );
            true
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::MetalFxTemporalUpscaler;

/// Halton(2,3) low-discrepancy jitter sequence — the standard TAA/MetalFX
/// subpixel camera jitter (the same base pair Apple's own MetalFX temporal
/// sample code uses). Pure function, independent of `target_os` — testable
/// without a GPU device.
///
/// Returns a subpixel offset in `[-0.5, 0.5)` PIXEL units (at the INPUT/
/// render resolution) for the given monotonic frame index.
/// `sequence_len` is the period after which the sequence repeats — 8 is
/// the common TAA choice, long enough to decorrelate samples, short enough
/// to converge quickly after a reset.
pub fn jitter_offset(frame_index: u32, sequence_len: u32) -> (f32, f32) {
    // Halton indices are conventionally 1-based (index 0 is degenerate:
    // both bases produce 0.0, i.e. no jitter at all).
    let i = (frame_index % sequence_len.max(1)) + 1;
    (halton(i, 2) - 0.5, halton(i, 3) - 0.5)
}

fn halton(mut index: u32, base: u32) -> f32 {
    let mut f = 1.0f32;
    let mut r = 0.0f32;
    while index > 0 {
        f /= base as f32;
        r += f * (index % base) as f32;
        index /= base;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halton_base2_matches_known_sequence() {
        // Halton base-2: 0.5, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875, ...
        let expected = [0.5, 0.25, 0.75, 0.125, 0.625, 0.375, 0.875];
        for (i, &e) in expected.iter().enumerate() {
            let got = halton((i + 1) as u32, 2);
            assert!((got - e).abs() < 1e-6, "halton({}, 2) = {got}, expected {e}", i + 1);
        }
    }

    #[test]
    fn halton_base3_matches_known_sequence() {
        // Halton base-3: 1/3, 2/3, 1/9, 4/9, 7/9, ...
        let expected = [1.0 / 3.0, 2.0 / 3.0, 1.0 / 9.0, 4.0 / 9.0, 7.0 / 9.0];
        for (i, &e) in expected.iter().enumerate() {
            let got = halton((i + 1) as u32, 3);
            assert!((got - e).abs() < 1e-6, "halton({}, 3) = {got}, expected {e}", i + 1);
        }
    }

    #[test]
    fn jitter_offset_stays_in_subpixel_range() {
        for i in 0..64 {
            let (x, y) = jitter_offset(i, 8);
            assert!((-0.5..0.5).contains(&x), "x={x} out of range at frame {i}");
            assert!((-0.5..0.5).contains(&y), "y={y} out of range at frame {i}");
        }
    }

    #[test]
    fn jitter_offset_repeats_with_period() {
        for i in 0..16 {
            assert_eq!(jitter_offset(i, 8), jitter_offset(i + 8, 8));
        }
    }

    #[test]
    fn jitter_offset_is_not_constant_across_the_sequence() {
        // A degenerate all-zero (or all-identical) jitter sequence would
        // silently defeat temporal supersampling — guard against that.
        let first = jitter_offset(0, 8);
        let distinct = (1..8).any(|i| jitter_offset(i, 8) != first);
        assert!(distinct, "jitter sequence must vary across its period");
    }
}
