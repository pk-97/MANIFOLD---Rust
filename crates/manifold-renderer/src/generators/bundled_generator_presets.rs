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
use manifold_core::effect_graph_def::EffectGraphDef;

// build.rs emits `BUNDLED_GENERATOR_PRESETS_GENERATED` — one entry per
// `assets/generator-presets/*.json`, sorted by filename stem.
include!(concat!(
    env!("OUT_DIR"),
    "/bundled_generator_presets_generated.rs"
));

/// Alias for the build.rs-generated array; stable name kept for
/// downstream consumers.
const BUNDLED_GENERATOR_PRESETS: &[(&str, &str)] = BUNDLED_GENERATOR_PRESETS_GENERATED;

/// Loader function for the core's
/// [`manifold_core::generator_definition_registry::LoadedPresetSource`]
/// inventory. Walks the bundled preset table, parses each JSON document,
/// and returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). Mirrors `loaded_presets_from_bundled` on the
/// effect side.
///
/// Cached at the `loaded_preset_metadata()` callsite — invoked once per
/// process. The §11 generator unification means a JSON preset's
/// `presetMetadata` block IS the canonical schema for that generator,
/// and the legacy inventory submission (if any) is overridden.
pub fn loaded_generator_presets_from_bundled()
-> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    BUNDLED_GENERATOR_PRESETS
        .iter()
        .filter_map(|(id, json)| {
            let def: EffectGraphDef = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("bundled generator preset {id}: parse failed: {e}"));
            def.preset_metadata
        })
        .collect()
}

inventory::submit! {
    manifold_core::generator_definition_registry::LoadedPresetSource {
        load: loaded_generator_presets_from_bundled,
    }
}

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

    /// Class-level guard for the "Lissajous's clip-trigger toggle
    /// only drove mux_x, not mux_y" bug. Every binding in every
    /// bundled preset must reference an outer-card slider that
    /// actually exists — the `id` shared between [`BindingDef::id`]
    /// and [`ParamSpecDef::id`] is the rendezvous point.
    ///
    /// Why this matters as a sweep test (vs. a per-preset assertion):
    /// the bug class is "preset author adds a fan-out binding +
    /// forgets the matching outer slider, OR typos the id". The
    /// runtime degrades gracefully (warn + drop) but the symptom is
    /// silent — the inner param sits forever on the binding's
    /// `default_value`. CI catching it before merge is the only
    /// safety net that scales to N future presets.
    #[test]
    fn every_bundled_preset_binding_resolves_to_an_outer_param() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        let mut violations: Vec<String> = Vec::new();
        for (preset_id, json) in BUNDLED_GENERATOR_PRESETS {
            let doc: EffectGraphDef = serde_json::from_str(json).unwrap_or_else(|e| {
                panic!("bundled preset {preset_id}: parse failed: {e}")
            });
            let Some(meta) = doc.preset_metadata.as_ref() else {
                continue; // legacy preset without metadata — no bindings to validate
            };
            let param_ids: std::collections::HashSet<&str> =
                meta.params.iter().map(|p| p.id.as_str()).collect();
            for binding in &meta.bindings {
                if !param_ids.contains(binding.id.as_str()) {
                    violations.push(format!(
                        "preset `{preset_id}`: binding id=`{}` (target {:?}) does not match \
                         any outer-card param id. Either add a `params` entry with \
                         id=`{}` or remove the binding — otherwise it will silently \
                         pin its inner target at default_value={} on every frame.",
                        binding.id, binding.target, binding.id, binding.default_value,
                    ));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "Bundled preset bindings reference nonexistent outer params:\n  - {}",
            violations.join("\n  - "),
        );
    }

    /// Sweep guard: every bundled preset must chain-build cleanly.
    /// Parse + binding-resolution pass already cover the schema; this
    /// catches the deeper failure modes that only the chain builder
    /// notices — unknown `typeId`, port-type mismatches on wires,
    /// missing required inputs, capacity-derivation cycles, output-
    /// slot-sizing failures.
    ///
    /// A new preset that lands in `assets/generator-presets/` and
    /// fails chain build would render black at runtime with just a
    /// warning in the log. This test catches it at compile time.
    #[test]
    fn every_bundled_preset_chain_builds() {
        use crate::generators::json_graph_generator::JsonGraphGenerator;
        use crate::node_graph::PrimitiveRegistry;
        use manifold_gpu::GpuTextureFormat;
        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let mut failures: Vec<String> = Vec::new();
        for (preset_id, json) in BUNDLED_GENERATOR_PRESETS {
            if let Err(e) = JsonGraphGenerator::from_json_str_with_device(
                json,
                &registry,
                &device,
                1920,
                1080,
                GpuTextureFormat::Rgba16Float,
            ) {
                failures.push(format!("{preset_id}: {e}"));
            }
        }
        assert!(
            failures.is_empty(),
            "Bundled presets failed chain build:\n  - {}",
            failures.join("\n  - "),
        );
    }
}
