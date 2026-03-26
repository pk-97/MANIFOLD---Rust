//! manifold-gpu — purpose-built GPU abstraction for MANIFOLD's content thread.
//!
//! Native Metal on macOS. Zero wgpu, zero vtable overhead.
//! All content-thread GPU resources are native Metal types.
//!
//! The UI thread stays on wgpu directly — this crate is only for the
//! content thread.

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
