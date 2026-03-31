//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD.
//!
//! Native Metal on macOS. Zero wgpu, zero vtable overhead.
//! All GPU resources are native Metal types.
//!
//! Used by both the content thread (rendering, effects, generators) and
//! the UI thread (native Metal UI rendering via GpuSurface).

// objc macros (msg_send!, sel!, class!) must be imported at crate root.
// objc 0.2's sel_impl macro uses `cfg(feature = "cargo-clippy")` which
// triggers unexpected_cfgs warnings — suppress at crate level.
#![allow(unexpected_cfgs)]

#[macro_use]
extern crate objc;

pub mod types;
pub use types::*;

mod metal;
pub use metal::*;
