/// Per-frame uniform sub-allocator that batches all uniform writes into a
/// single `queue.write_buffer()` call.
///
/// Problem: ~120 individual `queue.write_buffer()` calls per frame each create
/// a wgpu staging buffer, measured at 2.1ms in "(wgpu internal) Pending"
/// encoders via Metal Instruments.
///
/// Solution: allocate from a single large GPU buffer. CPU-side, writes go to a
/// staging Vec. At frame start, one `queue.write_buffer()` uploads everything.
/// Each consumer uses `BufferBinding { offset }` to reference its slice.
///
/// When `hal-encoding` is enabled, the buffer uses shared memory (Metal
/// MTLStorageMode::Shared). push() writes directly to the mapped GPU pointer
/// — zero staging, zero API calls. flush() is a no-op.
const DEFAULT_CAPACITY: u64 = 64 * 1024; // 64KB — enough for ~256 uniform writes

pub struct UniformArena {
    buffer: wgpu::Buffer,
    cpu_staging: Vec<u8>,
    cursor: u64,
    capacity: u64,
    min_align: u64,
    /// Persistent mapped pointer for shared-memory buffer (hal path).
    /// When Some, push() writes directly here instead of cpu_staging.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    mapped_ptr: Option<*mut u8>,
    /// Cached hal buffer pointer for hal bind groups.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_buffer_ptr: Option<
        *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
    >,
}

// Safety: mapped_ptr points to GPU shared memory (Metal MTLStorageMode::Shared).
// Only written from the content thread which owns the arena.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for UniformArena {}

impl UniformArena {
    pub fn new(
        device: &wgpu::Device,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off
        let min_align = device.limits().min_uniform_buffer_offset_alignment as u64;

        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            // Create shared-memory buffer via hal
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some("UniformArena"),
                        size: DEFAULT_CAPACITY,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal arena buffer")
            };

            // Map to get persistent pointer
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..DEFAULT_CAPACITY)
                    .expect("Failed to map hal arena buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();

            // Import into wgpu for the wgpu bind group fallback path
            let buffer = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some("UniformArena"),
                        size: DEFAULT_CAPACITY,
                        usage: wgpu::BufferUsages::UNIFORM
                            | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };

            // Cache hal buffer pointer
            let hal_buffer_ptr = {
                let guard = unsafe {
                    buffer
                        .as_hal::<wgpu::hal::api::Metal>()
                        .expect("arena buffer not Metal")
                };
                &*guard
                    as *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer
            };

            return Self {
                buffer,
                cpu_staging: Vec::new(), // unused in hal path
                cursor: 0,
                capacity: DEFAULT_CAPACITY,
                min_align,
                mapped_ptr: Some(mapped_ptr),
                hal_buffer_ptr: Some(hal_buffer_ptr),
            };
        }

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("UniformArena"),
            size: DEFAULT_CAPACITY,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            cpu_staging: Vec::with_capacity(DEFAULT_CAPACITY as usize),
            cursor: 0,
            capacity: DEFAULT_CAPACITY,
            min_align,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            mapped_ptr: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_buffer_ptr: None,
        }
    }

    /// Reset for a new frame. Call at the start of each frame before any push().
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.cpu_staging.clear();
    }

    /// Push uniform data into the arena. Returns the byte offset into the
    /// buffer for use in `BufferBinding { offset }`.
    ///
    /// The offset is aligned to `min_uniform_buffer_offset_alignment`.
    pub fn push<T: bytemuck::Pod>(&mut self, data: &T) -> u64 {
        let bytes = bytemuck::bytes_of(data);
        self.push_bytes(bytes)
    }

    /// Push raw bytes into the arena. Returns the aligned byte offset.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> u64 {
        let aligned = (self.cursor + self.min_align - 1) & !(self.min_align - 1);

        // Grow buffer if needed (double capacity)
        if aligned + bytes.len() as u64 > self.capacity {
            // Can't resize GPU buffer mid-frame, just extend staging
            // and the buffer will be recreated on next flush
            self.capacity =
                (self.capacity * 2).max(aligned + bytes.len() as u64 + 4096);
        }

        // hal path: write directly to shared-memory mapped pointer
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ptr) = self.mapped_ptr
            && aligned + bytes.len() as u64 <= self.buffer.size()
        {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    ptr.add(aligned as usize),
                    bytes.len(),
                );
            }
            self.cursor = aligned + bytes.len() as u64;
            return aligned;
        }

        // wgpu path: write to CPU staging Vec
        if (aligned as usize) > self.cpu_staging.len() {
            self.cpu_staging.resize(aligned as usize, 0);
        }
        self.cpu_staging.extend_from_slice(bytes);
        self.cursor = aligned + bytes.len() as u64;
        aligned
    }

    /// Flush all staged uniform data to the GPU buffer in a single write.
    /// Call after all push() calls, before encoding any passes.
    ///
    /// When using shared-memory (hal path), this is a no-op — data is already
    /// visible to the GPU.
    pub fn flush(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.cpu_staging.is_empty() {
            return;
        }

        // Recreate buffer if capacity grew
        if self.capacity > self.buffer.size() {
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("UniformArena"),
                size: self.capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // Invalidate hal pointer cache since buffer changed
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            {
                self.mapped_ptr = None;
                self.hal_buffer_ptr = None;
            }
        }

        queue.write_buffer(&self.buffer, 0, &self.cpu_staging);
    }

    /// Reference to the GPU buffer for creating bind groups.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Cached hal buffer pointer (if hal-encoding is active and buffer is shared-memory).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub fn hal_buffer_ptr(
        &self,
    ) -> Option<
        *const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer,
    > {
        self.hal_buffer_ptr
    }
}
