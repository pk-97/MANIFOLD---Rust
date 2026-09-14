// GPU readback infrastructure using native Metal shared-memory buffers.
//
// Usage (native Metal path):
//   frame N:   readback.submit(gpu, &texture, width, height)
//   frame N+1: if let Some(data) = readback.try_read() { process(data) }
//
// Uses GpuDevice::create_buffer_shared() for zero-copy CPU reads.
// GPU completion is guaranteed by wait_previous_frame() in the content pipeline
// (spins on MTLSharedEvent before new frame encoding).

/// A pending or completed GPU readback of a texture.
/// Supports both Rgba8Unorm (4 bpp) and Rgba16Float (8 bpp) sources.
/// Always returns tightly-packed RGBA8 data from try_read().
pub struct ReadbackRequest {
    width: u32,
    height: u32,
    bpp: u32,
    pending: bool,
    /// Native Metal shared-memory buffer for zero-copy readback.
    native_readback_buf: Option<manifold_gpu::GpuBuffer>,
    /// Persistent CPU pointer into the native shared-memory buffer.
    native_shared_ptr: Option<*const u8>,
    /// Allocated byte capacity of `native_readback_buf`. The buffer is kept
    /// after a consume and reused by later submissions at the same or a
    /// smaller size.
    buffer_capacity: u64,
}

// Safety: native_shared_ptr points to GPU shared memory
// (Metal MTLStorageMode::Shared). Only read after GPU completion.
unsafe impl Send for ReadbackRequest {}

impl Default for ReadbackRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;
    use crate::{gpu_encoder::GpuEncoder, render_target::RenderTarget};
    use manifold_gpu::GpuTextureFormat;

    #[test]
    fn gpu_readback_reuses_gpu_and_cpu_storage_after_consume_and_cancel() {
        let device = crate::test_device();
        let target = RenderTarget::new(
            &device,
            5,
            3,
            GpuTextureFormat::Rgba16Float,
            "readback-reuse",
        );
        let mut request = ReadbackRequest::new();
        let mut pixels = Vec::with_capacity(5 * 3 * 4);
        let cpu_ptr = pixels.as_ptr();
        let mut gpu_ptr = None;
        for frame in 0..3 {
            let mut enc = device.create_encoder("readback-reuse");
            let mut gpu = GpuEncoder::new(&mut enc, &device);
            gpu.clear_texture(&target.texture, 0.25, 0.5, 0.75, 1.0);
            request.submit(&mut gpu, &target.texture, 5, 3);
            enc.commit_and_wait_completed();
            if frame == 0 {
                gpu_ptr = request.native_shared_ptr;
            }
            assert_eq!(request.native_shared_ptr, gpu_ptr);
            assert_eq!(request.buffer_capacity, 256 * 3);
            if frame == 1 {
                request.cancel();
                assert!(!request.is_pending());
            } else {
                assert!(request.try_read_into(&mut pixels));
                assert_eq!(pixels.as_ptr(), cpu_ptr);
                assert_eq!(pixels.len(), 5 * 3 * 4);
                assert_eq!(&pixels[..4], &[64, 128, 191, 255]);
            }
        }
    }
}

impl ReadbackRequest {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            bpp: 4,
            pending: false,
            native_readback_buf: None,
            native_shared_ptr: None,
            buffer_capacity: 0,
        }
    }

    /// Returns true if a readback has been submitted but not yet consumed.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Submit a readback of `texture`. Accepts any format — try_read()
    /// always returns tightly-packed RGBA8 data regardless of source format.
    /// Allocates or grows a shared-memory buffer, encodes a blit copy on the
    /// native encoder, and keeps that buffer for later submissions. Call
    /// try_read() on the next frame to consume the result.
    pub fn submit(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        texture: &manifold_gpu::GpuTexture,
        width: u32,
        height: u32,
    ) {
        // A second submission would overwrite a shared buffer that the GPU
        // may still be reading. Callers must wait for/consume the pending
        // request before submitting again.
        if self.pending {
            debug_assert!(false, "readback submitted while another request is pending");
            return;
        }
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = align_to_256(width * bpp);
        let buffer_size = (bytes_per_row * height) as u64;

        if self.buffer_capacity < buffer_size {
            let shared_buf = gpu.device.create_buffer_shared(buffer_size);
            let mapped_ptr = shared_buf
                .mapped_ptr()
                .expect("shared buffer must have mapped pointer")
                as *const u8;
            self.native_readback_buf = Some(shared_buf);
            self.native_shared_ptr = Some(mapped_ptr);
            self.buffer_capacity = buffer_size;
        }

        let shared_buf = self
            .native_readback_buf
            .as_ref()
            .expect("readback buffer must exist after capacity check");

        gpu.native_enc
            .copy_texture_to_buffer(texture, shared_buf, width, height, bytes_per_row);

        self.width = width;
        self.height = height;
        self.bpp = bpp;
        self.pending = true;
    }

    /// Try to read pixel data from the shared-memory buffer.
    /// Returns Some(pixels) if a readback is pending. The GPU work is guaranteed
    /// complete by wait_previous_frame() in the content pipeline (called before
    /// any new frame encoding).
    ///
    /// Returns tightly-packed RGBA8 rows (stride = width * 4).
    pub fn try_read(&mut self) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let row_bytes = (self.width * 4) as usize;
        let mut out = vec![0u8; row_bytes * self.height as usize];
        self.try_read_into(&mut out).then_some(out)
    }

    /// Try to read into a caller-owned RGBA8 buffer, reusing its allocation.
    /// Returns `true` when a pending request was consumed. The shared Metal
    /// buffer remains owned by this request and is reusable by `submit()`.
    pub fn try_read_into(&mut self, out: &mut Vec<u8>) -> bool {
        if !self.pending {
            return false;
        }
        let Some(ptr) = self.native_shared_ptr else {
            return false;
        };

        let bytes_per_row = align_to_256(self.width * self.bpp) as usize;
        let row_bytes = (self.width * 4) as usize;
        out.resize(row_bytes * self.height as usize, 0);

        if self.bpp == 8 {
            // Rgba16Float: read 4× f16 channels, convert to u8.
            for row in 0..self.height as usize {
                let src_row = row * bytes_per_row;
                let dst_row = row * row_bytes;
                for col in 0..self.width as usize {
                    let src_px = src_row + col * 8;
                    let dst_px = dst_row + col * 4;
                    for ch in 0..4 {
                        let bits = unsafe {
                            let lo = *ptr.add(src_px + ch * 2);
                            let hi = *ptr.add(src_px + ch * 2 + 1);
                            u16::from_le_bytes([lo, hi])
                        };
                        let f = f16_to_f32(bits);
                        out[dst_px + ch] = (f * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        } else {
            // Rgba8Unorm: direct row copy.
            for row in 0..self.height as usize {
                let src_start = row * bytes_per_row;
                let dst_start = row * row_bytes;
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr.add(src_start),
                        out[dst_start..dst_start + row_bytes].as_mut_ptr(),
                        row_bytes,
                    );
                }
            }
        }

        self.pending = false;
        true
    }

    /// Read the raw, tightly-packed *source-format* bytes from the shared buffer
    /// without converting to RGBA8. For an Rgba16Float source this is
    /// `width * height * 8` bytes of little-endian f16 channels (stride =
    /// width * bytes_per_pixel). Used by the still-image export, which applies
    /// its own colour pipeline (highlight rolloff + sRGB) in float before
    /// quantizing — `try_read`'s direct f16→u8 quantize would wreck shadow
    /// precision. The `try_read` (RGBA8, raw/linear) path used by the DNN
    /// consumers is unchanged.
    pub fn try_read_packed(&mut self) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let row_bytes = (self.width * self.bpp) as usize;
        let mut out = vec![0u8; row_bytes * self.height as usize];
        self.try_read_packed_into(&mut out).then_some(out)
    }

    /// Packed-source equivalent of [`Self::try_read_into`].
    pub fn try_read_packed_into(&mut self, out: &mut Vec<u8>) -> bool {
        if !self.pending {
            return false;
        }
        let Some(ptr) = self.native_shared_ptr else {
            return false;
        };

        let bytes_per_row = align_to_256(self.width * self.bpp) as usize;
        let row_bytes = (self.width * self.bpp) as usize;
        out.resize(row_bytes * self.height as usize, 0);
        for row in 0..self.height as usize {
            let src_start = row * bytes_per_row;
            let dst_start = row * row_bytes;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(src_start),
                    out[dst_start..dst_start + row_bytes].as_mut_ptr(),
                    row_bytes,
                );
            }
        }

        self.pending = false;
        true
    }

    /// Cancel a pending request after the caller has reached a frame boundary
    /// where the previous GPU command buffer is complete. The shared buffer is
    /// released so a later submit cannot overwrite in-flight GPU work. An
    /// already-idle request keeps its allocation for reuse.
    pub fn cancel(&mut self) {
        if !self.pending {
            return;
        }
        self.pending = false;
        self.native_readback_buf = None;
        self.native_shared_ptr = None;
        self.buffer_capacity = 0;
    }
}

/// Round up to the next multiple of 256 (Metal texture copy alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}

/// Convert IEEE 754 half-precision (f16) bits to f32.
///
/// `pub`: also used by `manifold-app`'s clip-thumb disk worker to convert a
/// `try_read_packed()` Rgba16Float atlas readback off the content thread
/// (BUG-035 — the scalar per-pixel conversion this function drives is exactly
/// the CPU work that must never run on the content thread's hot path).
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;

    if exp == 0 {
        if frac == 0 {
            f32::from_bits(sign << 31) // ±0
        } else {
            // Subnormal: 2^-14 × (frac / 1024)
            let val = (frac as f32) * (1.0 / 1024.0) * (1.0 / 16384.0); // 2^-14 = 1/16384
            if sign == 1 { -val } else { val }
        }
    } else if exp == 31 {
        f32::from_bits((sign << 31) | (0xff << 23) | (frac << 13)) // inf or NaN
    } else {
        f32::from_bits((sign << 31) | ((exp + 112) << 23) | (frac << 13))
    }
}
