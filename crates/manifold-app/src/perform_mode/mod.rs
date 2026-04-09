//! Performance mode — minimal main-window HUD for live performance.
//!
//! When perform mode is active, the main window's normal UI is replaced
//! with a single high-readability screen designed to be visible at a
//! glance during a live show. The content thread and the audience-facing
//! output window are completely untouched — perform mode only swaps the
//! main window's tick/render path.
//!
//! Module layout:
//! - [`state`] — `PerformModeState` struct (one field on `Application`).
//! - [`render`] — main window render path (`tick_perform_mode`).
//! - [`lifecycle`] — entry / exit deferred-action handlers.
//! - [`input`] — input gating helpers called from `app.rs` event handlers.
//!
//! See `CLAUDE.md` and the `feedback_*` memories for the safety rules:
//! - Content thread is never touched.
//! - Triple-redundant exit detection (button + Escape + output-window
//!   close hooks + per-frame poll).
//! - Quiesce in-flight UI state on entry; force full rebuild on exit.

pub(crate) mod cue;
pub(crate) mod input;
pub(crate) mod lifecycle;
pub(crate) mod render;
pub(crate) mod state;

pub(crate) use state::PerformModeState;
