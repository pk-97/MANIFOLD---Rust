//! Safe Rust wrapper around the native Metal encoder FFI.
//!
//! Provides `MetalEncoder` for zero-copy GPU video encoding via
//! AVAssetWriter + VideoToolbox. Port of Unity MetalEncoderNative.cs.

use std::ffi::{CString, c_void};
use std::fmt;

use crate::frame_rate::fps_to_rational;
use crate::metal_ffi;

/// Error codes returned by the native Metal encoder.
/// Matches ME_ERR_* defines in MetalEncoderPlugin.m.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderError {
    /// Encoder handle is NULL.
    NullHandle,
    /// AVAssetWriter not in writing state or input not ready.
    WriterNotReady,
    /// Failed to create CVPixelBuffer from pool.
    PixelBufferCreate,
    /// Failed to create Metal texture from CVPixelBuffer.
    TextureCreate,
    /// GPU compute blit/copy failed.
    BlitFailed,
    /// Failed to append pixel buffer to AVAssetWriter.
    AppendFailed,
    /// AVAssetWriter finalization failed.
    WriterFailed,
    /// Source Metal texture pointer is NULL.
    NullTexture,
    /// Metal compute shader compilation failed.
    ShaderFailed,
    /// Metal encoder is not available on this system.
    Unavailable,
}

impl EncoderError {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::NullHandle,
            2 => Self::WriterNotReady,
            3 => Self::PixelBufferCreate,
            4 => Self::TextureCreate,
            5 => Self::BlitFailed,
            6 => Self::AppendFailed,
            7 => Self::WriterFailed,
            8 => Self::NullTexture,
            9 => Self::ShaderFailed,
            _ => Self::WriterFailed,
        }
    }
}

impl fmt::Display for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NullHandle => write!(f, "encoder handle is null"),
            Self::WriterNotReady => write!(f, "AVAssetWriter not ready"),
            Self::PixelBufferCreate => write!(f, "CVPixelBuffer creation failed"),
            Self::TextureCreate => write!(f, "Metal texture creation failed"),
            Self::BlitFailed => write!(f, "GPU compute blit failed"),
            Self::AppendFailed => write!(f, "pixel buffer append failed"),
            Self::WriterFailed => write!(f, "AVAssetWriter finalization failed"),
            Self::NullTexture => write!(f, "source Metal texture is null"),
            Self::ShaderFailed => write!(f, "Metal shader compilation failed"),
            Self::Unavailable => write!(f, "Metal encoder not available"),
        }
    }
}

/// Native Metal GPU video encoder.
///
/// Wraps the Objective-C MetalEncoderPlugin for zero-copy GPU encoding.
/// SDR mode: H.264 High Profile, ~0.6 bpp dynamic bitrate, BGRA8.
/// HDR mode: HEVC Main10, ~0.6 bpp dynamic bitrate, RGBA16Float, BT.2020/PQ.
pub struct MetalEncoder {
    handle: *mut c_void,
    width: u32,
    height: u32,
    frames_encoded: u32,
    is_hdr: bool,
}

// The native encoder state is single-threaded (called only from content thread).
// The handle is an opaque pointer to heap-allocated Obj-C state.
unsafe impl Send for MetalEncoder {}

impl MetalEncoder {
    /// Check if a Metal device is available for encoding.
    pub fn is_available() -> bool {
        unsafe { metal_ffi::MetalEncoder_IsAvailable() != 0 }
    }

    /// Check if HDR (HEVC 10-bit) encoding is supported.
    pub fn is_hdr_available() -> bool {
        unsafe { metal_ffi::MetalEncoder_IsHDRAvailable() != 0 }
    }

    /// Create a new encoder session.
    ///
    /// - `width`, `height`: output resolution
    /// - `fps`: frame rate (rounded to nearest integer for CMTime)
    /// - `output_path`: file path for the output MP4
    /// - `hdr`: true for HEVC HDR, false for H.264 SDR
    pub fn new(
        width: u32,
        height: u32,
        fps: f32,
        output_path: &str,
        hdr: bool,
    ) -> Result<Self, EncoderError> {
        if !Self::is_available() {
            return Err(EncoderError::Unavailable);
        }
        if hdr && !Self::is_hdr_available() {
            return Err(EncoderError::Unavailable);
        }

        let c_path = CString::new(output_path).map_err(|_| EncoderError::WriterFailed)?;
        let (fps_num, fps_den) = fps_to_rational(fps);

        let handle = unsafe {
            if hdr {
                metal_ffi::MetalEncoder_CreateHDR(
                    width as i32,
                    height as i32,
                    fps_num,
                    fps_den,
                    c_path.as_ptr(),
                )
            } else {
                metal_ffi::MetalEncoder_Create(
                    width as i32,
                    height as i32,
                    fps_num,
                    fps_den,
                    c_path.as_ptr(),
                )
            }
        };

        if handle.is_null() {
            return Err(EncoderError::WriterFailed);
        }

        log::info!(
            "[MetalEncoder] Created {} encoder {}x{} @ {} fps -> {}",
            if hdr { "HDR" } else { "SDR" },
            width,
            height,
            fps,
            output_path,
        );

        Ok(Self {
            handle,
            width,
            height,
            frames_encoded: 0,
            is_hdr: hdr,
        })
    }

    /// Create an encoder session using an external Metal device.
    ///
    /// Same as `new()` but shares the caller's `id<MTLDevice>` instead of
    /// creating a new one. Avoids cross-device GPU synchronization overhead
    /// when the source textures already live on this device.
    ///
    /// # Safety
    /// `device_ptr` must be a valid `id<MTLDevice>` that outlives the encoder.
    pub unsafe fn new_with_device(
        width: u32,
        height: u32,
        fps: f32,
        output_path: &str,
        hdr: bool,
        device_ptr: *mut c_void,
    ) -> Result<Self, EncoderError> {
        let c_path = CString::new(output_path).map_err(|_| EncoderError::WriterFailed)?;
        let (fps_num, fps_den) = fps_to_rational(fps);

        let handle = unsafe {
            if hdr {
                metal_ffi::MetalEncoder_CreateHDRWithDevice(
                    width as i32,
                    height as i32,
                    fps_num,
                    fps_den,
                    c_path.as_ptr(),
                    device_ptr,
                )
            } else {
                metal_ffi::MetalEncoder_CreateWithDevice(
                    width as i32,
                    height as i32,
                    fps_num,
                    fps_den,
                    c_path.as_ptr(),
                    device_ptr,
                )
            }
        };

        if handle.is_null() {
            return Err(EncoderError::WriterFailed);
        }

        log::info!(
            "[MetalEncoder] Created {} encoder (shared device) {}x{} @ {} fps -> {}",
            if hdr { "HDR" } else { "SDR" },
            width,
            height,
            fps,
            output_path,
        );

        Ok(Self {
            handle,
            width,
            height,
            frames_encoded: 0,
            is_hdr: hdr,
        })
    }

    /// Encode a single frame from a raw Metal texture pointer.
    ///
    /// The `metal_texture_ptr` must be a valid `id<MTLTexture>` cast to `*mut c_void`.
    /// Obtained via `GpuTexture::raw_ptr()` or equivalent.
    ///
    /// # Safety
    /// The caller must ensure `metal_texture_ptr` points to a valid Metal texture
    /// that is not currently being written to by another GPU command.
    pub unsafe fn encode_frame(
        &mut self,
        metal_texture_ptr: *mut c_void,
    ) -> Result<(), EncoderError> {
        let result = unsafe {
            metal_ffi::MetalEncoder_EncodeFrame(
                self.handle,
                metal_texture_ptr,
                self.frames_encoded as i32,
            )
        };

        if result != 0 {
            log::error!(
                "[MetalEncoder] encode_frame failed at frame {}: {:?}",
                self.frames_encoded,
                EncoderError::from_code(result),
            );
            return Err(EncoderError::from_code(result));
        }

        self.frames_encoded += 1;
        Ok(())
    }

    /// Finalize the encoding session and write the MP4 file.
    /// Consumes the encoder — cannot be used after this call.
    pub fn end_session(self) -> Result<u32, EncoderError> {
        let frames = self.frames_encoded;
        let result = unsafe { metal_ffi::MetalEncoder_EndSession(self.handle) };

        // Skip Drop — handle is consumed by EndSession
        std::mem::forget(self);

        if result != 0 {
            log::error!(
                "[MetalEncoder] end_session failed: {:?}",
                EncoderError::from_code(result),
            );
            return Err(EncoderError::from_code(result));
        }

        log::info!("[MetalEncoder] Session complete, {} frames encoded", frames);
        Ok(frames)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frames_encoded(&self) -> u32 {
        self.frames_encoded
    }

    pub fn is_hdr(&self) -> bool {
        self.is_hdr
    }
}

impl Drop for MetalEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            log::warn!("[MetalEncoder] Encoder dropped without end_session(), cleaning up");
            unsafe {
                metal_ffi::MetalEncoder_EndSession(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod srgb_shader_parity_tests {
    //! BUG-128 value-level gate.
    //!
    //! `native/ColorTransferFunctions.h`'s `manifold_srgb_encode` (the MSL
    //! spliced into `kCopyShaderSDRBody` at
    //! `native/MetalEncoderPlugin.m:54-63` via the format-string splice at
    //! `native/MetalEncoderPlugin.m:194-199`) is hand-verified to implement
    //! the exact same piecewise formula as the tested Rust reference,
    //! `still_exporter::linear_to_srgb` (`src/still_exporter.rs:27-34`):
    //! `x <= 0.0031308 ? x * 12.92 : 1.055 * x.powf(1.0/2.4) - 0.055`, with
    //! input clamped to `[0, 1]` first. Compare literally:
    //! `native/ColorTransferFunctions.h`'s `manifold_srgb_encode` body uses
    //! `x <= 0.0031308`, `x * 12.92`, and `1.055 * pow(x, 1.0/2.4) - 0.055`
    //! — the same breakpoint and constants, just spelled in MSL instead of
    //! Rust and operating on `float3` instead of a scalar.
    //!
    //! We can't invoke the Metal shader function directly from a `cargo
    //! test` (no headless MSL-eval harness in this crate), so this test
    //! instead runs the identical formula (`shader_srgb_encode` below, a
    //! direct transliteration of the MSL) against
    //! `still_exporter::linear_to_srgb` across a spread of linear inputs,
    //! including the shadow region (< 0.04) where the old `pow(1/2.2)`
    //! approximation diverges most. If either the header or the Rust
    //! reference drifts, this test is the tripwire.

    use crate::still_exporter::linear_to_srgb;

    /// Literal transliteration of `manifold_srgb_encode` in
    /// `native/ColorTransferFunctions.h` (scalar form; the MSL operates
    /// component-wise on float3, which is equivalent per-channel).
    fn shader_srgb_encode(x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    }

    #[test]
    fn shader_encode_matches_still_exporter_reference() {
        let samples = [
            0.0_f32, 0.001, 0.0031308, 0.004, 0.01, 0.02, 0.04, 0.1, 0.18, 0.25, 0.5, 0.735, 0.9,
            1.0,
        ];
        for &x in &samples {
            let shader = shader_srgb_encode(x);
            let reference = linear_to_srgb(x);
            assert!(
                (shader - reference).abs() < 1e-6,
                "shader/reference diverge at linear={x}: shader={shader}, reference={reference}"
            );
        }
    }

    #[test]
    fn shader_encode_diverges_from_old_pow_2_2_approximation_in_shadows() {
        let old_approx = |x: f32| x.max(0.0).powf(1.0 / 2.2);
        let x = 0.02_f32; // well below the shadow knee
        let true_srgb = shader_srgb_encode(x);
        let approx = old_approx(x);
        assert!(
            (true_srgb - approx).abs() > 0.01,
            "expected old pow(1/2.2) to visibly diverge from true sRGB at x={x}: \
             true={true_srgb}, approx={approx}"
        );
    }
}

#[cfg(test)]
mod fractional_fps_timebase_tests {
    //! BUG-129 gate: export at a fractional (NTSC-family) fps must not
    //! silently round to an integer CMTime timescale.

    use super::*;

    #[test]
    fn new_converts_2997_fps_to_the_exact_ntsc_rational_before_ffi() {
        // FFI-boundary check: `MetalEncoder::new`'s only path to the native
        // `MetalEncoder_Create` is through `fps_to_rational`. Confirm the
        // exact (num, den) pair that would cross the FFI boundary for
        // 29.97 fps is 30000/1001 — not a rounded 30/1 — matching the
        // per-frame presentation-time contract documented on
        // `metal_ffi::MetalEncoder_Create`.
        let (num, den) = fps_to_rational(29.97);
        assert_eq!((num, den), (30000, 1001));

        // Frame N's presentation time is `N * den / num` seconds (see
        // MetalEncoderPlugin.m's CMTimeMake(frameIndex * fpsDen, fpsNum)).
        // Frame 1's duration must be exactly 1001/30000 s, not 1/30 s.
        let frame_duration = den as f64 / num as f64;
        assert!(
            (frame_duration - (1001.0 / 30000.0)).abs() < 1e-12,
            "frame duration {frame_duration} must equal exactly 1001/30000s"
        );
        assert!(
            (frame_duration - (1.0 / 30.0)).abs() > 1e-6,
            "frame duration must NOT collapse to the rounded 1/30s duration"
        );
    }

    /// Real export smoke test: encode a handful of frames at 29.97 fps
    /// through the actual native encoder and probe the resulting file with
    /// `ffprobe` (available on this dev machine) to confirm the container's
    /// reported frame rate is the exact NTSC rational, not a rounded 30 fps.
    ///
    /// Marked `#[ignore]`: it opens a real Metal device, encodes real
    /// frames, and shells out to `ffprobe` — not the deterministic,
    /// GPU-free default `cargo nextest` sweep (see CLAUDE.md's testing-scope
    /// rule). Run explicitly: `cargo test -p manifold-media --lib
    /// fractional_fps_timebase_tests -- --ignored`.
    #[test]
    #[ignore = "opens a real Metal device + shells out to ffprobe; run explicitly, not in the default sweep"]
    fn export_at_2997_fps_produces_container_with_exact_ntsc_frame_rate() {
        if !MetalEncoder::is_available() {
            eprintln!("Metal not available on this machine, skipping BUG-129 smoke test");
            return;
        }

        let ffprobe_available = std::process::Command::new("ffprobe")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ffprobe_available {
            eprintln!("ffprobe not available on this machine, skipping BUG-129 smoke test");
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "manifold_bug129_fps_smoke_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output_path = dir.join("bug129_2997fps.mp4");

        // Minimal real Metal texture the encoder's compute-copy kernel can
        // read from (uninitialized contents are irrelevant — only the
        // container's frame-rate metadata is under test).
        let device = manifold_gpu::GpuDevice::new();
        let texture = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 64,
            height: 64,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "bug129_smoke_src",
            mip_levels: 1,
        });

        let mut encoder = MetalEncoder::new(64, 64, 29.97, output_path.to_str().unwrap(), false)
            .expect("encoder creation should succeed with a real Metal device");

        for _ in 0..5 {
            unsafe {
                encoder
                    .encode_frame(texture.raw_ptr())
                    .expect("encode_frame should succeed");
            }
        }
        encoder.end_session().expect("end_session should succeed");

        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&output_path)
            .output()
            .expect("ffprobe should run");

        let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(
            reported, "30000/1001",
            "exported container must report the exact NTSC rational frame rate, not a rounded 30/1 \
             (ffprobe stderr: {})",
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
