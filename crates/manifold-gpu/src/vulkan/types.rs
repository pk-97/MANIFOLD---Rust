//! Vulkan resource types. The handles (`VkBuffer`, `VkImage` and their
//! memory, `VkPipeline` and its layout) arrive in P1 and P2; until then the
//! structs carry only what callers read.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use std::sync::Arc;

use super::SlotMap;
use crate::*;

/// GPU buffer: uniform, storage, vertex, index or readback.
#[derive(Clone)]
pub struct GpuBuffer {
    pub size: u64,
}

/// GPU texture: a 2D, 3D or array image with its view.
///
/// `Clone` is cheap, as on Metal: clones share the image.
#[derive(Clone)]
pub struct GpuTexture {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
    /// Fence-retirement mark: drops wait for the completion fence (D8).
    pub(crate) retire: Option<Arc<crate::RetireMark>>,
}

#[derive(Clone)]
pub struct GpuSampler {
    _reserved: (),
}

/// Compute pipeline: SPIR-V module, `VkPipeline`, layout and slot map.
pub struct GpuComputePipeline {
    pub slot_map: SlotMap,
    pub label: String,
    pub workgroup_size: [u32; 3],
}

/// Render pipeline. Vertex and fragment stages share one slot map, as on
/// Metal.
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

/// What a fence-stamped drop holds until its frame retires (D8).
pub(crate) enum RetiredResource {}

impl RetiredResource {
    pub(crate) fn release(self) {
        match self {}
    }
}

impl GpuTexture {
    /// Whether this texture may be bound to a shader stage (sampled).
    pub fn is_shader_readable(&self) -> bool {
        todo!("vulkan P1")
    }

    /// Whether two handles refer to the SAME underlying Metal texture object
    /// (identity, not value/size equality) — the `GpuBuffer::ptr_eq` idiom,
    /// extended to textures.
    pub fn ptr_eq(&self, other: &GpuTexture) -> bool {
        todo!("vulkan P1")
    }

    /// Raw pointer identity as a plain integer — a lighter-weight sibling of
    /// [`ptr_eq`](Self::ptr_eq) for callers that want to cache the identity
    /// (e.g.
    pub fn identity_key(&self) -> usize {
        todo!("vulkan P1")
    }

    /// Number of mip levels on the underlying Metal texture.
    pub fn mip_level_count(&self) -> u32 {
        todo!("vulkan P1")
    }

    /// Create a VIEW of a single mip level of `self`, sharing the same
    /// underlying storage (no new GPU allocation) but presented to the
    /// shader as its own complete, single-mip `GpuTexture` of the given
    /// level's dimensions.
    pub fn mip_level_view(&self, level: u32, mip_width: u32, mip_height: u32) -> GpuTexture {
        todo!("vulkan P1")
    }

    /// A plain 2D view of one layer of a `D2Array` texture, sharing its
    /// storage.
    pub fn layer_view(&self, layer: u32) -> GpuTexture {
        todo!("vulkan P1")
    }
}

impl GpuBuffer {
    /// Persistent mapped pointer (shared-memory buffers only).
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        todo!("vulkan P1")
    }

    /// Write data at offset via memcpy (shared-memory buffers only).
    ///
    /// # Safety
    /// Caller must ensure offset + data.len() <= buffer size,
    /// and no GPU reads overlap this write.
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        todo!("vulkan P1")
    }

    /// Zero every byte of a shared-memory buffer in place (CPU memset, no
    /// allocation).
    pub fn zero_fill(&self) {
        todo!("vulkan P1")
    }

    /// Whether two handles refer to the SAME underlying Metal buffer object
    /// (identity, not value/size equality).
    pub fn ptr_eq(&self, other: &GpuBuffer) -> bool {
        todo!("vulkan P1")
    }

    /// Raw pointer identity as a plain integer — the [`GpuTexture::identity_key`]
    /// sibling.
    pub fn identity_key(&self) -> usize {
        todo!("vulkan P1")
    }

    pub fn size(&self) -> u64 {
        todo!("vulkan P1")
    }

    /// RT-T1-B: this buffer's GPU virtual address (`MTLBuffer::gpuAddress`)
    /// — the bindless-read mechanism the RT trace kernel uses to fetch a
    /// per-object vertex-normal buffer at ray-hit time, indexed by
    /// `hit.instance_id` (see `raytrace::RtNormalSource`).
    pub fn gpu_address(&self) -> u64 {
        todo!("vulkan P1")
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
