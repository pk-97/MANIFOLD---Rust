// GPU readback infrastructure.
// Unity equivalent: AsyncGPUReadback.Request() + callback.
// In wgpu there is no native async readback; we use a staging buffer
// that the CPU maps after the GPU fence completes.
//
// Usage patterns:
//   wgpu path (no hal):
//     frame N:   readback.submit(device, encoder, &texture, width, height)
//     frame N+3: if let Some(data) = readback.try_read(device) { process(data) }
//
//   hal shared-memory path (Phase 4.6):
//     frame N:   readback.submit_shared(gpu, &texture, width, height)
//     frame N+1: if let Some(data) = readback.try_read_shared(hal_ctx)
//                { process(data) }

use std::sync::mpsc;

/// A pending or completed GPU readback of an Rgba8Unorm texture.
pub struct ReadbackRequest {
    staging_buffer: Option<wgpu::Buffer>,
    width: u32,
    height: u32,
    // True between submit() and the first successful try_read().
    pending: bool,
    // Receiver for the map_async callback. Created once per readback cycle.
    map_rx: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    /// Shared-memory mapped pointer for direct CPU reads (hal path).
    /// When Some, try_read_shared() reads directly without map_async.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    shared_mapped_ptr: Option<*const u8>,
    /// SharedEvent signal value at submit time. The readback is complete
    /// when hal_ctx.is_frame_done(hal_submit_signal + 1).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_submit_signal: u64,
    /// Native Metal shared-memory buffer for zero-copy readback.
    /// Created via GpuDevice::create_buffer_shared(), contents() gives
    /// a persistent CPU pointer. Blit copy encoded on the native encoder.
    #[cfg(target_os = "macos")]
    native_readback_buf: Option<manifold_gpu::GpuBuffer>,
    /// Persistent CPU pointer into the native shared-memory buffer.
    /// Valid after wait_previous_frame() confirms GPU completion.
    #[cfg(target_os = "macos")]
    native_shared_ptr: Option<*const u8>,
}

// Safety: native_shared_ptr / shared_mapped_ptr point to GPU shared memory
// (Metal MTLStorageMode::Shared). Only read from the content thread after
// GPU completion is confirmed via SharedEvent / wait_previous_frame().
#[cfg(target_os = "macos")]
unsafe impl Send for ReadbackRequest {}

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
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            shared_mapped_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_submit_signal: 0,
            #[cfg(target_os = "macos")]
            native_readback_buf: None,
            #[cfg(target_os = "macos")]
            native_shared_ptr: None,
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
            usage: wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::MAP_READ,
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
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        {
            self.shared_mapped_ptr = None;
        }
    }

    /// Submit a readback via the unified GpuEncoder.
    ///
    /// Native Metal path: creates a shared-memory buffer via manifold_gpu,
    /// encodes a blit copy on the native encoder, stores the mapped pointer.
    /// Read via `try_read_native()` after GPU completion (next frame).
    /// Zero wgpu involvement — no map_async, no Queue::submit.
    ///
    /// wgpu path: falls back to standard submit() with wgpu encoder.
    pub fn submit_via_gpu_encoder(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) {
        // ── Native Metal path: shared-memory readback ─────────────
        #[cfg(target_os = "macos")]
        if let (Some(enc_ptr), Some(dev_ptr)) = (gpu.native_enc, gpu.native_device) {
            let bytes_per_row = align_to_256(width * 4);
            let buffer_size = (bytes_per_row * height) as u64;

            let native_dev = unsafe { &*dev_ptr };
            let shared_buf = native_dev.create_buffer_shared(buffer_size);
            let mapped_ptr = shared_buf.mapped_ptr()
                .expect("shared buffer must have mapped pointer") as *const u8;

            // Extract the source texture for native blit
            let src_gpu = unsafe {
                crate::gpu_encoder::extract_native_texture(texture)
            };
            let enc = unsafe { &mut *enc_ptr };
            enc.copy_texture_to_buffer(
                &src_gpu, &shared_buf, width, height, bytes_per_row,
            );

            self.native_readback_buf = Some(shared_buf);
            self.native_shared_ptr = Some(mapped_ptr);
            self.staging_buffer = None;
            self.width = width;
            self.height = height;
            self.pending = true;
            self.map_rx = None;
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            {
                self.shared_mapped_ptr = None;
            }
            return;
        }

        // ── wgpu fallback ─────────────────────────────────────────
        let encoder = gpu.encoder.as_mut()
            .expect("wgpu encoder required on non-native path");
        self.submit(gpu.device, encoder, texture, width, height);
    }

    /// Try to read pixel data from native shared-memory buffer.
    /// Returns Some(pixels) if the readback was submitted via the native path
    /// and the GPU has completed (guaranteed by wait_previous_frame on the
    /// next frame). Returns None if not pending or not a native readback.
    #[cfg(target_os = "macos")]
    pub fn try_read_native(&mut self) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let ptr = self.native_shared_ptr?;

        // GPU work is complete — wait_previous_frame() in the content pipeline
        // spins on MTLSharedEvent before any new frame encoding. By the time
        // this is called (during the next frame's apply()), the blit is done.
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

        // Clean up
        self.native_readback_buf = None;
        self.native_shared_ptr = None;
        self.map_rx = None;
        self.pending = false;

        Some(out)
    }

    /// Shared-memory readback: creates a shared-memory staging buffer,
    /// encodes copy via hal encoder, reads directly from mapped pointer.
    /// No map_async, no device.poll(), no wgpu submission required.
    ///
    /// Call try_read_shared() on subsequent frames to consume the result.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub fn submit_shared(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) {
        use wgpu::hal::{self, CommandEncoder as _, Device as HalDevice};
        type MetalApi = hal::api::Metal;

        let bytes_per_row = align_to_256(width * 4);
        let buffer_size = (bytes_per_row * height) as u64;

        let hal_ctx = gpu.hal_ctx.unwrap();

        // Create shared-memory staging buffer via hal
        let hal_buf = unsafe {
            hal_ctx
                .device()
                .create_buffer(&hal::BufferDescriptor {
                    label: Some("Readback Staging (shared)"),
                    size: buffer_size,
                    usage: wgpu::wgt::BufferUses::COPY_DST
                        | wgpu::wgt::BufferUses::MAP_READ,
                    memory_flags: hal::MemoryFlags::PREFER_COHERENT,
                })
                .expect("Failed to create hal readback staging buffer")
        };

        // Map to get persistent read pointer
        let mapping = unsafe {
            hal_ctx
                .device()
                .map_buffer(&hal_buf, 0..buffer_size)
                .expect("Failed to map hal readback staging buffer")
        };
        let mapped_ptr = mapping.ptr.as_ptr() as *const u8;

        // Import into wgpu (needed for the wgpu Buffer handle we store)
        let staging = unsafe {
            gpu.device.create_buffer_from_hal::<MetalApi>(
                hal_buf,
                &wgpu::BufferDescriptor {
                    label: Some("Readback Staging (shared)"),
                    size: buffer_size,
                    usage: wgpu::BufferUsages::COPY_DST
                        | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                },
            )
        };

        // Extract hal pointers for the copy
        let hal_tex_ptr = {
            let g = unsafe { texture.as_hal::<MetalApi>() }
                .expect("readback texture not Metal");
            &*g as *const _
        };
        let hal_buf_ptr = {
            let g = unsafe { staging.as_hal::<MetalApi>() }
                .expect("readback staging not Metal");
            &*g as *const _
        };

        // Encode copy via hal encoder
        let (hal_enc, _) =
            unsafe { gpu.hal_encoder_mut() }.unwrap();
        unsafe {
            hal_enc.copy_texture_to_buffer(
                &*hal_tex_ptr,
                wgpu::wgt::TextureUses::COPY_SRC,
                &*hal_buf_ptr,
                std::iter::once(hal::BufferTextureCopy {
                    buffer_layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: None,
                    },
                    texture_base: hal::TextureCopyBase {
                        mip_level: 0,
                        array_layer: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: hal::FormatAspects::COLOR,
                    },
                    size: hal::CopyExtent {
                        width,
                        height,
                        depth: 1,
                    },
                }),
            );
        }

        // Store signal value at submit time — readback is complete when
        // hal_ctx.is_frame_done(hal_submit_signal + 1).
        self.hal_submit_signal = hal_ctx.current_signal_value();
        self.shared_mapped_ptr = Some(mapped_ptr);
        self.staging_buffer = Some(staging);
        self.width = width;
        self.height = height;
        self.pending = true;
        self.map_rx = None;
    }

    /// Try to read pixel data from the staging buffer.
    /// Returns Some(pixels) if the GPU has finished writing, None otherwise.
    /// On success, clears the pending flag and returns tightly-packed RGBA8 rows
    /// (stride = width * 4), matching Unity's NativeArray<byte> layout.
    ///
    /// Unity equivalent: the readback callback receiving request.GetData<byte>().
    pub fn try_read(
        &mut self,
        device: &wgpu::Device,
    ) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        let staging = self.staging_buffer.as_ref()?;

        // Poll the device to make completed GPU work visible.
        let _ = device.poll(wgpu::PollType::Poll);

        // First call after submit(): issue map_async once.
        // Subsequent calls: just check the existing receiver.
        if self.map_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            staging.slice(..).map_async(
                wgpu::MapMode::Read,
                move |result| {
                    let _ = tx.send(result);
                },
            );
            self.map_rx = Some(rx);
            // Poll again to kick off the mapping request.
            let _ = device.poll(wgpu::PollType::Poll);
        }

        // Check if mapping completed.
        let rx = self.map_rx.as_ref()?;
        match rx.try_recv() {
            Ok(Ok(())) => {} // Mapping complete, proceed to read
            Ok(Err(_)) => {
                // Mapping failed — discard this readback.
                self.staging_buffer = None;
                self.map_rx = None;
                self.pending = false;
                return None;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Not ready yet — try again next frame.
                return None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Channel closed unexpectedly — discard.
                self.staging_buffer = None;
                self.map_rx = None;
                self.pending = false;
                return None;
            }
        }

        Some(self.read_staging_data())
    }

    /// Shared-memory readback: check SharedEvent and read directly from
    /// mapped pointer. No map_async, no device.poll().
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub fn try_read_shared(
        &mut self,
        hal_ctx: &crate::hal_context::HalContext,
    ) -> Option<Vec<u8>> {
        if !self.pending {
            return None;
        }
        self.shared_mapped_ptr?;

        // Check if the frame that encoded the copy has completed.
        // We stored the signal value BEFORE signal_frame_completion(),
        // so we wait for signal_value + 1.
        if !hal_ctx.is_frame_done(self.hal_submit_signal + 1) {
            return None;
        }

        // GPU work is complete — read directly from shared memory.
        let ptr = self.shared_mapped_ptr.unwrap();
        let bytes_per_row = align_to_256(self.width * 4) as usize;
        let row_bytes = (self.width * 4) as usize;

        let mut out =
            vec![0u8; row_bytes * self.height as usize];
        for row in 0..self.height as usize {
            let src_start = row * bytes_per_row;
            let dst_start = row * row_bytes;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    ptr.add(src_start),
                    out[dst_start..dst_start + row_bytes]
                        .as_mut_ptr(),
                    row_bytes,
                );
            }
        }

        // Clean up — drop the staging buffer
        self.staging_buffer = None;
        self.shared_mapped_ptr = None;
        self.map_rx = None;
        self.pending = false;

        Some(out)
    }

    /// Read tightly-packed RGBA8 data from the mapped staging buffer.
    /// Removes alignment padding to match Unity NativeArray<byte> layout.
    fn read_staging_data(&mut self) -> Vec<u8> {
        let staging = self.staging_buffer.as_ref().unwrap();
        let bytes_per_row = align_to_256(self.width * 4) as usize;
        let row_bytes = (self.width * 4) as usize;
        let slice = staging.slice(..);
        let mapped = slice.get_mapped_range();

        let mut out =
            vec![0u8; row_bytes * self.height as usize];
        for row in 0..self.height as usize {
            let src_start = row * bytes_per_row;
            let dst_start = row * row_bytes;
            out[dst_start..dst_start + row_bytes]
                .copy_from_slice(
                    &mapped[src_start..src_start + row_bytes],
                );
        }

        drop(mapped);
        staging.unmap();
        self.staging_buffer = None;
        self.map_rx = None;
        self.pending = false;

        out
    }
}

/// Round up to the next multiple of 256 (wgpu copy_texture_to_buffer alignment).
fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}
