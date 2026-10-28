//! Vulkan resource types. Buffers, textures and samplers hold their handles
//! behind an `Arc`, so clones are cheap and an encoder keeps what it touched
//! alive until its submit completes (D8). Pipelines arrive in P1 step 10 and
//! P2, events in step 12.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::Arc;

use ash::vk;
use gpu_allocator::vulkan::Allocation;

use super::SlotMap;
use super::core::Core;
use crate::*;

/// A `VkBuffer` and its memory, freed when the last handle drops.
pub(crate) struct BufferCore {
    core: Arc<Core>,
    pub(crate) raw: vk::Buffer,
    allocation: ManuallyDrop<Allocation>,
    mapped: Option<NonNull<u8>>,
}

// Safety: the mapped pointer is plain host memory the buffer owns; writes go
// through `GpuBuffer`'s unsafe contract, as on Metal.
unsafe impl Send for BufferCore {}
unsafe impl Sync for BufferCore {}

impl BufferCore {
    /// `shared` buffers expose their mapping; private ones never do, even
    /// where all memory is host-visible, so CPU access fails the same way it
    /// would on a discrete GPU.
    pub(crate) fn new(
        core: Arc<Core>,
        raw: vk::Buffer,
        allocation: Allocation,
        shared: bool,
    ) -> Self {
        let mapped = allocation
            .mapped_ptr()
            .filter(|_| shared)
            .map(|p| p.cast::<u8>());
        Self {
            core,
            raw,
            allocation: ManuallyDrop::new(allocation),
            mapped,
        }
    }
}

impl Drop for BufferCore {
    fn drop(&mut self) {
        // Safety: the allocation is taken once, here, and the buffer is
        // destroyed after its memory is returned.
        let allocation = unsafe { ManuallyDrop::take(&mut self.allocation) };
        if let Err(err) = self
            .core
            .allocator
            .lock()
            .expect("allocator poisoned")
            .free(allocation)
        {
            log::error!("[Vulkan] buffer free failed: {err}");
        }
        unsafe { self.core.device.destroy_buffer(self.raw, None) };
    }
}

/// GPU buffer: uniform, storage, vertex, index or readback.
#[derive(Clone)]
pub struct GpuBuffer {
    pub size: u64,
    pub(crate) inner: Arc<BufferCore>,
    /// Fence-retirement mark: drops wait for the completion fence.
    pub(crate) retire: Option<Arc<crate::RetireMark>>,
}

impl Drop for GpuBuffer {
    fn drop(&mut self) {
        if let Some(mark) = &self.retire {
            mark.retire(RetiredResource::Buffer(self.inner.clone()));
        }
    }
}

/// A `VkImage` and its memory: the root every view of it shares, and the
/// identity the hazard tracker keys on (design section 5).
pub(crate) struct ImageCore {
    core: Arc<Core>,
    pub(crate) raw: vk::Image,
    allocation: ManuallyDrop<Allocation>,
    pub(crate) aspect: vk::ImageAspectFlags,
    pub(crate) usage: GpuTextureUsage,
    pub(crate) mip_levels: u32,
}

impl ImageCore {
    pub(crate) fn new(
        core: Arc<Core>,
        raw: vk::Image,
        allocation: Allocation,
        aspect: vk::ImageAspectFlags,
        usage: GpuTextureUsage,
        mip_levels: u32,
    ) -> Self {
        Self {
            core,
            raw,
            allocation: ManuallyDrop::new(allocation),
            aspect,
            usage,
            mip_levels,
        }
    }
}

impl Drop for ImageCore {
    fn drop(&mut self) {
        // Safety: as for `BufferCore`.
        let allocation = unsafe { ManuallyDrop::take(&mut self.allocation) };
        if let Err(err) = self
            .core
            .allocator
            .lock()
            .expect("allocator poisoned")
            .free(allocation)
        {
            log::error!("[Vulkan] image free failed: {err}");
        }
        unsafe { self.core.device.destroy_image(self.raw, None) };
    }
}

/// One `VkImageView` over a range of an image's mips and layers.
pub(crate) struct ViewCore {
    pub(crate) image: Arc<ImageCore>,
    pub(crate) raw: vk::ImageView,
    pub(crate) base_mip: u32,
    pub(crate) mip_count: u32,
    pub(crate) base_layer: u32,
    // The encoder's barriers read the range (P1 step 11).
    #[allow(dead_code)]
    pub(crate) layer_count: u32,
}

impl ViewCore {
    pub(crate) fn new(
        image: Arc<ImageCore>,
        format: GpuTextureFormat,
        view_type: vk::ImageViewType,
        mips: (u32, u32),
        layers: (u32, u32),
        label: &str,
    ) -> Self {
        let core = &image.core;
        let info = vk::ImageViewCreateInfo::default()
            .image(image.raw)
            .view_type(view_type)
            .format(super::core::vk_format(format))
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: image.aspect,
                base_mip_level: mips.0,
                level_count: mips.1,
                base_array_layer: layers.0,
                layer_count: layers.1,
            });
        let raw = unsafe { core.device.create_image_view(&info, None) }
            .unwrap_or_else(|e| panic!("Vulkan: image view for {label} failed: {e}"));
        core.name(raw, label);
        Self {
            image,
            raw,
            base_mip: mips.0,
            mip_count: mips.1,
            base_layer: layers.0,
            layer_count: layers.1,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn range(&self) -> vk::ImageSubresourceRange {
        vk::ImageSubresourceRange {
            aspect_mask: self.image.aspect,
            base_mip_level: self.base_mip,
            level_count: self.mip_count,
            base_array_layer: self.base_layer,
            layer_count: self.layer_count,
        }
    }
}

impl Drop for ViewCore {
    fn drop(&mut self) {
        unsafe { self.image.core.device.destroy_image_view(self.raw, None) };
    }
}

/// GPU texture: a 2D, 3D or array image seen through one view.
///
/// `Clone` is cheap, as on Metal: clones share the view and the image.
#[derive(Clone)]
pub struct GpuTexture {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
    /// Fence-retirement mark: drops wait for the completion fence (D8).
    pub(crate) retire: Option<Arc<crate::RetireMark>>,
    pub(crate) view: Arc<ViewCore>,
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        if let Some(mark) = &self.retire {
            mark.retire(RetiredResource::Texture(self.view.clone()));
        }
    }
}

/// A `VkSampler`, destroyed when the last handle drops.
pub(crate) struct SamplerCore {
    core: Arc<Core>,
    pub(crate) raw: vk::Sampler,
}

impl SamplerCore {
    pub(crate) fn new(core: Arc<Core>, raw: vk::Sampler) -> Self {
        Self { core, raw }
    }
}

impl Drop for SamplerCore {
    fn drop(&mut self) {
        unsafe { self.core.device.destroy_sampler(self.raw, None) };
    }
}

#[derive(Clone)]
pub struct GpuSampler {
    // Descriptor writes read the handle (P1 step 10).
    #[allow(dead_code)]
    pub(crate) inner: Arc<SamplerCore>,
}

/// Compute pipeline: SPIR-V module, `VkPipeline`, layout and slot map.
#[derive(Clone)]
pub struct GpuComputePipeline {
    pub slot_map: SlotMap,
    pub label: String,
    pub workgroup_size: [u32; 3],
}

/// Render pipeline. Vertex and fragment stages share one slot map, as on
/// Metal.
#[derive(Clone)]
pub struct GpuRenderPipeline {
    pub slot_map: SlotMap,
    pub label: String,
    pub sample_count: u32,
}

pub struct GpuDepthStencilState {
    _reserved: (),
}

/// A timeline semaphore and its CPU counter (D7).
pub struct GpuEvent {
    _reserved: (),
}

pub struct GpuFenceWaiter {
    _reserved: (),
}

impl Default for GpuFenceWaiter {
    fn default() -> Self {
        Self::new()
    }
}

/// What a fence-stamped drop holds until its frame retires: a handle that
/// keeps the resource alive (D8), never read.
#[allow(dead_code)]
pub(crate) enum RetiredResource {
    Buffer(Arc<BufferCore>),
    Texture(Arc<ViewCore>),
}

impl RetiredResource {
    pub(crate) fn release(self) {
        drop(self);
    }
}

impl GpuTexture {
    /// Whether this texture may be bound to a shader stage (sampled).
    pub fn is_shader_readable(&self) -> bool {
        self.view.image.usage.contains(GpuTextureUsage::SHADER_READ)
    }

    /// Whether two handles are the same texture: clones share a view, while
    /// a mip or layer view of it is a different texture over the same image.
    pub fn ptr_eq(&self, other: &GpuTexture) -> bool {
        Arc::ptr_eq(&self.view, &other.view)
    }

    /// [`ptr_eq`](Self::ptr_eq)'s identity as a plain integer, for callers
    /// that cache it.
    pub fn identity_key(&self) -> usize {
        Arc::as_ptr(&self.view) as usize
    }

    /// Number of mip levels this texture shows.
    pub fn mip_level_count(&self) -> u32 {
        self.view.mip_count
    }

    /// A single-mip 2D view of `level` (layer 0), sharing the image. The
    /// image needs `mip_levels > level`.
    pub fn mip_level_view(&self, level: u32, mip_width: u32, mip_height: u32) -> GpuTexture {
        let image = &self.view.image;
        assert!(
            level < image.mip_levels,
            "mip {level} out of range ({} levels)",
            image.mip_levels
        );
        GpuTexture {
            width: mip_width,
            height: mip_height,
            depth: 1,
            format: self.format,
            retire: self.retire.clone(),
            view: Arc::new(ViewCore::new(
                image.clone(),
                self.format,
                vk::ImageViewType::TYPE_2D,
                (level, 1),
                (0, 1),
                "mip view",
            )),
        }
    }

    /// A plain 2D view of one layer of a `D2Array` texture, sharing its
    /// storage. Create views when the array is allocated, never per frame.
    pub fn layer_view(&self, layer: u32) -> GpuTexture {
        assert!(
            layer < self.depth,
            "layer {layer} out of range ({} layers)",
            self.depth
        );
        GpuTexture {
            width: self.width,
            height: self.height,
            depth: 1,
            format: self.format,
            retire: self.retire.clone(),
            view: Arc::new(ViewCore::new(
                self.view.image.clone(),
                self.format,
                vk::ImageViewType::TYPE_2D,
                (0, 1),
                (layer, 1),
                "layer view",
            )),
        }
    }
}

impl GpuBuffer {
    /// Persistent mapped pointer (shared-memory buffers only).
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        self.inner.mapped.map(NonNull::as_ptr)
    }

    /// Write data at offset via memcpy (shared-memory buffers only).
    ///
    /// # Safety
    /// Caller must ensure offset + data.len() <= buffer size,
    /// and no GPU reads overlap this write.
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        let ptr = self
            .mapped_ptr()
            .expect("write() requires a shared-memory buffer");
        debug_assert!(offset + data.len() as u64 <= self.size);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset as usize), data.len())
        };
    }

    /// Zero every byte of a shared-memory buffer in place (CPU memset, no
    /// allocation).
    pub fn zero_fill(&self) {
        let ptr = self
            .mapped_ptr()
            .expect("zero_fill() requires a shared-memory buffer");
        // Safety: the mapping covers `size` bytes; called before GPU use.
        unsafe { std::ptr::write_bytes(ptr, 0, self.size as usize) };
    }

    /// Whether two handles are the same buffer (identity, not contents).
    pub fn ptr_eq(&self, other: &GpuBuffer) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// [`ptr_eq`](Self::ptr_eq)'s identity as a plain integer.
    pub fn identity_key(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The buffer's device address, which only ray tracing reads (P7).
    pub fn gpu_address(&self) -> u64 {
        panic!("buffer device addresses arrive with ray tracing (vulkan P7)")
    }
}

impl GpuComputePipeline {
    /// Legal threadgroup maximum reported by the backend, not an occupancy metric.
    pub fn max_threads_per_threadgroup(&self) -> Option<u32> {
        todo!("vulkan P1")
    }

    /// The first binding the entry point references that `bindings` leaves
    /// out.
    pub fn unbound_binding(&self, bindings: &[crate::GpuBinding]) -> Option<u32> {
        self.slot_map
            .bindings()
            .find(|&wanted| !bindings.iter().any(|b| b.binding() == wanted))
    }
}

impl GpuEvent {
    /// Second handle to the SAME underlying shared event and host counter.
    pub fn second_handle(&self) -> Self {
        todo!("vulkan P1")
    }

    /// Check if the GPU has completed work signaled at `value`.
    pub fn is_done(&self, value: u64) -> bool {
        todo!("vulkan P1")
    }

    /// The value the last encoded signal will reach once its command buffer
    /// completes: a CPU counter bumped at encode time, not a GPU reading.
    pub fn encoded_value(&self) -> u64 {
        todo!("vulkan P1")
    }

    /// Read the GPU-side signaled value directly.
    pub fn signaled_value(&self) -> u64 {
        todo!("vulkan P1")
    }

    /// Block the calling thread until the GPU has signaled `value`, with a timeout.
    pub fn wait_until_done_timeout(&self, value: u64, timeout_ms: u64) -> bool {
        todo!("vulkan P1")
    }

    /// Block the calling thread until the GPU has signaled `value`.
    pub fn wait_until_done(&self, value: u64) {
        todo!("vulkan P1")
    }
}

impl GpuFenceWaiter {
    /// Create a new fence waiter.
    pub fn new() -> Self {
        todo!("vulkan P1")
    }

    /// Register a notification for when the GPU event reaches `target_value`.
    pub fn register<F>(&self, event: &GpuEvent, target_value: u64, wake: F)
    where
        F: FnOnce() + Send + 'static,
    {
        todo!("vulkan P1")
    }
}
