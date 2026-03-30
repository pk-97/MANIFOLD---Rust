//! GPU resource types backed by native Metal objects.

use crate::types::*;
use super::*;

// ─── GpuTexture ───────────────────────────────────────────────────────

/// GPU texture backed by a native Metal texture.
pub struct GpuTexture {
    pub(crate) raw: metal::Texture,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

impl GpuTexture {
    /// Wrap an existing metal::Texture (e.g. from IOSurface).
    pub fn from_raw(
        raw: metal::Texture,
        width: u32,
        height: u32,
        depth: u32,
        format: GpuTextureFormat,
    ) -> Self {
        Self { raw, width, height, depth, format }
    }

    /// Raw Metal texture reference.
    pub fn raw(&self) -> &metal::TextureRef {
        &self.raw
    }

    /// Raw Metal texture pointer as `*mut c_void` for FFI interop.
    pub fn raw_ptr(&self) -> *mut std::ffi::c_void {
        use metal::foreign_types::ForeignType;
        self.raw.as_ptr() as *mut std::ffi::c_void
    }
}

// ─── GpuBuffer ────────────────────────────────────────────────────────

/// GPU buffer backed by a native Metal buffer.
pub struct GpuBuffer {
    pub(crate) raw: metal::Buffer,
    pub size: u64,
    /// Persistent mapped pointer for shared-memory buffers.
    /// Some for MTLStorageMode::Shared, None for Private.
    pub(super) mapped_ptr: Option<*mut u8>,
}

unsafe impl Send for GpuBuffer {}
unsafe impl Sync for GpuBuffer {}

impl GpuBuffer {
    /// Wrap an existing metal::Buffer (e.g. extracted from wgpu).
    pub fn from_raw(raw: metal::Buffer, size: u64) -> Self {
        let ptr = raw.contents() as *mut u8;
        Self {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Persistent mapped pointer (shared-memory buffers only).
    /// Direct CPU→GPU writes with zero API overhead.
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        self.mapped_ptr
    }

    /// Write data at offset via memcpy (shared-memory buffers only).
    ///
    /// # Safety
    /// Caller must ensure offset + data.len() <= buffer size,
    /// and no GPU reads overlap this write.
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        let ptr = self.mapped_ptr.expect("write() requires shared-memory buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                ptr.add(offset as usize),
                data.len(),
            );
        }
    }

    /// Raw Metal buffer reference.
    pub fn raw(&self) -> &metal::BufferRef {
        &self.raw
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

// ─── GpuSampler ───────────────────────────────────────────────────────

pub struct GpuSampler {
    pub(crate) raw: metal::SamplerState,
}

unsafe impl Send for GpuSampler {}
unsafe impl Sync for GpuSampler {}

impl GpuSampler {
    /// Raw Metal sampler state reference.
    pub fn raw(&self) -> &metal::SamplerStateRef {
        &self.raw
    }
}

// ─── GpuComputePipeline ───────────────────────────────────────────────

pub struct GpuComputePipeline {
    pub(crate) state: metal::ComputePipelineState,
    pub slot_map: SlotMap,
    pub label: String,
    /// Workgroup size from the shader's @workgroup_size declaration.
    /// Used for dispatch_thread_groups second argument.
    pub workgroup_size: [u32; 3],
    /// Whether this pipeline needs a sizes buffer for runtime-sized arrays.
    pub needs_sizes_buffer: bool,
}

impl Clone for GpuComputePipeline {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            slot_map: self.slot_map.clone(),
            label: self.label.clone(),
            workgroup_size: self.workgroup_size,
            needs_sizes_buffer: self.needs_sizes_buffer,
        }
    }
}

unsafe impl Send for GpuComputePipeline {}
unsafe impl Sync for GpuComputePipeline {}

// ─── GpuRenderPipeline ───────────────────────────────────────────────

pub struct GpuRenderPipeline {
    pub(crate) state: metal::RenderPipelineState,
    pub slot_map: SlotMap,
    pub label: String,
}

impl GpuRenderPipeline {
    /// Raw Metal render pipeline state reference.
    pub fn raw_state(&self) -> &metal::RenderPipelineStateRef {
        &self.state
    }
}

impl Clone for GpuRenderPipeline {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            slot_map: self.slot_map.clone(),
            label: self.label.clone(),
        }
    }
}

unsafe impl Send for GpuRenderPipeline {}
unsafe impl Sync for GpuRenderPipeline {}

// ─── GpuEvent ─────────────────────────────────────────────────────────

/// GPU↔CPU synchronization via MTLSharedEvent.
/// Near-zero overhead polling (direct counter read).
pub struct GpuEvent {
    raw: metal::SharedEvent,
    pub(crate) counter: std::cell::Cell<u64>,
}

unsafe impl Send for GpuEvent {}
unsafe impl Sync for GpuEvent {}

impl GpuEvent {
    /// Create a new GpuEvent from a shared event.
    pub(crate) fn new(raw: metal::SharedEvent) -> Self {
        Self {
            raw,
            counter: std::cell::Cell::new(0),
        }
    }

    /// Check if the GPU has completed work signaled at `value`.
    pub fn is_done(&self, value: u64) -> bool {
        self.raw.signaled_value() >= value
    }

    /// Current signal counter (store after signal_event).
    pub fn current_value(&self) -> u64 {
        self.counter.get()
    }

    /// Read the GPU-side signaled value directly.
    pub fn signaled_value(&self) -> u64 {
        self.raw.signaled_value()
    }

    /// Block the calling thread until the GPU has signaled `value`, with a timeout.
    /// Returns `true` if the event was signaled, `false` if timed out.
    pub fn wait_until_done_timeout(&self, value: u64, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(timeout_ms);
        while !self.is_done(value) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// Block the calling thread until the GPU has signaled `value`.
    /// Times out after 5 seconds and logs an error if the GPU appears hung.
    pub fn wait_until_done(&self, value: u64) {
        if !self.wait_until_done_timeout(value, 5000) {
            log::error!(
                "GpuEvent::wait_until_done timed out after 5s \
                 (waiting for value={}, signaled={})",
                value,
                self.raw.signaled_value(),
            );
        }
    }

    /// Raw Metal shared event reference.
    pub fn raw(&self) -> &metal::SharedEventRef {
        &self.raw
    }
}

// ─── GpuHeap ──────────────────────────────────────────────────────────

/// GPU heap backed by a native MTLHeap.
/// Sub-allocates textures without per-allocation kernel calls.
pub struct GpuHeap {
    heap: metal::Heap,
}

unsafe impl Send for GpuHeap {}
unsafe impl Sync for GpuHeap {}

impl GpuHeap {
    /// Create a new GpuHeap wrapping a Metal heap.
    pub(crate) fn new(heap: metal::Heap) -> Self {
        Self { heap }
    }

    /// Sub-allocate a texture from this heap.
    /// Returns `None` if the heap doesn't have enough space.
    pub fn new_texture(&self, desc: &GpuTextureDesc) -> Option<GpuTexture> {
        let mtl_desc = GpuDevice::build_mtl_texture_desc(desc);
        // Override storage mode to match heap's storage mode.
        mtl_desc.set_storage_mode(self.heap.storage_mode());
        self.heap.new_texture(&mtl_desc).map(|raw| GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        })
    }

    /// Total heap size in bytes.
    pub fn size(&self) -> u64 {
        self.heap.size()
    }

    /// Currently used heap memory in bytes.
    pub fn used_size(&self) -> u64 {
        self.heap.used_size()
    }

    /// Maximum available contiguous allocation size with given alignment.
    pub fn max_available_size(&self, alignment: u64) -> u64 {
        self.heap.max_available_size_with_alignment(alignment)
    }
}
