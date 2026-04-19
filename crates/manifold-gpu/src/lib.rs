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

mod metal;
pub use metal::*;
