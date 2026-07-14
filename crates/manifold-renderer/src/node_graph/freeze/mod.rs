//! Freeze / fusion compiler — the production compiler on the live render
//! path (design: `docs/GRAPH_FREEZE_COMPILER_DESIGN.md`; current-state map:
//! `docs/FREEZE_COMPILER_MAP.md`, the authoritative reference for this
//! module).
//!
//! Collapses maximal runs of pure per-element GPU atoms into one generated
//! WGSL kernel per region — read once, math in registers, write once — to
//! eliminate the VRAM round-trips that make the naive node-graph runtime ~5×
//! slower than the old hand-fused Rust effects (Phase 0 findings:
//! `docs/archive/GRAPH_FREEZE_PHASE0_FINDINGS.md`). [`classify`] tags every
//! primitive's fusion eligibility; [`region`] partitions a graph into maximal
//! fusable regions (texture and buffer domains); [`codegen`] and [`install`]
//! generate the fused kernel per region and swap it into the graph as one
//! `node.wgsl_compute` node; [`segment`]/[`space`] handle cross-card
//! concatenation and element-space bookkeeping. [`diff`] is the foundational
//! verification primitive underneath all of it — a GPU texture-diff reducer
//! that compares two renders (unfused = free exact oracle, fused = candidate)
//! to a tiny verdict — and backs both the `proof` test suite and the fuzzers.

pub mod classify;
pub mod codegen;
pub mod derived_uniform_registry;
pub mod diff;
pub mod fusion_report;
pub mod install;
pub mod reference;
pub mod region;
pub mod segment;
pub mod space;

pub use classify::FusionKind;
pub use diff::{DiffResult, TextureDiff};
pub use fusion_report::{FusionReport, NodeFusionInfo, RegionSummary, fusion_report};

/// First end-to-end fusion proof — hand-fused Gain and ColorGrade chains
/// validated against the unfused chains through the oracle (correct fusion
/// clears, wrong fusion fails). Test-only; the eventual codegen reuses this
/// render-two-ways shape and the [`reference`] kernels as its targets.
#[cfg(all(test, feature = "gpu-proofs"))]
mod proof;
