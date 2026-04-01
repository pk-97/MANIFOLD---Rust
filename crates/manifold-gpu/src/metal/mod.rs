//! Native Metal backend for macOS content thread.
//!
//! Owns metal::Device, metal::CommandQueue, metal::CommandBuffer directly.
//! Zero abstraction overhead — direct Metal API calls.
//!
//! Shader compilation pipeline: WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross → MSL.
//! naga parses WGSL and provides binding introspection for the SlotMap.
//! spirv-opt runs optimization passes (constant folding, dead code elimination, etc.).
//! SPIRV-Cross compiles optimized SPIR-V to MSL with explicit resource binding indices
//! matching the SlotMap assignments. Metal compiles MSL at runtime.
//!
//! ## objc2-metal migration path (future task)
//!
//! The current `metal` crate (v0.33 from gfx-rs) is functional but missing newer
//! Metal features (MetalFX, MPS image processing), which is why `mps.rs` and
//! `metalfx.rs` use raw `objc::msg_send!`. `objc2-metal` (v0.3.2, 13M+ downloads)
//! is the successor with full API coverage including MPS and MetalFX. A full
//! migration from `metal` to `objc2-metal` would touch every file in manifold-gpu
//! (different naming conventions, ownership model via `objc2::rc::Retained`).
//! This is a future task — the current `metal` crate works correctly for all
//! existing functionality.

#[allow(unexpected_cfgs)]
pub mod mps;
pub mod archive;
pub mod metalfx;

mod device;
mod types;
mod texture_pool;
mod encoder;
mod shader_compiler;
mod format;
mod msl_cache;
pub mod surface;

// Re-export all public types so external code paths remain identical.
pub use device::GpuDevice;
pub use types::{
    GpuTexture, GpuBuffer, GpuSampler, GpuComputePipeline, GpuRenderPipeline, GpuEvent, GpuHeap,
};
pub use texture_pool::TexturePool;
pub use encoder::GpuEncoder;
use encoder::ComputeBindCache;
pub use surface::{GpuSurface, GpuDrawable};

// Raw ObjC retain/release — avoids dependency on objc::msg_send! macro.
// Used by both device (command buffer retain) and encoder (encoder retain/release).
unsafe extern "C" {
    fn objc_retain(obj: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn objc_release(obj: *mut std::ffi::c_void);
}

/// Reserved WGSL "binding" index for the naga sizes buffer.
/// Not a real @binding — used internally by the slot map.
/// Shared between shader_compiler (slot map construction) and encoder (dispatch).
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

// ─── Slot mapping ─────────────────────────────────────────────────────

/// Maps WGSL @binding(N) to Metal argument indices.
/// Built during pipeline creation from naga module introspection.
#[derive(Clone, Debug)]
pub struct SlotMap {
    /// Indexed by WGSL @binding(N). Each entry gives the Metal argument type and index.
    slots: Vec<Option<Slot>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub kind: SlotKind,
    pub metal_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Buffer,
    Texture,
    Sampler,
}

impl SlotMap {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn insert(&mut self, binding: u32, slot: Slot) {
        let idx = binding as usize;
        if idx >= self.slots.len() {
            self.slots.resize(idx + 1, None);
        }
        self.slots[idx] = Some(slot);
    }

    /// Look up the Metal argument index for a WGSL @binding(N).
    #[inline]
    pub fn get(&self, binding: u32) -> Option<&Slot> {
        self.slots.get(binding as usize).and_then(|s| s.as_ref())
    }
}
