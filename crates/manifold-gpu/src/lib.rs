//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD.
//!
//! Native Metal on macOS. Zero vtable overhead.
//! All GPU resources are native Metal types.
//!
//! Used by both the content thread (rendering, effects, generators) and
//! the UI thread (native Metal UI rendering via GpuSurface).

// objc2-metal marks some methods `unsafe fn` and others safe based on ObjC
// memory-management rules that don't translate to meaningful Rust invariants
// (e.g. `setWidth:` on a fresh descriptor is safe; `setWidth:` on a shared
// descriptor with active encoders is not). We wrap Metal calls in `unsafe`
// blocks uniformly — the per-call safety audit lives in the objc2-metal
// bindings, not here. Silence the unused-unsafe lint crate-wide.
#![allow(unused_unsafe)]

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub mod queue;
pub mod types;
pub use types::*;
pub mod shadow_caster;
pub use shadow_caster::{ShadowCaster, upload_shadow_casters};
pub mod trace_planner;
pub use trace_planner::*;
// The recording core is unused on Vulkan until its store records command
// buffers; that store lifts this allow.
#[cfg_attr(feature = "vulkan", allow(dead_code))]
mod replay;
pub use replay::{
    GATED_RANGE_BYTES, GpuReplayStats, GpuTemplateStats, REPLAY_RING, TemplateRanges,
    chunk_rounds, template_chunks,
};

/// Point-in-time device memory values used by resource admission. The
/// snapshot is deliberately plain data so callers can capture it once at an
/// admission boundary and pass it through pure planning code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuMemorySnapshot {
    pub current_allocated_bytes: u64,
    pub recommended_max_working_set_bytes: u64,
}

// Backend-neutral shader compilation pipeline (WGSL → naga → SPIR-V).
// Compiled on every platform; each backend's shader compiler consumes the
// optimised SPIR-V and emits platform-specific shader modules.
mod shader_common;
mod workgroup_zeroing;

// ─── Backend selection ────────────────────────────────────────────────
//
// Exactly one backend is active per build:
//
// * Default on macOS: native Metal (`metal/` module). Zero overhead, full
//   feature set including MPSGraph FFT and MetalFX. This is the ship path.
//
// * With `--features vulkan`: Vulkan backend (`vulkan/` module). Used for
//   cross-platform builds (Windows/Linux) and for dev-box testing on
//   macOS through KosmicKrisp or MoltenVK (D21). Metal backend is not
//   compiled in this mode.
//
// The two backends expose the same public surface (GpuDevice, GpuBuffer,
// GpuEncoder, etc.), so downstream crates and user code don't need to
// care which is active. Metal-only APIs that don't have a Vulkan
// equivalent (MetalFX upscaler, MPSGraph FFT) remain exposed only in the
// Metal build — callers that want them are already platform-gating.

#[cfg(not(feature = "vulkan"))]
mod metal;
#[cfg(not(feature = "vulkan"))]
use metal as backend;
#[cfg(not(feature = "vulkan"))]
pub use metal::*;

#[cfg(feature = "vulkan")]
mod vulkan;
#[cfg(feature = "vulkan")]
use vulkan as backend;
#[cfg(feature = "vulkan")]
pub use vulkan::*;

// GPU fault counters and kinds; each backend decodes its own errors into them.
// Vulkan records into them once it submits (P1).
#[cfg_attr(feature = "vulkan", allow(dead_code))]
pub mod gpu_fault;
// macOS GPU reset reports, read the same under either backend.
pub mod firmware_reports;
mod gpu_profile;
pub use gpu_profile::*;
// Vulkan reads the draw descriptors once it draws (P2).
#[cfg_attr(feature = "vulkan", allow(dead_code))]
mod draw_types;
pub use draw_types::*;

// Backend-neutral frame lifetime: the frame fence, fence-stamped drop
// retirement and the texture pool. Each backend supplies small hooks
// (`RetiredResource`, `PoolAllocator`).
mod frame_fence;
pub use frame_fence::FrameFence;
// Vulkan stamps its drops once it submits (P1).
#[cfg_attr(feature = "vulkan", allow(dead_code))]
mod retire;
pub use retire::{FrameClock, RetireMark, RetireQueue, RetireSender};
mod texture_pool;
pub use texture_pool::TexturePool;
// Ray-tracing data types; each backend re-exports them from its `raytrace`.
// Vulkan uses them once it traces rays (P7).
#[cfg_attr(feature = "vulkan", allow(dead_code))]
mod raytrace_types;
