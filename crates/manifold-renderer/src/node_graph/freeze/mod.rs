//! Freeze / fusion compiler (design: `docs/GRAPH_FREEZE_COMPILER_DESIGN.md`).
//!
//! Collapses chains of pure per-element GPU atoms into one kernel — read once,
//! math in registers, write once — to eliminate the VRAM round-trips that make
//! the node-graph runtime ~5× slower than the old fused-Rust effects (Phase 0
//! findings: `docs/GRAPH_FREEZE_PHASE0_FINDINGS.md`).
//!
//! Built bottom-up, oracle first (build sequence §9): the verification oracle
//! gates everything downstream, so it lands before the compiler it checks. The
//! oracle's foundation is [`diff`] — a GPU texture-diff reducer that compares
//! two renders (unfused = exact oracle, fused = candidate) to a tiny verdict.

pub mod classify;
pub mod codegen;
pub mod diff;
pub mod install;
pub mod perf_gate;
pub mod reference;
pub mod region;
pub mod space;

pub use classify::FusionKind;
pub use diff::{DiffResult, TextureDiff};

/// First end-to-end fusion proof — hand-fused Gain and ColorGrade chains
/// validated against the unfused chains through the oracle (correct fusion
/// clears, wrong fusion fails). Test-only; the eventual codegen reuses this
/// render-two-ways shape and the [`reference`] kernels as its targets.
#[cfg(test)]
mod proof;
