//! Chrome API — declarative widget/layout for chrome panels.
//!
//! A panel describes its UI once as a [`View`] tree; the runtime decides whether
//! that description is a fresh build or an in-place update and emits the minimal
//! [`UITree`](crate::tree::UITree) mutations. This removes the second, hand-
//! mirrored write (`build()` vs `sync_*()`) that drifts and dead-zones controls.
//!
//! Three pure layers, no new shared state, no engine dependency:
//!
//! - [`view`] — the immutable description ([`View`], fluent builders, intents,
//!   [`validate`]).
//! - [`layout`] — the pure mini-flexbox ([`solve`](layout::solve) → laid nodes).
//! - [`diff`] — the reconciler ([`ChromeHost`]: build / in-place update /
//!   needs-rebuild + intent population).
//!
//! See `docs/CHROME_API_DESIGN.md` for the full rationale and the Phase 2b
//! migration contract.

pub mod components;
pub mod diff;
pub mod layout;
pub mod theme;
pub mod view;

pub use diff::{materialize, ChromeHost, Reconcile};
pub use layout::{solve, solve_into, LaidNode};
pub use theme::Theme;
pub use view::{validate, Align, Layout, Pad, Sizing, SliderSpec, View, ViewIntent};
