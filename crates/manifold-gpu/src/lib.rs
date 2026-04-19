//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD.
//!
//! Native Metal on macOS. Zero vtable overhead.
//! All GPU resources are native Metal types.
//!
//! Used by both the content thread (rendering, effects, generators) and
//! the UI thread (native Metal UI rendering via GpuSurface).

// objc2-metal marks most method wrappers `unsafe fn` but some are safe
// accessors (e.g. `label()`, `signaledValue()`). We wrap ambiguous calls in
// unsafe blocks uniformly rather than tracking each method's safety status,
// so the redundant-unsafe lint fires in places where the compiler later
// decides the call was safe. Silence it crate-wide.
#![allow(unused_unsafe)]

pub mod types;
pub use types::*;

mod metal;
pub use metal::*;
