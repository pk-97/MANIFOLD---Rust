//! GPU readback via native Metal shared-memory buffer.
//! The shared-memory buffer is CPU-readable without mapping — after the GPU
//! command buffer completes, bytes are available directly via the mapped pointer.

use manifold_gpu::{GpuBuffer, GpuDevice, GpuTexture};

/// A pending or completed GPU readback of an Rgba8Unorm texture.
pub struct ReadbackRequest {
    /// Shared-memory staging buffer (CPU-readable without mapping).
    staging_buffer: Option<GpuBuffer>,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    pending: bool,
    /// GpuEvent signal value from the frame that submitted this readback.
    /// The readback is safe to read once the event reaches this value.
    pending_signal: u64,
}

impl Default for ReadbackRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadbackRequest {
    pub fn new() -> Self {
        Self {
            staging_buffer: None,
            width: 0,
            height: 0,
            bytes_per_row: 0,
            pending: false,
            pending_signal: 0,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Encode a blit copy from `texture` (Rgba8Unorm) into the shared staging buffer.
    /// Creates the buffer lazily on first use or dimension change.
    pub fn submit(
        &mut self,
        device: &GpuDevice,
        enc: &mut manifold_gpu::GpuEncoder,
        texture: &GpuTexture,
        width: u32,
        height: u32,
        signal_value: u64,
    ) {
        // Metal blit copy requires bytes_per_row aligned to 256.
        let bytes_per_row = align_to_256(width * 4);
        let buffer_size = (bytes_per_row * height) as u64;

        // Create or recreate staging buffer if dimensions changed.
        let needs_new = self.staging_buffer.as_ref()
            .is_none_or(|b| b.size() < buffer_size);
        if needs_new {
            self.staging_buffer = Some(device.create_buffer_shared(buffer_size));
        }

        let staging = self.staging_buffer.as_ref().unwrap();

        // Encode blit copy: texture → shared buffer.
        enc.copy_texture_to_buffer(texture, staging, width, height, bytes_per_row);

        self.width = width;
        self.height = height;
        self.bytes_per_row = bytes_per_row;
        self.pending = true;
        self.pending_signal = signal_value;
    }

    /// Try to read pixel data. Returns `Some(pixels)` if GPU finished, `None` otherwise.
    /// On success, returns tightly-packed RGBA8 rows (stride = width * 4).
    pub fn try_read(&mut self, event: &manifold_gpu::GpuEvent) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }

        // Check if the GPU finished the command buffer that encoded this readback.
        if !event.is_done(self.pending_signal) {
            return None;
        }

        let staging = self.staging_buffer.as_ref()?;
        let ptr = staging.mapped_ptr()?;

        let row_bytes = (self.width * 4) as usize;
        let stride = self.bytes_per_row as usize;

        // Re-pack: remove alignment padding from each row.
        let mut out = vec![0u8; row_bytes * self.height as usize];
        for row in 0..self.height as usize {
            let src_start = row * stride;
            let dst_start = row * row_bytes;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(src_start),
                    out.as_mut_ptr().add(dst_start),
                    row_bytes,
                );
            }
        }

        self.pending = false;
        Some(out)
    }
}

/// Round up to the next multiple of 256 (Metal blit copy alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}
