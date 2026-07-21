//! Inspector dispatch handlers, split by domain (UI_FUNNEL_DECOMPOSITION P-B,
//! D6). Each module owns a disjoint slice of the inspector's `PanelAction`
//! variants, speaks today's `PanelAction`, and reads `ctx` fields directly.
//! `inspector::dispatch_inspector` is an ordered first-non-unhandled CHAIN over
//! these modules — NO per-variant delegation arm table (a misroute would hide
//! there). Bridge layer (D2): intent → `ContentCommand`/`EditingService`.

pub(crate) mod browser;
