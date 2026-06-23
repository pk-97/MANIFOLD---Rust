//! Unit-typed wrappers for beat and time values.
//!
//! These moved to the zero-dependency `manifold-foundation` crate so the UI can
//! share them without depending on the engine (see
//! `docs/UI_LAYERING_INVERSION.md`). This module re-exports them at their
//! historical path: every `manifold_core::units::*` and `manifold_core::Beats`
//! usage is unchanged, and project-file serialization is byte-identical.

pub use manifold_foundation::units::*;
