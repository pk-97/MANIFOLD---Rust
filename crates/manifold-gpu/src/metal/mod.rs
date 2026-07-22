//! Native Metal backend for macOS content thread.
//!
//! Uses `objc2-metal` typed bindings for all Metal interop. No dependency on
//! the unmaintained `metal` crate (gfx-rs).
//!
//! Shader compilation pipeline: WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross → MSL.
//! naga parses WGSL and provides binding introspection for the SlotMap.
//! spirv-opt runs optimization passes (constant folding, dead code elimination, etc.).
//! SPIRV-Cross compiles optimized SPIR-V to MSL with explicit resource binding indices
//! matching the SlotMap assignments. Metal compiles MSL at runtime.

pub mod archive;
pub mod fft;
pub mod metalfx;
pub mod mps;

mod device;
mod encoder;
mod format;
mod frame_fence;
mod msl_cache;
mod profiling;
pub mod raytrace;
mod shader_compiler;
pub mod surface;
mod texture_pool;
mod types;

// Re-export all public types so external code paths remain identical.
pub use device::GpuDevice;
pub use encoder::{DepthMsaaDraw, DepthMsaaPassDesc, GpuEncoder};
pub use fft::{FftKind, GpuFft};
pub use frame_fence::FrameFence;
pub use profiling::{GpuFrameProfile, GpuProfiledSpan, GpuTimestampSampler, GpuWorkKind};
pub use surface::{GpuDrawable, GpuSurface};
pub use texture_pool::TexturePool;
pub use types::{
    GpuBuffer, GpuComputePipeline, GpuDepthStencilState, GpuEvent, GpuFenceWaiter, GpuHeap,
    GpuRenderPipeline, GpuSampler, GpuTexture,
};

/// Reserved WGSL "binding" index for the naga sizes buffer.
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

// ─── Slot mapping ─────────────────────────────────────────────────────

/// Maps WGSL @binding(N) to Metal argument indices.
#[derive(Clone, Debug)]
pub struct SlotMap {
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
