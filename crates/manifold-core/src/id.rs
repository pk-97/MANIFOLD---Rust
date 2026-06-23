//! Typed identifiers for compile-time safety.
//!
//! These moved to the zero-dependency `manifold-foundation` crate so the UI can
//! share them without depending on the engine (see
//! `docs/UI_LAYERING_INVERSION.md`). This module re-exports them at their
//! historical path: every `manifold_core::id::*` and `manifold_core::LayerId`
//! usage is unchanged, and project-file serialization is byte-identical.

pub use manifold_foundation::id::*;
