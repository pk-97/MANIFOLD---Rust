//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD.
//!
//! Native Metal on macOS. Zero vtable overhead.
//! All GPU resources are native Metal types.
//!
//! Used by both the content thread (rendering, effects, generators) and
//! the UI thread (native Metal UI rendering via GpuSurface).

pub mod types;
pub use types::*;

mod metal;
pub use metal::*;
