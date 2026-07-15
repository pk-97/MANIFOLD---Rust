//! Vulkan backend resource types.
//!
//! Phase 0: struct shells with the minimum state the cross-backend API
//! surface requires (label, size, format). Phase 1 fills in the real
//! Vulkan handles — `VkBuffer`, `VkDeviceMemory`, `VkImage`, `VkImageView`,
//! `VkPipeline`, `VkPipelineLayout`, etc. Inner layout will mirror the
//! Metal backend's `metal/types.rs`: concrete struct, pub crate fields,
//! backend-specific `raw` handle, shared book-keeping up front.

use super::SlotMap;

/// GPU buffer — uniform, storage, or vertex/index.
///
/// The Metal backend exposes `size` + an optional host-mapped pointer via
/// `mapped_ptr()`. Vulkan's equivalent wraps `VkBuffer` + `VkDeviceMemory`;
/// host-visible allocations populate `mapped_ptr` after `vkMapMemory` at
/// creation time and stay mapped for the buffer's lifetime (persistent
/// mapping is the common pattern for per-frame upload buffers).
pub struct GpuBuffer {
    pub size: u64,
    /// Host-mapped pointer for `HOST_VISIBLE | HOST_COHERENT` allocations.
    /// `None` for device-local buffers that require staging for upload.
    pub(crate) mapped_ptr: Option<*mut u8>,
}

impl GpuBuffer {
    /// Host-mapped pointer for zero-copy CPU writes. Returns `None` when
    /// the buffer was created device-local (requires a staging transfer
    /// to populate from the CPU).
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        self.mapped_ptr
    }
}

/// GPU texture — 2D or 3D image with an image view.
///
/// Phase 0 holds only the dimensions and format metadata callers already
/// consume. Phase 1 adds `VkImage`, `VkImageView`, `VkDeviceMemory`, and
/// a current `VkImageLayout` for barrier bookkeeping.
///
/// `Clone` is cheap — the Metal-backed equivalent is one atomic retain.
/// The portability stub mirrors that contract so chain-runtime call
/// sites compile against both backends.
#[derive(Clone)]
pub struct GpuTexture {
    pub width: u32,
    pub height: u32,
    pub format: crate::GpuTextureFormat,
}

/// GPU sampler — `VkSampler` under the hood. Configuration comes from
/// `GpuSamplerDesc` (shared crate-level type).
// VULKAN_BACKEND_DESIGN: `GpuSamplerDesc.max_anisotropy` (GLB_CONFORMANCE_DESIGN
// G-P3, D7) is not wired here yet — Phase 0 has no `create_sampler` at all. When
// Phase 1 brings up `vkCreateSampler`, map it to
// `VkSamplerCreateInfo.anisotropyEnable`/`maxAnisotropy` (requires the
// `samplerAnisotropy` physical-device feature). Tracked gap, not silently ignored.
pub struct GpuSampler {
    pub(crate) _reserved: (),
}

/// Compute pipeline: compiled SPIR-V module + `VkPipeline` + layout +
/// slot reflection.
pub struct GpuComputePipeline {
    pub slot_map: SlotMap,
    pub label: String,
    pub workgroup_size: [u32; 3],
    /// Whether this pipeline reads from naga's sizes buffer (set when the
    /// shader calls `arrayLength()` on a runtime-sized storage array).
    pub needs_sizes_buffer: bool,
}

/// Render pipeline: vertex + fragment SPIR-V + `VkPipeline` + layout +
/// shared slot reflection. Mirrors the Metal backend's flat model: both
/// stages see the same descriptor bindings.
pub struct GpuRenderPipeline {
    pub slot_map: SlotMap,
    pub label: String,
    pub needs_sizes_buffer: bool,
}
