//! Non-blocking GPU texture readback for native CV/DNN processing.
//!
//! Matches Unity's `AsyncGPUReadback.Request()` pattern:
//! - Frame N: submit readback (copy texture → staging buffer + map_async)
//! - Frame N+1: poll — if ready, read bytes and return them; if not, skip
//!
//! This gives 1-frame latency, matching Unity's 1-3 frame latency from
//! AsyncGPUReadback. The main thread never stalls.

/// A pending GPU readback request.
///
/// Created by `submit_readback()`, polled by `try_read()`.
/// The staging buffer is reusable after `try_read()` returns Some or after drop.
pub struct ReadbackRequest {
    staging_buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    /// True once map_async has been called and we're waiting for completion.
    pending: bool,
    /// True once the mapping is complete and data is ready to read.
    ready: bool,
}

impl ReadbackRequest {
    /// Create a new readback request (staging buffer only, no GPU work yet).
    ///
    /// Call `submit()` to actually kick off the copy + map.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        // wgpu requires bytes_per_row to be aligned to 256 bytes (COPY_BYTES_PER_ROW_ALIGNMENT)
        let unpadded_bytes_per_row = width * 4; // RGBA8 = 4 bytes/pixel
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bytes_per_row = (unpadded_bytes_per_row + align - 1) / align * align;

        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback_staging"),
            size: (bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            staging_buffer,
            width,
            height,
            bytes_per_row,
            pending: false,
            ready: false,
        }
    }

    /// Submit the readback: copy texture to staging buffer and request mapping.
    ///
    /// Call this once per readback cycle (throttled by the effect's readback interval).
    /// The encoder must be submitted to the queue after this call.
    pub fn submit(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        source_texture: &wgpu::Texture,
    ) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        // Request async mapping. The callback just sets an atomic flag.
        self.staging_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |_result| {
                // Mapping completed (success or failure).
                // We check via buffer.slice(..).get_mapped_range() on the next poll.
            });

        self.pending = true;
        self.ready = false;
    }

    /// Poll the readback. Call this once per frame after `device.poll(Maintain::Poll)`.
    ///
    /// Returns `Some(data)` with RGBA8 pixel bytes if the readback is complete.
    /// Returns `None` if still pending or no readback was submitted.
    ///
    /// The returned Vec has row-major RGBA8 pixels (width * height * 4 bytes),
    /// with row padding stripped.
    pub fn try_read(&mut self) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }

        // Try to get the mapped range. If the mapping isn't done yet, this will panic
        // in debug or return an error — we catch it by checking the buffer state.
        let slice = self.staging_buffer.slice(..);

        // Attempt to read. If the buffer isn't mapped yet, get_mapped_range will panic.
        // Instead, we use a simpler approach: after poll(Maintain::Poll), if the buffer
        // is mapped, get_mapped_range succeeds.
        let mapped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            slice.get_mapped_range()
        }));

        match mapped {
            Ok(data) => {
                // Strip row padding: copy only width*4 bytes per row
                let unpadded_bytes_per_row = self.width as usize * 4;
                let padded_bytes_per_row = self.bytes_per_row as usize;
                let mut pixels = Vec::with_capacity(unpadded_bytes_per_row * self.height as usize);

                for row in 0..self.height as usize {
                    let start = row * padded_bytes_per_row;
                    let end = start + unpadded_bytes_per_row;
                    pixels.extend_from_slice(&data[start..end]);
                }

                drop(data);
                self.staging_buffer.unmap();
                self.pending = false;
                self.ready = false;

                Some(pixels)
            }
            Err(_) => {
                // Buffer not yet mapped — try again next frame
                None
            }
        }
    }

    /// Whether a readback is currently in-flight.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Dimensions of the readback target.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
