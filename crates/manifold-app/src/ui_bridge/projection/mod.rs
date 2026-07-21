//! Projection layer (UI_FUNNEL_DECOMPOSITION_DESIGN.md D1/D2): per-domain
//! `&Project` -> view-model modules, split from the former state_sync.rs god
//! file. Reads only; projection never sends commands (INV-G5).
pub(crate) mod cards;
pub(crate) mod inspector;
pub(crate) mod scene;
pub(crate) mod timeline;
pub(crate) mod transport;
