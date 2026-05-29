//! Consolidated parity-test binary.
//!
//! Each Phase-4a primitive migration ships a parity submodule that
//! asserts pixel-exact equality between the legacy `EffectChain` and
//! the new graph-runtime decomposition. Originally one Cargo
//! integration-test binary per effect, but `EffectRegistry::new` builds
//! every registered effect's pipelines (~5s on M-series) and Cargo
//! runs distinct test binaries serially — so 21 binaries cost ~125s
//! of harness init alone. Folding them into a single binary lets one
//! `harness::shared()` invocation amortize that cost across the whole
//! suite, and Cargo parallelizes within the binary.
//!
//! When adding a new effect parity test:
//!
//! 1. Drop a new file at `tests/parity/<effect>.rs`. Imports look like
//!    `use crate::harness::{self, Fixture, default_ctx, make_default_effect, ...};`
//!    and the test grabs the cached harness via `let h = harness::shared();`.
//! 2. Add a `mod <effect>;` line below.
//!
//! `tests/parity/sanity.rs` is the framework self-test (determinism)
//! and is the *only* file that constructs `ParityHarness` directly via
//! `ParityHarness::new()` — proving fresh instances stay byte-stable.

mod harness;

mod affine_transform;
mod bloom;
mod chromatic_offset;
mod clamp_stretch;
mod color_grade;
mod dither_pattern;
mod edge_detect;
mod glitch;
mod highlight_boost;
mod invert;
mod kaleido_fold;
mod lut1d;
mod sanity;
mod smoke;
mod strobe;
mod voronoi_prism;
mod watercolor;
mod wireframe_depth;
