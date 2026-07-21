//! Frame stages (UI_FUNNEL_DECOMPOSITION D7). Only `present` is split so far;
//! drain/events/sync/push remain inline in tick_and_render (parked).
pub(crate) mod present;
