//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD's content thread.
//!
//! Native Metal on macOS. Zero wgpu, zero vtable overhead.
//! All content-thread GPU resources are native Metal types.
//!
//! The UI thread stays on wgpu directly — this crate is only for the
//! content thread.

pub mod types;
pub use types::*;

mod metal;
pub use metal::*;
