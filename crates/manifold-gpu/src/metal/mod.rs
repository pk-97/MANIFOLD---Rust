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

mod archive;
pub mod denoiser;
pub mod fft;
pub mod metalfx;
pub mod metalfx_m4; // RAYTRACING_DESIGN.md section 17.7 DN-K

pub mod defer_drop;
mod device;
mod encoder;
mod fault_decode;
mod format;
mod msl_cache;
mod profiling;
pub mod raytrace;
mod replay;
mod residency;
mod scalers;
mod shader_compiler;
mod span_timer;
mod spirv_msl_fixup;
pub mod surface;
mod template;
mod types;

#[cfg(all(test, feature = "gpu-proofs"))]
mod async_lane_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod pipeline_cache_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod replay_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod template_tests;
#[cfg(test)]
mod workgroup_zeroing_tests;

// Re-export all public types so external code paths remain identical.
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

/// Reserved WGSL "binding" index for the naga sizes buffer.
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

/// Runs `f` inside an autorelease pool, so the Metal objects one frame
/// autoreleases are freed at its end instead of piling up until the thread
/// exits.
pub fn autorelease_scope<R>(f: impl FnOnce() -> R) -> R {
    objc2::rc::autoreleasepool(|_| f())
}

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
    /// The shader writes this texture (storage write or read_write access).
    /// Debug builds check the bound texture carries shader-write usage.
    pub writes: bool,
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

    /// Whether every binding is a buffer: Metal refuses indirect-command
    /// support for a function that binds a texture or sampler directly.
    pub(crate) fn buffers_only(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .all(|slot| slot.kind == SlotKind::Buffer)
    }

    /// The WGSL bindings the entry point references, the sizes buffer
    /// (bound by the encoder itself) left out.
    pub fn bindings(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(b, _)| b as u32)
            .filter(|&b| b != SIZES_BUFFER_BINDING)
    }
}
