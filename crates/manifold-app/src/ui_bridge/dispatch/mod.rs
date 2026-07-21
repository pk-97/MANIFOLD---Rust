//! Inspector dispatch handlers, split by domain (UI_FUNNEL_DECOMPOSITION P-B,
//! D6). Each module owns a disjoint slice of the inspector's `PanelAction`
//! variants, speaks today's `PanelAction`, and reads `ctx` fields directly.
//! Routing is the flat-sum match in `ui_bridge::dispatch` (P-D / D-D1): it
//! destructures `PanelAction` once and calls each `dispatch_<domain>` directly.
//! The former `inspector::dispatch_inspector` chain and its
//! `dispatch_chain_completeness` invariant were retired with the sum (an
//! exhaustive match proves routing totality). Bridge layer (D2): intent ->
//! `ContentCommand`/`EditingService`.

pub(crate) mod audio_setup;
pub(crate) mod browser;
pub(crate) mod clip;
pub(crate) mod mapping;
pub(crate) mod modulation;
pub(crate) mod params;
pub(crate) mod resolve;

// `resolve.rs`'s moved helpers reference `super::resolve_effect_id` /
// `super::resolve_active_layer_index` verbatim (their `super::` was written
// relative to `inspector.rs`'s nesting depth under `ui_bridge`, one level
// shallower than `resolve.rs`'s own depth under `ui_bridge::dispatch`). This
// re-export makes both names members of `dispatch` too, so the moved bodies
// resolve unchanged — a private `use`, visible to `dispatch` and its
// descendants (`resolve.rs`), needs no wider visibility. `params.rs` (S3)
// extends the same set: its moved arm bodies and D-11 preamble reference
// `super::editor_dispatch_context` / `super::resolve_effect_target` verbatim,
// and its own top-level `use super::{resolve_effects_mut,
// resolve_effects_read};` (mirroring inspector.rs's own import, one level
// deeper) needs those two re-exported here too.
use super::{
    editor_dispatch_context, resolve_active_layer_index, resolve_effect_id, resolve_effect_target,
    resolve_effects_mut, resolve_effects_read,
};

