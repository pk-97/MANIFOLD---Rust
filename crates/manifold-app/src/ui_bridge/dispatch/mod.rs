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
pub(crate) mod presets;
pub(crate) mod resolve;

// Lets the dispatch submodules reach these ui_bridge helpers as `super::name`.
use super::{editor_active_layer, resolve_active_layer_index, resolve_effect_by_id};
