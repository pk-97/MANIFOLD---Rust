//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD's content thread.
//!
//! Provides a thin, zero-overhead GPU API for the content thread hot path.
//! Metal backend on macOS (raw metal:: crate, zero wgpu), wgpu backend on
//! Windows/Linux.
//!
//! Compile-time backend selection via `#[cfg]` — zero vtable overhead.
//! Both backends export the same type names: `GpuDevice`, `GpuEncoder`,
//! `GpuTexture`, `GpuBuffer`, `GpuComputePipeline`, `GpuRenderPipeline`,
//! `GpuSampler`, `GpuEvent`.
//!
//! The UI thread stays on wgpu directly — this crate is only for the
//! content thread.

pub mod types;
pub use types::*;

#[cfg(target_os = "macos")]
mod metal;
#[cfg(target_os = "macos")]
pub use metal::*;

#[cfg(not(target_os = "macos"))]
mod wgpu_backend;
#[cfg(not(target_os = "macos"))]
pub use wgpu_backend::*;
