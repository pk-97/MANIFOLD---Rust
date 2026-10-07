pub mod clip_trigger;
pub mod line;
pub mod platonic;
pub mod stateful;
pub mod mesh;
pub mod atomic;
mod bindings;
#[cfg(test)]
mod builtins;
#[doc = "Canonical channel-name registry for the Channel type system. The"]
#[doc = "`well_known_channels!` macro generates the constants and the"]
#[doc = "collision-check test from a single source list; see the module"]
#[doc = "docs and `docs/CHANNEL_TYPE_SYSTEM.md` section 7."]
pub mod channel_names;
pub mod content_revision;
pub mod descriptor;
pub mod freeze;
mod graph;
mod palette;
mod param_binding;
pub mod param_doc;
mod param_tooltips_bulk;
mod param_tooltips_table;
pub(crate) mod parameters;
pub(crate) mod persistence;
pub mod ports;
pub mod preview_encoding;
pub mod primitive;
mod snapshot;
mod state_store;
pub mod trigger_shadow_lint;
pub mod validate;
mod validation;
pub mod particles;
pub mod runtime;
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
pub mod exec;
pub mod gpu;
pub mod load;
pub mod primitives;
pub mod scene;
pub mod water;
