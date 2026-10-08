//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

pub(crate) mod bundled_presets;
pub mod catalog_gen;






pub use bundled_presets::{
    bundled_preset_def, bundled_preset_json, bundled_preset_type_ids, loaded_presets_from_bundled,
    loaded_scene_modifier_presets_from_bundled,
};
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod image_tests;
