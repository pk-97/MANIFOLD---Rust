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
//! ## ObjC bindings: objc2 + block2
//!
//! All direct ObjC interop (CAMetalLayer configuration, MPS/MetalFX kernels,
//! MTLSharedEvent notifiers, command buffer completion handlers, NSError
//! extraction) goes through `objc2` and `block2`. The `metal` crate (v0.33,
//! gfx-rs) still backs the device/queue/encoder/texture types — it internally
//! uses `objc 0.2`, but manifold-gpu itself no longer depends on `objc` or
//! `block` directly. Where the `metal` crate's typed API is insufficient, the
//! generated `objc2-metal`, `objc2-metal-fx`, and `objc2-metal-performance-shaders`
//! crates provide full coverage; raw pointer bridging between the two
//! ecosystems lives in `objc2_bridge.rs`.
//!
//! A future task is the full `metal` → `objc2-metal` migration (replacing the
//! wrapper types themselves). The current split keeps the hot-path code in
//! the battle-tested `metal` crate while removing our direct exposure to
//! unmaintained libraries.

pub mod archive;
pub mod metalfx;
pub mod mps;

mod device;
mod encoder;
mod format;
mod msl_cache;
mod objc2_bridge;
mod shader_compiler;
pub mod surface;
mod texture_pool;
mod types;

// Re-export all public types so external code paths remain identical.
pub use device::GpuDevice;
use encoder::ComputeBindCache;
pub use encoder::GpuEncoder;
pub use surface::{GpuDrawable, GpuSurface};
pub use texture_pool::TexturePool;
pub use types::{
    GpuBuffer, GpuComputePipeline, GpuDepthStencilState, GpuEvent, GpuFenceWaiter, GpuHeap,
    GpuRenderPipeline, GpuSampler, GpuTexture,
};

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
