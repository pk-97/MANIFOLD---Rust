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

pub mod types;
pub use types::*;

// Backend-neutral shader compilation pipeline (WGSL → naga → SPIR-V).
// Compiled on every platform; each backend's shader compiler consumes the
// optimised SPIR-V and emits platform-specific shader modules.
mod shader_common;

// ─── Backend selection ────────────────────────────────────────────────
//
// Exactly one backend is active per build:
//
// * Default on macOS: native Metal (`metal/` module). Zero overhead, full
//   feature set including MPSGraph FFT and MetalFX. This is the ship path.
//
// * With `--features vulkan`: Vulkan backend (`vulkan/` module). Used for
//   cross-platform builds (Windows/Linux) and for dev-box testing on
//   macOS via MoltenVK. Metal backend is not compiled in this mode.
//
// The two backends expose the same public surface (GpuDevice, GpuBuffer,
// GpuEncoder, etc.), so downstream crates and user code don't need to
// care which is active. Metal-only APIs that don't have a Vulkan
// equivalent (MetalFX upscaler, MPSGraph FFT) remain exposed only in the
// Metal build — callers that want them are already platform-gating.

#[cfg(not(feature = "vulkan"))]
mod metal;
#[cfg(not(feature = "vulkan"))]
pub use metal::*;

#[cfg(feature = "vulkan")]
mod vulkan;
#[cfg(feature = "vulkan")]
pub use vulkan::*;
