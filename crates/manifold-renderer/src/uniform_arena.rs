/// Per-frame uniform sub-allocator that batches all uniform writes.
///
/// Uses a shared-memory GpuBuffer (Metal MTLStorageMode::Shared). push() writes
/// directly to the mapped GPU pointer — zero staging, zero API calls.
/// flush() is a no-op since shared memory is CPU+GPU coherent.
const DEFAULT_CAPACITY: u64 = 64 * 1024; // 64KB — enough for ~256 uniform writes

pub struct UniformArena {
    buffer: manifold_gpu::GpuBuffer,
    cursor: u64,
    capacity: u64,
    min_align: u64,
}

impl UniformArena {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let buffer = device.create_buffer_shared(DEFAULT_CAPACITY);
        Self {
            buffer,
            cursor: 0,
            capacity: DEFAULT_CAPACITY,
            min_align: 256, // Metal uniform buffer offset alignment
        }
    }

    /// Reset for a new frame. Call at the start of each frame before any push().
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Push uniform data into the arena. Returns the byte offset into the
    /// buffer for use in `GpuBinding::Buffer { offset }`.
    ///
    /// The offset is aligned to `min_uniform_buffer_offset_alignment`.
    pub fn push<T: bytemuck::Pod>(&mut self, data: &T) -> u64 {
        let bytes = bytemuck::bytes_of(data);
        self.push_bytes(bytes)
    }

    /// Push raw bytes into the arena. Returns the aligned byte offset.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> u64 {
        let aligned = (self.cursor + self.min_align - 1) & !(self.min_align - 1);

        // Grow capacity tracking if needed
        if aligned + bytes.len() as u64 > self.capacity {
            self.capacity =
                (self.capacity * 2).max(aligned + bytes.len() as u64 + 4096);
        }

        // Write directly to shared-memory mapped pointer
        if aligned + bytes.len() as u64 <= self.buffer.size() {
            unsafe {
                self.buffer.write(aligned, bytes);
            }
        }

        self.cursor = aligned + bytes.len() as u64;
        aligned
    }

    /// Flush is a no-op — shared-memory buffer is CPU+GPU coherent.
    /// Recreates the buffer if capacity grew beyond current allocation.
    pub fn flush(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.capacity > self.buffer.size() {
            self.buffer = device.create_buffer_shared(self.capacity);
        }
    }

    /// Reference to the GPU buffer.
    pub fn buffer(&self) -> &manifold_gpu::GpuBuffer {
        &self.buffer
    }
}
