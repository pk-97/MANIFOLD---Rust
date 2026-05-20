//! Bundled generator preset registry.
//!
//! Mirror of `node_graph::bundled_presets` (effect presets) for the
//! generator side. Each JSON file in `assets/generator-presets/*.json`
//! is picked up by `build.rs`, embedded via `include_str!`, and exposed
//! here as a lookup by `GeneratorTypeId`.
//!
//! The [`GeneratorRegistry`](crate::generators::registry::GeneratorRegistry)
//! consults this table when creating a generator: if an entry matches
//! the requested type id, the registry constructs a
//! [`JsonGraphGenerator`](crate::generators::json_graph_generator::JsonGraphGenerator)
//! from the embedded JSON; otherwise it falls back to the
//! `inventory::submit!` Rust factories.
//!
//! ## Add a new generator preset
//!
//! 1. Drop a JSON file at `assets/generator-presets/<TypeId>.json` —
//!    must reference `system.generator_input` + `system.final_output`
//!    boundary nodes (see [`crate::generators::json_graph_generator`]).
//! 2. Build — `build.rs` picks up the new file automatically and the
//!    preset becomes available in the picker on next launch.

use manifold_core::GeneratorTypeId;

// build.rs emits `BUNDLED_GENERATOR_PRESETS_GENERATED` — one entry per
// `assets/generator-presets/*.json`, sorted by filename stem.
include!(concat!(
    env!("OUT_DIR"),
    "/bundled_generator_presets_generated.rs"
));

/// Alias for the build.rs-generated array; stable name kept for
/// downstream consumers.
const BUNDLED_GENERATOR_PRESETS: &[(&str, &str)] = BUNDLED_GENERATOR_PRESETS_GENERATED;

/// Raw embedded JSON for the bundled generator preset of
/// `generator_type`, or `None` if no preset is registered for that id.
pub fn bundled_generator_preset_json(
    generator_type: &GeneratorTypeId,
) -> Option<&'static str> {
    BUNDLED_GENERATOR_PRESETS
        .iter()
        .find(|(id, _)| *id == generator_type.as_str())
        .map(|(_, json)| *json)
}

/// Every `GeneratorTypeId` that has a bundled JSON preset registered.
/// Iterated by the GeneratorRegistry at startup to discover all
/// JSON-defined generators alongside the inventory-registered Rust
/// generators.
pub fn bundled_generator_preset_type_ids() -> impl Iterator<Item = GeneratorTypeId> {
    BUNDLED_GENERATOR_PRESETS
        .iter()
        .map(|(id, _)| GeneratorTypeId::from_string((*id).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TrivialPassthrough + Plasma presets that ship today must be
    /// discoverable through this table. The `Plasma` entry binds to
    /// `GeneratorTypeId::PLASMA` (the legacy id) so it supersedes the
    /// Rust factory of the same id — renaming or removing it would
    /// silently revert every existing Plasma layer to the Rust path
    /// and break the editor's cog button on those layers.
    #[test]
    fn bundled_presets_include_shipping_generators() {
        let ids: Vec<String> = bundled_generator_preset_type_ids()
            .map(|t| t.as_str().to_string())
            .collect();
        assert!(
            ids.contains(&"TrivialPassthrough".to_string()),
            "TrivialPassthrough preset must ship — got {ids:?}",
        );
        assert!(
            ids.contains(&"Plasma".to_string()),
            "Plasma preset must ship under id `Plasma` to supersede the legacy Rust factory — got {ids:?}",
        );
    }

    /// Every embedded JSON must be parseable. Structural checks already
    /// happen in build.rs; this is the deeper schema check that the
    /// bytes round-trip through serde.
    #[test]
    fn every_bundled_generator_preset_parses() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        for (id, json) in BUNDLED_GENERATOR_PRESETS {
            let _: EffectGraphDef = serde_json::from_str(json).unwrap_or_else(|e| {
                panic!("bundled generator preset {id}: parse failed: {e}")
            });
        }
    }
}
