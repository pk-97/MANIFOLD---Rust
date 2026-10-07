pub mod clip_trigger;
pub mod line;
pub mod platonic;
pub mod stateful;
pub mod mesh;
pub mod atomic;
pub mod bindings;
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
pub mod graph;
pub mod palette;
pub mod param_binding;
pub mod param_doc;
mod param_tooltips_bulk;
mod param_tooltips_table;
pub mod parameters;
pub mod persistence;
pub mod ports;
pub mod preview_encoding;
pub mod primitive;
pub mod snapshot;
pub mod state_store;
pub mod trigger_shadow_lint;
pub mod validate;
pub mod validation;
pub mod particles;
pub mod runtime;
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
pub mod exec;
pub mod gpu;
pub mod load;
pub mod primitives;
pub mod scene;
pub mod water;
