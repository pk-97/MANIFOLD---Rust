//! Node graph engine for loading, execution and freeze compilation.
//! Owns graph types, GPU dispatch, shared node helpers and built-ins.
//! Never depends on node families, the catalog, compositor, UI paint,
//! UI, editing, IO, media or the app.

pub mod atomic;
pub mod bindings;
#[cfg(test)]
mod builtins;
#[doc = "Canonical channel-name registry for the Channel type system. The"]
#[doc = "`well_known_channels!` macro generates the constants and the"]
#[doc = "collision-check test from a single source list; see the module docs."]
pub mod channel_names;
pub mod clip_trigger;
pub mod content_revision;
pub mod descriptor;
pub mod exec;
pub mod freeze;
pub mod gpu;
pub mod graph;
pub mod load;
pub mod mesh;
pub mod palette;
pub mod param_binding;
pub mod param_doc;
mod param_tooltips_bulk;
mod param_tooltips_table;
pub mod parameters;
pub mod particles;
pub mod persistence;
pub mod platonic;
pub mod ports;
pub mod preview_encoding;
pub mod primitive;
pub mod primitives;
pub mod runtime;
pub mod scene;
pub mod snapshot;
pub mod state_store;
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
pub mod trigger_shadow_lint;
pub mod validate;
pub mod validation;
