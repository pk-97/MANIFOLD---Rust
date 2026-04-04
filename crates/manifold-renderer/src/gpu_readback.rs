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
}

// Safety: native_shared_ptr points to GPU shared memory
// (Metal MTLStorageMode::Shared). Only read after GPU completion.
unsafe impl Send for ReadbackRequest {}

impl Default for ReadbackRequest {
    fn default() -> Self {
        Self::new()
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
        }
    }

    /// Returns true if a readback has been submitted but not yet consumed.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Submit a readback of `texture`. Accepts any format — try_read()
    /// always returns tightly-packed RGBA8 data regardless of source format.
    /// Creates a shared-memory buffer, encodes a blit copy on the native encoder.
    /// Call try_read() on the next frame to consume the result.
    pub fn submit(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        texture: &manifold_gpu::GpuTexture,
        width: u32,
        height: u32,
    ) {
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = align_to_256(width * bpp);
        let buffer_size = (bytes_per_row * height) as u64;

        let shared_buf = gpu.device.create_buffer_shared(buffer_size);
        let mapped_ptr = shared_buf
            .mapped_ptr()
            .expect("shared buffer must have mapped pointer") as *const u8;

        gpu.native_enc
            .copy_texture_to_buffer(texture, &shared_buf, width, height, bytes_per_row);

        self.native_readback_buf = Some(shared_buf);
        self.native_shared_ptr = Some(mapped_ptr);
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
        let ptr = self.native_shared_ptr?;

        let bytes_per_row = align_to_256(self.width * self.bpp) as usize;
        let row_bytes = (self.width * 4) as usize;
        let mut out = vec![0u8; row_bytes * self.height as usize];

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

        self.native_readback_buf = None;
        self.native_shared_ptr = None;
        self.pending = false;

        Some(out)
    }
}

/// Round up to the next multiple of 256 (Metal texture copy alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}

/// Convert IEEE 754 half-precision (f16) bits to f32.
fn f16_to_f32(bits: u16) -> f32 {
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
