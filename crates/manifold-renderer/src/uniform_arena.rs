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
const DEFAULT_CAPACITY: u64 = 64 * 1024; // 64KB — enough for ~256 uniform writes

pub struct UniformArena {
    buffer: wgpu::Buffer,
    cpu_staging: Vec<u8>,
    cursor: u64,
    capacity: u64,
    min_align: u64,
}

impl UniformArena {
    pub fn new(device: &wgpu::Device) -> Self {
        let min_align = device.limits().min_uniform_buffer_offset_alignment as u64;
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
            self.capacity = (self.capacity * 2).max(aligned + bytes.len() as u64 + 4096);
        }

        // Pad staging to aligned offset
        if (aligned as usize) > self.cpu_staging.len() {
            self.cpu_staging.resize(aligned as usize, 0);
        }

        self.cpu_staging.extend_from_slice(bytes);
        self.cursor = aligned + bytes.len() as u64;
        aligned
    }

    /// Flush all staged uniform data to the GPU buffer in a single write.
    /// Call after all push() calls, before encoding any passes.
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
        }

        queue.write_buffer(&self.buffer, 0, &self.cpu_staging);
    }

    /// Reference to the GPU buffer for creating bind groups.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}
