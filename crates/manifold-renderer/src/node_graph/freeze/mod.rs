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

pub mod diff;

pub use diff::{DiffResult, TextureDiff};
