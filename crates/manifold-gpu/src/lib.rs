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
#[cfg_attr(vulkan_backend, allow(dead_code))]
mod replay;
pub use replay::{
    GATED_RANGE_BYTES, GpuReplayStats, GpuTemplateStats, REPLAY_RING, TemplateRanges, chunk_rounds,
    template_chunks,
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
mod alloc_counts;
mod shader_common;
// `manifold look-loop`'s edited shader text, swapped in at compile time.
pub mod shader_swap;
mod workgroup_zeroing;

// ─── Backend selection ────────────────────────────────────────────────
//
// Exactly one backend is active per build, chosen by `build.rs`:
//
// * macOS by default: native Metal (`metal/`). This is the ship path.
//
// * Any other target, or `--features vulkan` on macOS: Vulkan (`vulkan/`).
//   On macOS it runs on KosmicKrisp or MoltenVK (D21).
//
// The two backends expose the same public surface; features one backend
// lacks are capabilities on `GpuDevice` (`supports_ray_tracing` and the
// rest), so downstream crates never check which backend is active.

#[cfg(not(vulkan_backend))]
mod metal;
#[cfg(not(vulkan_backend))]
use metal as backend;
#[cfg(not(vulkan_backend))]
pub use metal::*;

#[cfg(vulkan_backend)]
mod vulkan;
#[cfg(vulkan_backend)]
use vulkan as backend;
#[cfg(vulkan_backend)]
pub use vulkan::*;

// GPU fault counters and kinds; each backend decodes its own errors into them.
// Vulkan records into them once it submits (P1).
#[cfg_attr(vulkan_backend, allow(dead_code))]
pub mod gpu_fault;
// macOS GPU reset reports, read the same under either backend.
pub mod firmware_reports;
mod gpu_profile;
pub use gpu_profile::*;
mod draw_types;
pub use draw_types::*;
#[cfg(test)]
mod render_parity_tests;

// Backend-neutral frame lifetime: the frame fence, fence-stamped drop
// retirement and the texture pool. Each backend supplies small hooks
// (`RetiredResource`, `PoolAllocator`).
mod frame_fence;
pub use frame_fence::FrameFence;
// Vulkan stamps its drops once it submits (P1).
#[cfg_attr(vulkan_backend, allow(dead_code))]
mod retire;
pub use retire::{FrameClock, RetireMark, RetireQueue, RetireSender};
mod texture_pool;
pub use texture_pool::TexturePool;

/// Load the app's shader disk caches into `device`, so shaders an earlier
/// process compiled skip naga, spirv-opt and the driver compile. Every
/// process that builds pipelines calls it right after making its device:
/// the app, the headless tools and the GPU tests. Each backend keeps its own
/// files there: Metal a metallib archive and MSL, Vulkan a
/// `VkPipelineCache` blob and SPIR-V. No home directory, no caches.
pub fn load_disk_shader_caches(device: &GpuDevice) {
    let Some(dirs) = manifold_foundation::AppDirs::current() else {
        return;
    };
    let cache = dirs.cache;
    std::fs::create_dir_all(&cache).ok();
    device.load_pipeline_archive(&cache.join("pipeline_cache.metallib"));
    device.load_msl_cache(&cache.join("msl_cache"));
}
// Ray-tracing data types; each backend re-exports them from its `raytrace`.
// Vulkan uses them once it traces rays (P7).
#[cfg_attr(vulkan_backend, allow(dead_code))]
mod raytrace_types;
