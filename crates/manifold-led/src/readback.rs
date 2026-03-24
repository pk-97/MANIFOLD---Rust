//! GPU readback via staging buffer.
//! Replica of manifold-renderer's gpu_readback.rs pattern, kept independent
//! to avoid cross-crate dependency. Small enough to own.

use std::sync::mpsc;

/// A pending or completed GPU readback of an Rgba8Unorm texture.
pub struct ReadbackRequest {
    staging_buffer: Option<wgpu::Buffer>,
    width: u32,
    height: u32,
    pending: bool,
    map_rx: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
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
            pending: false,
            map_rx: None,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Encode a copy from `texture` (Rgba8Unorm, COPY_SRC) into a staging buffer.
    pub fn submit(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) {
        let bytes_per_row = align_to_256(width * 4);
        let buffer_size = (bytes_per_row * height) as u64;

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("LED Readback Staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        self.staging_buffer = Some(staging);
        self.width = width;
        self.height = height;
        self.pending = true;
        self.map_rx = None;
    }

    /// Try to read pixel data. Returns `Some(pixels)` if GPU finished, `None` otherwise.
    /// On success, returns tightly-packed RGBA8 rows (stride = width * 4).
    pub fn try_read(&mut self, device: &wgpu::Device) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let staging = self.staging_buffer.as_ref()?;

        let _ = device.poll(wgpu::PollType::Poll);

        // First call after submit: issue map_async once.
        if self.map_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            staging
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            self.map_rx = Some(rx);
            let _ = device.poll(wgpu::PollType::Poll);
        }

        let rx = self.map_rx.as_ref()?;
        match rx.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.reset();
                return None;
            }
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.reset();
                return None;
            }
        }

        let bytes_per_row = align_to_256(self.width * 4) as usize;
        let row_bytes = (self.width * 4) as usize;
        let slice = staging.slice(..);
        let mapped = slice.get_mapped_range();

        // Re-pack: remove alignment padding.
        let mut out = vec![0u8; row_bytes * self.height as usize];
        for row in 0..self.height as usize {
            let src_start = row * bytes_per_row;
            let dst_start = row * row_bytes;
            out[dst_start..dst_start + row_bytes]
                .copy_from_slice(&mapped[src_start..src_start + row_bytes]);
        }

        drop(mapped);
        staging.unmap();
        self.reset();

        Some(out)
    }

    fn reset(&mut self) {
        self.staging_buffer = None;
        self.map_rx = None;
        self.pending = false;
    }
}

/// Round up to the next multiple of 256 (wgpu copy alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}
