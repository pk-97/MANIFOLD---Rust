//! Semantic workflow program runner. Design contract: docs/WORKFLOW_RUNTIME_DESIGN.md.
//! The model is a stateless function `context -> typed artifact`; this crate owns
//! every side effect. Invariant (structural): no subprocess spawn outside `gates`.

pub mod artifacts;
pub mod gates;
pub mod program;
pub mod runner;
pub mod template;
pub mod transport;
