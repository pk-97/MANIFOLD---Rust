// GPU readback infrastructure.
// Unity equivalent: AsyncGPUReadback.Request() + callback.
// In wgpu there is no native async readback; we use a staging buffer
// that the CPU maps after the GPU fence completes.
//
// Usage pattern (matching Unity's two-phase approach):
//   frame N:   readback.submit(device, encoder, &texture, width, height)
//   frame N+3: if let Some(data) = readback.try_read(device)  { process(data) }

/// A pending or completed GPU readback of an Rgba8Unorm texture.
pub struct ReadbackRequest {
    staging_buffer: Option<wgpu::Buffer>,
    width: u32,
    height: u32,
    // True between submit() and the first successful try_read().
    pending: bool,
}

impl ReadbackRequest {
    pub fn new() -> Self {
        Self {
            staging_buffer: None,
            width: 0,
            height: 0,
            pending: false,
        }
    }

    /// Returns true if a readback has been submitted but not yet consumed.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Submit a readback of `texture` (must be Rgba8Unorm, COPY_SRC usage).
    /// Encodes a copy from the texture into a staging buffer.
    /// Call try_read() on subsequent frames to consume the result.
    ///
    /// Unity equivalent: AsyncGPUReadback.Request(rt, 0, TextureFormat.RGBA32, callback)
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
            label: Some("Readback Staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        // wgpu 28: ImageCopyTexture → TexelCopyTextureInfo
        //          ImageCopyBuffer  → TexelCopyBufferInfo
        //          ImageDataLayout  → TexelCopyBufferLayout
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
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        self.staging_buffer = Some(staging);
        self.width = width;
        self.height = height;
        self.pending = true;
    }

    /// Try to read pixel data from the staging buffer.
    /// Returns Some(pixels) if the GPU has finished writing, None otherwise.
    /// On success, clears the pending flag and returns tightly-packed RGBA8 rows
    /// (stride = width * 4), matching Unity's NativeArray<byte> layout.
    ///
    /// Unity equivalent: the readback callback receiving request.GetData<byte>().
    pub fn try_read(&mut self, device: &wgpu::Device) -> Option<Vec<u8>> {
        let staging = self.staging_buffer.as_ref()?;

        // Poll the device to make completed work visible.
        // wgpu 28: MaintainBase renamed to PollType.
        let _ = device.poll(wgpu::PollType::Poll);

        let slice = staging.slice(..);
        // map_async is non-blocking; we check if it's ready immediately.
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        // Poll once more to flush the map request.
        let _ = device.poll(wgpu::PollType::Poll);

        match rx.try_recv() {
            Ok(Ok(())) => {}
            _ => return None, // Not ready yet
        }

        let bytes_per_row = align_to_256(self.width * 4) as usize;
        let row_bytes = (self.width * 4) as usize;
        let mapped = slice.get_mapped_range();

        // Re-pack: remove alignment padding to match Unity NativeArray<byte> layout.
        // Unity: NativeArray<byte>.Copy(nativeData, pixelBuffer, copyLen)
        // where pixelBuffer = new byte[READBACK_WIDTH * READBACK_HEIGHT * 4]
        let mut out = vec![0u8; row_bytes * self.height as usize];
        for row in 0..self.height as usize {
            let src_start = row * bytes_per_row;
            let dst_start = row * row_bytes;
            out[dst_start..dst_start + row_bytes]
                .copy_from_slice(&mapped[src_start..src_start + row_bytes]);
        }

        drop(mapped);
        staging.unmap();
        self.staging_buffer = None;
        self.pending = false;

        Some(out)
    }
}

/// Round up to the next multiple of 256 (wgpu copy_texture_to_buffer alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}
