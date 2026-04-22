//! Vulkan `GpuDevice` — the root handle callers create first.
//!
//! Phase 0 scaffold: empty struct, no `new()` yet. Phase 1 will bring
//! up `VkInstance`, `VkPhysicalDevice`, `VkDevice`, queue selection, and
//! the `VkCommandPool` / `VkDescriptorPool` / `VkPipelineCache` handles
//! all resource creation hangs off of.
//!
//! The public method surface will mirror `metal::device::GpuDevice` so
//! the crate root can re-export either backend without callers noticing.

/// GPU device — owns the `VkInstance` + `VkDevice` and acts as the root
/// for all resource creation (`create_buffer_shared`, `create_render_pipeline`,
/// etc.). Handles are freed on drop.
pub struct GpuDevice {
    pub(crate) _reserved: (),
}
