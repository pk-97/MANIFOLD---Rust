//! GPU resource types backed by native Metal objects.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBuffer, MTLComputePipelineState, MTLDepthStencilState, MTLHeap, MTLRenderPipelineState,
    MTLSamplerState, MTLSharedEvent, MTLSharedEventListener, MTLTexture,
};

use super::*;
use crate::types::*;

// ─── GpuTexture ───────────────────────────────────────────────────────

/// GPU texture backed by a native Metal texture.
///
/// `Clone` is cheap — a `Retained` clone is one atomic retain on the
/// underlying `MTLTexture`, with no GPU allocation. Used by the
/// node-graph chain runtime to install an upstream input texture into a
/// `Source` node's output slot without a `copy_texture_to_texture`.
#[derive(Clone)]
pub struct GpuTexture {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLTexture>>,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

impl GpuTexture {
    /// Wrap an existing Metal texture (e.g. from IOSurface).
    pub fn from_raw(
        raw: Retained<ProtocolObject<dyn MTLTexture>>,
        width: u32,
        height: u32,
        depth: u32,
        format: GpuTextureFormat,
    ) -> Self {
        Self {
            raw,
            width,
            height,
            depth,
            format,
        }
    }

    /// Raw Metal texture reference.
    pub fn raw(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.raw
    }

    /// Raw Metal texture pointer as `*mut c_void` for FFI interop.
    pub fn raw_ptr(&self) -> *mut std::ffi::c_void {
        Retained::as_ptr(&self.raw) as *mut std::ffi::c_void
    }

    /// Whether this texture may be bound to a shader stage (sampled).
    ///
    /// A render-target-only texture — created without `ShaderRead` usage —
    /// has no shader-read view, and binding it via `setVertexTexture` /
    /// `setFragmentTexture` makes AGX nil-deref that missing view (a hard
    /// `EXC_BAD_ACCESS` at offset `0x78`, not a recoverable error). The
    /// thumbnail-capture paths sample arbitrary producer outputs, so they
    /// must check this before binding rather than trust the producer.
    pub fn is_shader_readable(&self) -> bool {
        unsafe { self.raw.usage() }.contains(objc2_metal::MTLTextureUsage::ShaderRead)
    }

    /// Whether two handles refer to the SAME underlying Metal texture object
    /// (identity, not value/size equality) — the `GpuBuffer::ptr_eq` idiom,
    /// extended to textures. A mip-level view created by
    /// [`mip_level_view`](Self::mip_level_view) is a DIFFERENT Metal object
    /// backed by the same storage, so this does NOT consider a texture and
    /// its own mip view equal.
    pub fn ptr_eq(&self, other: &GpuTexture) -> bool {
        (Retained::as_ptr(&self.raw) as *const ()) == (Retained::as_ptr(&other.raw) as *const ())
    }

    /// Raw pointer identity as a plain integer — a lighter-weight sibling of
    /// [`ptr_eq`](Self::ptr_eq) for callers that want to cache the identity
    /// (e.g. "did the wired texture change since last frame") rather than
    /// compare two live references directly.
    pub fn identity_key(&self) -> usize {
        Retained::as_ptr(&self.raw) as *const () as usize
    }

    /// Number of mip levels on the underlying Metal texture. `1` for the
    /// common flat allocation; mip-chained material-map slots
    /// (IMPORT_FIDELITY F-P6) report their full chain length. Callers use
    /// this to gate `GpuEncoder::generate_mipmaps` — generating on a
    /// single-level texture is a pointless blit pass.
    pub fn mip_level_count(&self) -> u32 {
        self.raw.mipmapLevelCount() as u32
    }

    /// Create a VIEW of a single mip level of `self`, sharing the same
    /// underlying storage (no new GPU allocation) but presented to the
    /// shader as its own complete, single-mip `GpuTexture` of the given
    /// level's dimensions. Used to bind one destination mip of a
    /// mip-mapped texture to a compute pass that writes per-level content
    /// the hardware `GpuEncoder::generate_mipmaps` blit (box filter) cannot
    /// express — e.g. GGX-importance-convolved specular prefiltering
    /// (IMPORT_FIDELITY_DESIGN.md D2/F-P1's prefiltered specular chain).
    ///
    /// `self` must have been created with `mip_levels > level` and with
    /// `SHADER_WRITE` usage (the view inherits the parent's usage bits —
    /// Metal texture views cannot add usage the parent lacks).
    pub fn mip_level_view(&self, level: u32, mip_width: u32, mip_height: u32) -> GpuTexture {
        let mtl_format = super::format::to_mtl_pixel_format(self.format);
        let view = unsafe {
            self.raw.newTextureViewWithPixelFormat_textureType_levels_slices(
                mtl_format,
                objc2_metal::MTLTextureType::Type2D,
                objc2_foundation::NSRange {
                    location: level as usize,
                    length: 1,
                },
                objc2_foundation::NSRange {
                    location: 0,
                    length: 1,
                },
            )
        }
        .expect("newTextureViewWithPixelFormat_textureType_levels_slices failed");
        GpuTexture {
            raw: view,
            width: mip_width,
            height: mip_height,
            depth: 1,
            format: self.format,
        }
    }
}

// ─── GpuBuffer ────────────────────────────────────────────────────────

/// GPU buffer backed by a native Metal buffer.
pub struct GpuBuffer {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub size: u64,
    /// Persistent mapped pointer for shared-memory buffers.
    /// Some for MTLStorageMode::Shared, None for Private.
    pub(super) mapped_ptr: Option<*mut u8>,
}

unsafe impl Send for GpuBuffer {}
unsafe impl Sync for GpuBuffer {}

impl GpuBuffer {
    /// Wrap an existing Metal buffer.
    pub fn from_raw(raw: Retained<ProtocolObject<dyn MTLBuffer>>, size: u64) -> Self {
        let ptr = unsafe { raw.contents() };
        let ptr = ptr.as_ptr() as *mut u8;
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
        let ptr = self
            .mapped_ptr
            .expect("write() requires shared-memory buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset as usize), data.len());
        }
    }

    /// Zero every byte of a shared-memory buffer in place (CPU memset, no
    /// allocation). Metal's `newBufferWithLength` does NOT zero-initialize, so
    /// any buffer whose first GPU use READS it (an atomic accumulator cleared
    /// after read, a scratch buffer summed into) must be zeroed once at
    /// allocation or it reads uninitialized VRAM. Shared-only — private buffers
    /// have no mapped pointer; zero those via the encoder's `clear_buffer` blit.
    ///
    /// # Panics
    /// If the buffer is not shared-memory (no mapped pointer).
    pub fn zero_fill(&self) {
        let ptr = self
            .mapped_ptr
            .expect("zero_fill() requires a shared-memory buffer");
        // SAFETY: `ptr` is the buffer's mapped base, valid for `size` bytes by
        // construction; no GPU work has touched the buffer yet (called at
        // allocation), so there is no concurrent access.
        unsafe {
            std::ptr::write_bytes(ptr, 0, self.size as usize);
        }
    }

    /// Raw Metal buffer reference.
    pub fn raw(&self) -> &ProtocolObject<dyn MTLBuffer> {
        &self.raw
    }

    /// Whether two handles refer to the SAME underlying Metal buffer object
    /// (identity, not value/size equality). Mirrors the encoder's internal
    /// `buffer_identity`. Used to detect aliased in/out wires — e.g. an
    /// in-place particle feedback loop where `in` and `out` resolve to one
    /// pre-bound buffer, so a frame-delay copy between them would be a no-op.
    pub fn ptr_eq(&self, other: &GpuBuffer) -> bool {
        (&*self.raw as *const _ as *const ()) == (&*other.raw as *const _ as *const ())
    }

    /// Raw pointer identity as a plain integer — the [`GpuTexture::identity_key`]
    /// sibling. Lets a caller cache "is this the SAME physical buffer as last
    /// frame" without holding a borrowed `&GpuBuffer` across frames
    /// (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 — a slot's pool-recycled
    /// buffer must be treated as changed content even when the CPU-side cache
    /// that feeds it didn't change).
    pub fn identity_key(&self) -> usize {
        (&*self.raw as *const _ as *const ()) as usize
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// RT-T1-B: this buffer's GPU virtual address (`MTLBuffer::gpuAddress`)
    /// — the bindless-read mechanism the RT trace kernel uses to fetch a
    /// per-object vertex-normal buffer at ray-hit time, indexed by
    /// `hit.instance_id` (see `raytrace::RtNormalSource`). Valid to read
    /// from a kernel WITHOUT a separate `useResource` call as long as the
    /// SAME command encoder also binds an acceleration structure this
    /// buffer's geometry feeds (`setAccelerationStructure_atBufferIndex`
    /// transitively makes every BLAS's referenced vertex/index buffers
    /// resident) — true for every `dispatch_shadow_rays` call, the only
    /// caller of this today.
    pub fn gpu_address(&self) -> u64 {
        self.raw.gpuAddress()
    }
}

// ─── GpuSampler ───────────────────────────────────────────────────────

/// `Clone` is cheap — a `Retained` clone is one atomic retain on the
/// underlying `MTLSamplerState`, no GPU allocation (same rationale as
/// `GpuTexture`'s `Clone` above). Lets a caller extract an owned sampler
/// handle before a subsequent `&mut self` call that would otherwise
/// conflict with holding a borrow of the field it came from.
#[derive(Clone)]
pub struct GpuSampler {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

unsafe impl Send for GpuSampler {}
unsafe impl Sync for GpuSampler {}

impl GpuSampler {
    /// Raw Metal sampler state reference.
    pub fn raw(&self) -> &ProtocolObject<dyn MTLSamplerState> {
        &self.raw
    }
}

// ─── GpuComputePipeline ───────────────────────────────────────────────

pub struct GpuComputePipeline {
    pub(crate) state: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
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
    pub(crate) state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub slot_map: SlotMap,
    pub label: String,
    /// Whether this pipeline needs a naga sizes buffer bound (fragment or
    /// vertex shader uses `arrayLength()` on a runtime-sized storage array).
    pub needs_sizes_buffer: bool,
}

impl GpuRenderPipeline {
    /// Raw Metal render pipeline state reference.
    pub fn raw_state(&self) -> &ProtocolObject<dyn MTLRenderPipelineState> {
        &self.state
    }
}

impl Clone for GpuRenderPipeline {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            slot_map: self.slot_map.clone(),
            label: self.label.clone(),
            needs_sizes_buffer: self.needs_sizes_buffer,
        }
    }
}

unsafe impl Send for GpuRenderPipeline {}
unsafe impl Sync for GpuRenderPipeline {}

// ─── GpuDepthStencilState ────────────────────────────────────────────

/// Compiled depth-stencil state object (MTLDepthStencilState).
/// Created once, set on the render encoder for depth-tested draws.
pub struct GpuDepthStencilState {
    pub(crate) raw: Retained<ProtocolObject<dyn MTLDepthStencilState>>,
}

unsafe impl Send for GpuDepthStencilState {}
unsafe impl Sync for GpuDepthStencilState {}

impl GpuDepthStencilState {
    /// Raw Metal depth-stencil state reference.
    pub fn raw(&self) -> &ProtocolObject<dyn MTLDepthStencilState> {
        &self.raw
    }
}

// ─── GpuEvent ─────────────────────────────────────────────────────────

/// GPU↔CPU synchronization via MTLSharedEvent.
/// Near-zero overhead polling (direct counter read).
pub struct GpuEvent {
    raw: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    pub(crate) counter: std::cell::Cell<u64>,
}

unsafe impl Send for GpuEvent {}
unsafe impl Sync for GpuEvent {}

impl GpuEvent {
    /// Create a new GpuEvent from a shared event.
    pub(crate) fn new(raw: Retained<ProtocolObject<dyn MTLSharedEvent>>) -> Self {
        Self {
            raw,
            counter: std::cell::Cell::new(0),
        }
    }

    /// Check if the GPU has completed work signaled at `value`.
    pub fn is_done(&self, value: u64) -> bool {
        unsafe { self.raw.signaledValue() >= value }
    }

    /// Current signal counter (store after signal_event).
    pub fn current_value(&self) -> u64 {
        self.counter.get()
    }

    /// Read the GPU-side signaled value directly.
    pub fn signaled_value(&self) -> u64 {
        unsafe { self.raw.signaledValue() }
    }

    /// Block the calling thread until the GPU has signaled `value`, with a timeout.
    /// Returns `true` if the event was signaled, `false` if timed out.
    pub fn wait_until_done_timeout(&self, value: u64, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
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
                unsafe { self.raw.signaledValue() },
            );
        }
    }

    /// Raw Metal shared event reference.
    pub fn raw(&self) -> &ProtocolObject<dyn MTLSharedEvent> {
        &self.raw
    }
}

// ─── GpuHeap ──────────────────────────────────────────────────────────

/// GPU heap backed by a native MTLHeap.
/// Sub-allocates textures without per-allocation kernel calls.
pub struct GpuHeap {
    heap: Retained<ProtocolObject<dyn MTLHeap>>,
}

unsafe impl Send for GpuHeap {}
unsafe impl Sync for GpuHeap {}

impl GpuHeap {
    /// Create a new GpuHeap wrapping a Metal heap.
    pub(crate) fn new(heap: Retained<ProtocolObject<dyn MTLHeap>>) -> Self {
        Self { heap }
    }

    /// Sub-allocate a texture from this heap.
    /// Returns `None` if the heap doesn't have enough space.
    pub fn new_texture(&self, desc: &GpuTextureDesc) -> Option<GpuTexture> {
        let mtl_desc = GpuDevice::build_mtl_texture_desc(desc);
        // Override storage mode to match heap's storage mode.
        let storage_mode = unsafe { self.heap.storageMode() };
        unsafe { mtl_desc.setStorageMode(storage_mode) };
        let raw = unsafe { self.heap.newTextureWithDescriptor(&mtl_desc) }?;
        Some(GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        })
    }

    /// Total heap size in bytes.
    pub fn size(&self) -> u64 {
        unsafe { self.heap.size() as u64 }
    }

    /// Currently used heap memory in bytes.
    pub fn used_size(&self) -> u64 {
        unsafe { self.heap.usedSize() as u64 }
    }

    /// Maximum available contiguous allocation size with given alignment.
    pub fn max_available_size(&self, alignment: u64) -> u64 {
        unsafe { self.heap.maxAvailableSizeWithAlignment(alignment as usize) as u64 }
    }
}

// ─── GpuFenceWaiter ──────────────────────────────────────────────────

/// Kernel-notified GPU fence waiter.
///
/// Instead of busy-spinning on `GpuEvent::is_done()`, this registers a
/// Metal `MTLSharedEvent.notifyListener:atValue:block:` notification that
/// sends a wake signal through the caller's event channel when the GPU
/// signals the target value.
///
/// Platform-agnostic concept — this implementation uses Metal's
/// SharedEventListener. Future Vulkan/D3D12 backends would implement
/// equivalent fence notification (timeline semaphores / SetEventOnCompletion).
pub struct GpuFenceWaiter {
    listener: Retained<MTLSharedEventListener>,
}

unsafe impl Send for GpuFenceWaiter {}
unsafe impl Sync for GpuFenceWaiter {}

impl Default for GpuFenceWaiter {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuFenceWaiter {
    /// Create a new fence waiter.
    ///
    /// `[MTLSharedEventListener new]` — Metal provisions a default internal
    /// dispatch queue for firing notification blocks.
    pub fn new() -> Self {
        let listener = MTLSharedEventListener::new();
        Self { listener }
    }

    /// Register a notification for when the GPU event reaches `target_value`.
    pub fn register<F>(&self, event: &GpuEvent, target_value: u64, wake: F)
    where
        F: FnOnce() + Send + 'static,
    {
        use block2::RcBlock;

        let wake = std::sync::Mutex::new(Some(wake));
        let block = RcBlock::new(
            move |_event: std::ptr::NonNull<ProtocolObject<dyn MTLSharedEvent>>, _value: u64| {
                if let Ok(mut guard) = wake.lock()
                    && let Some(f) = guard.take()
                {
                    f();
                }
            },
        );
        unsafe {
            event.raw().notifyListener_atValue_block(
                &self.listener,
                target_value,
                RcBlock::as_ptr(&block),
            );
        }
    }
}
