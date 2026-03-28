// GPU readback infrastructure using native Metal shared-memory buffers.
//
// Usage (native Metal path):
//   frame N:   readback.submit(gpu, &texture, width, height)
//   frame N+1: if let Some(data) = readback.try_read() { process(data) }
//
// Uses GpuDevice::create_buffer_shared() for zero-copy CPU reads.
// GPU completion is guaranteed by wait_previous_frame() in the content pipeline
// (spins on MTLSharedEvent before new frame encoding).

/// A pending or completed GPU readback of an Rgba8Unorm texture.
pub struct ReadbackRequest {
    width: u32,
    height: u32,
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
            pending: false,
            native_readback_buf: None,
            native_shared_ptr: None,
        }
    }

    /// Returns true if a readback has been submitted but not yet consumed.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Submit a readback of `texture` (must be Rgba8Unorm, COPY_SRC usage).
    /// Creates a shared-memory buffer, encodes a blit copy on the native encoder.
    /// Call try_read() on the next frame to consume the result.
    pub fn submit(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        texture: &manifold_gpu::GpuTexture,
        width: u32,
        height: u32,
    ) {
        let bytes_per_row = align_to_256(width * 4);
        let buffer_size = (bytes_per_row * height) as u64;

        let shared_buf = gpu.device.create_buffer_shared(buffer_size);
        let mapped_ptr = shared_buf.mapped_ptr()
            .expect("shared buffer must have mapped pointer") as *const u8;

        gpu.native_enc.copy_texture_to_buffer(
            texture, &shared_buf, width, height, bytes_per_row,
        );

        self.native_readback_buf = Some(shared_buf);
        self.native_shared_ptr = Some(mapped_ptr);
        self.width = width;
        self.height = height;
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

        let bytes_per_row = align_to_256(self.width * 4) as usize;
        let row_bytes = (self.width * 4) as usize;
        let mut out = vec![0u8; row_bytes * self.height as usize];

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
