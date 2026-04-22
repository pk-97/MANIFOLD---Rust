//! Vulkan backend for manifold-gpu.
//!
//! Selected at compile time via the `vulkan` Cargo feature. Uses `ash`
//! loader bindings and consumes SPIR-V emitted by `crate::shader_common`
//! (the same pipeline the Metal backend feeds into SPIRV-Cross → MSL).
//!
//! ## Scope
//!
//! The Vulkan backend is the cross-platform path for Windows and Linux.
//! On macOS it routes through MoltenVK — not for shipping, but as a
//! convenient way to iterate on the cross-platform code from the dev box
//! without booting into another OS.
//!
//! ## Status
//!
//! Phase 0 scaffolding only. The type surface mirrors `metal/` so the
//! cfg-gated backend selection at the crate root compiles against either
//! backend without callers noticing. Actual Vulkan calls — VkInstance,
//! VkDevice, memory allocation, descriptor sets, pipelines, command
//! buffers — land in Phase 1+.
//!
//! ## API parity
//!
//! Names and signatures of exported types mirror `metal/`. Internals
//! differ — Metal tracks bindings as flat argument indices; Vulkan tracks
//! them as `(set=0, binding=N)` descriptor slots with stage flags. The
//! per-backend `SlotMap` captures that difference.

mod device;
mod encoder;
mod shader_compiler;
mod types;

pub use device::GpuDevice;
pub use encoder::GpuEncoder;
pub use types::{GpuBuffer, GpuComputePipeline, GpuRenderPipeline, GpuSampler, GpuTexture};

/// Reserved WGSL binding slot for naga's "sizes buffer" (resolves
/// `arrayLength()` on runtime-sized storage arrays). Kept at the same
/// sentinel value as the Metal backend so `shader_common` never needs to
/// ask which backend is active — the value is universally a non-emitted
/// binding that can't collide with real user-declared @binding(N)s.
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

/// Descriptor type kinds that a Vulkan binding can take. Mirrors the set
/// of `VkDescriptorType` values we actually emit (uniform buffer, storage
/// buffer, sampled image, storage image, sampler).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    /// Uniform or storage buffer (`VK_DESCRIPTOR_TYPE_{UNIFORM,STORAGE}_BUFFER`).
    Buffer,
    /// Sampled or storage image (`VK_DESCRIPTOR_TYPE_{SAMPLED,STORAGE}_IMAGE`).
    Texture,
    /// Sampler (`VK_DESCRIPTOR_TYPE_SAMPLER`).
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
}
