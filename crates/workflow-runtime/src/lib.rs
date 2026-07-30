//! Semantic workflow program runner. Design contract: docs/WORKFLOW_RUNTIME_DESIGN.md.
//! The model is a stateless function `context -> typed artifact`; this crate owns
//! every side effect. Invariant (structural): no subprocess spawn outside
//! `gates`, `worktree`, `lane`, and `transport`'s keyget. The `lane` opcode
//! (D20) is the one sanctioned tool loop — every other opcode's model is
//! still a stateless `context -> typed artifact` call.

pub mod artifacts;
pub mod check;
pub mod cost;
pub mod gates;
pub mod lane;
pub mod ledger;
pub mod locate;
pub mod program;
pub mod runner;
pub mod scrub;
pub mod status;
pub mod template;
pub mod transport;
pub mod worktree;
