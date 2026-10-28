//! Vulkan backend for manifold-gpu.
//!
//! Selected at compile time by the `vulkan` Cargo feature. Uses `ash`
//! loader bindings and consumes the SPIR-V `crate::shader_common` emits (the
//! same SPIR-V the Metal backend turns into MSL).
//!
//! The backend for Linux and, later, Windows. On the Mac it runs on
//! KosmicKrisp, the primary test driver, or MoltenVK (D21).
//!
//! ## Status
//!
//! A skeleton: every type and method mirrors `metal/` with a `todo!` that
//! names the phase that builds it (design section 9), so the workspace
//! compiles against this backend. Ray tracing, the denoiser, the upscaler
//! and the GPU FFT report unavailable through `GpuDevice::supports_*`.
//!
//! ## Bindings
//!
//! Metal tracks bindings as flat argument indices; Vulkan as
//! `(set=0, binding=N)` descriptor slots with a descriptor type each. The
//! per-backend `SlotMap` captures that difference.

mod core;
mod device;
mod encoder;
pub mod fft;
mod profiling;
pub mod raytrace;
mod replay;
mod residency;
mod scalers;
mod shader_compiler;
mod span_timer;
pub mod surface;
mod template;
mod types;

#[cfg(test)]
mod parity_tests;
#[cfg(test)]
pub(crate) use core::validation_error_count;
mod pipeline;
mod submit;
mod tracker;

pub use device::GpuDevice;
pub(crate) use device::PoolAllocator;
pub use encoder::GpuEncoder;
pub use fft::GpuFft;
pub use profiling::GpuTimestampSampler;
pub use replay::GpuReplayCache;
pub use residency::GpuResidencyManager;
pub use scalers::{GpuSpatialUpscaler, GpuTemporalDenoiser, GpuTemporalUpscaler};
pub use span_timer::GpuSpanTimer;
pub use surface::{GpuDrawable, GpuSurface, set_presented_observer};
pub use template::{GatedRecorder, GpuTemplateStore, TemplateTicket};
pub(crate) use types::RetiredResource;
pub use types::{
    GpuBuffer, GpuComputePipeline, GpuDepthStencilState, GpuEvent, GpuFenceWaiter,
    GpuRenderPipeline, GpuSampler, GpuTexture,
};

/// Vulkan autoreleases nothing, so there is no per-frame pool to drain.
pub fn autorelease_scope<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// The `VkDescriptorType` a binding takes (D2): uniform and storage buffers,
/// and sampled and storage images, are different descriptor types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    UniformBuffer,
    StorageBuffer,
    SampledImage,
    StorageImage,
    Sampler,
}

/// One entry in a Vulkan pipeline's slot map — WGSL `@binding(N)` → Vulkan
/// descriptor binding at `(set=0, binding=N)`. Because naga's SPIR-V
/// back-end emits `(set=0, binding=N)` decorations matching the original
/// WGSL binding number, this is effectively identity on the index; the
/// kind / writability / stage flags drive `VkDescriptorSetLayout` creation.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub kind: SlotKind,
    /// Vulkan descriptor binding index (same as WGSL @binding(N) in most
    /// cases — retained as a distinct field for symmetry with the Metal
    /// backend's `metal_index` where Metal's flat argument table demanded
    /// remapping).
    pub vulkan_binding: u32,
    pub writable: bool,
}

/// Maps WGSL `@binding(N)` to Vulkan descriptor binding metadata. Built by
/// `shader_compiler::build_slot_map`; consumed by descriptor set layout
/// creation and by `GpuEncoder` binding logic.
///
/// Sparse by binding index — WGSL shaders commonly skip numbers
/// (`@binding(0)`, `@binding(2)`, ...).
#[derive(Clone, Debug, Default)]
pub struct SlotMap {
    slots: Vec<Option<Slot>>,
}

impl SlotMap {
    pub(crate) fn new() -> Self {
        Self { slots: Vec::new() }
    }

    pub(crate) fn insert(&mut self, binding: u32, slot: Slot) {
        let idx = binding as usize;
        if idx >= self.slots.len() {
            self.slots.resize(idx + 1, None);
        }
        self.slots[idx] = Some(slot);
    }

    #[inline]
    pub fn get(&self, binding: u32) -> Option<&Slot> {
        self.slots.get(binding as usize).and_then(|s| s.as_ref())
    }

    /// The WGSL bindings the entry point references.
    pub fn bindings(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(b, _)| b as u32)
    }
}
