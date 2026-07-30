//! Semantic workflow program runner. Design contract: docs/WORKFLOW_RUNTIME_DESIGN.md.
//! The model is a stateless function `context -> typed artifact`; this crate owns
//! every side effect. Invariant (structural): no subprocess spawn outside
//! `gates`, `worktree`, and `transport`'s keyget.

pub mod artifacts;
pub mod check;
pub mod cost;
pub mod gates;
pub mod locate;
pub mod program;
pub mod runner;
pub mod scrub;
pub mod status;
pub mod template;
pub mod transport;
pub mod worktree;
