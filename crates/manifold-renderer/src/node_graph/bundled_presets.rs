//! Bundled effect preset registry.
//!
//! Each shipping effect ships with one **bundled preset** — a JSON
//! [`EffectGraphDef`] living in
//! `crates/manifold-renderer/assets/effect-presets/<EffectTypeId>.json`.
//! The file is the on-disk source of truth; `build.rs` scans the
//! directory at compile time and emits the
//! [`BUNDLED_PRESETS`](BUNDLED_PRESETS) array of
//! `(type_id, include_str!(json))` pairs. Adding a preset is just
//! dropping a JSON file in the directory — no hand-maintained table,
//! no central registration to forget.
//!
//! The bundled preset for `EffectTypeId::X` is the canonical default
//! graph for that effect. Today it equals the output of
//! `chain_spec_by_id(X).build_canonical_graph()` (verified by the
//! `bundled_presets_match_canonical_splices` test in
//! `tests/bundled_presets_drift.rs`); after the §11 cutover (full
//! plan in `docs/PRIMITIVE_LIBRARY_DESIGN.md`) the JSON file becomes
//! authoritative and the splice fn-pointer is deleted.
//!
//! User-authored per-instance graphs are stored separately on the
//! [`EffectInstance`](manifold_core::effects::EffectInstance). Both
//! shapes use the same [`EffectGraphDef`] schema and the same
//! [`PrimitiveRegistry`] loader; they differ only in storage location.
//!
//! ## Add a new preset
//!
//! 1. Drop a JSON file at `assets/effect-presets/<TypeId>.json`.
//! 2. (Until §11 block 4 lands) Add the matching
//!    [`ChainSpec`](crate::node_graph::ChainSpec) submission via
//!    `inventory::submit!` and run
//!    `cargo test -p manifold-renderer --test bundled_presets_drift -- --ignored`
//!    to regenerate the canonical JSON.
//! 3. Build — `build.rs` picks up the new file automatically.

use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;

// build.rs emits the `BUNDLED_PRESETS_GENERATED` array — one entry
// per `assets/effect-presets/*.json`, sorted alphabetically, with each
// JSON embedded via `include_str!`.
include!(concat!(env!("OUT_DIR"), "/bundled_presets_generated.rs"));

/// Bundled preset table — alias for the build.rs-generated array.
/// Stable name kept for the existing test/consumer code.
const BUNDLED_PRESETS: &[(&str, &str)] = BUNDLED_PRESETS_GENERATED;

/// Raw embedded JSON for the bundled preset of `effect_type`, or
/// `None` if no preset is registered.
///
/// The string is the on-disk file verbatim — same bytes the drift
/// test compares against. Useful when a caller wants to re-export the
/// preset (e.g., copy-on-write into a per-instance override).
pub fn bundled_preset_json(effect_type: &EffectTypeId) -> Option<&'static str> {
    BUNDLED_PRESETS
        .iter()
        .find(|(id, _)| *id == effect_type.as_str())
        .map(|(_, json)| *json)
}

/// Parsed [`EffectGraphDef`] for the bundled preset of `effect_type`,
/// or `None` if no preset is registered.
///
/// First call lazily parses every bundled JSON into a cached
/// [`AHashMap`]; subsequent calls return a borrowed reference into
/// that cache. Parsing happens once per process.
///
/// Parse failures panic with the effect type id and underlying error
/// — these come from files we author, so any failure is a developer
/// mistake to fix, not a runtime condition to handle.
pub fn bundled_preset_def(effect_type: &EffectTypeId) -> Option<&'static EffectGraphDef> {
    static CACHE: OnceLock<AHashMap<&'static str, EffectGraphDef>> = OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let mut m: AHashMap<&'static str, EffectGraphDef> = AHashMap::default();
        for (id, json) in BUNDLED_PRESETS {
            let def: EffectGraphDef = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("bundled preset {id}: parse failed: {e}"));
            m.insert(id, def);
        }
        m
    });
    map.get(effect_type.as_str())
}

/// Every [`EffectTypeId`] that has a bundled preset registered.
pub fn bundled_preset_type_ids() -> impl Iterator<Item = EffectTypeId> {
    BUNDLED_PRESETS
        .iter()
        .map(|(id, _)| EffectTypeId::new(id))
}

/// Loader function for the core's [`LoadedPresetSource`] inventory.
/// Walks the bundled preset table, parses each JSON document, and
/// returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). v1 entries (the 27 shipping presets prior to the
/// §11 block-4 migration) yield nothing — block 4 populates metadata
/// one effect at a time, and this iterator widens as those land.
///
/// Cached at the `loaded_preset_metadata()` callsite — invoked once
/// per process.
pub fn loaded_presets_from_bundled() -> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    BUNDLED_PRESETS
        .iter()
        .filter_map(|(id, json)| {
            let def: EffectGraphDef = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("bundled preset {id}: parse failed: {e}"));
            def.preset_metadata
        })
        .collect()
}

inventory::submit! {
    manifold_core::effect_definition_registry::LoadedPresetSource {
        load: loaded_presets_from_bundled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use crate::node_graph::validation::validate;
    use crate::node_graph::execution_plan::compile;

    #[test]
    fn every_bundled_preset_loads_validates_and_compiles() {
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids() {
            let def = bundled_preset_def(&type_id)
                .expect("registered preset must have a parsed def")
                .clone();
            let graph = def.into_graph(&registry).unwrap_or_else(|e| {
                panic!("bundled preset {}: into_graph failed: {e}", type_id.as_str())
            });
            validate(&graph).unwrap_or_else(|e| {
                panic!("bundled preset {}: validate failed: {e:?}", type_id.as_str())
            });
            compile(&graph).unwrap_or_else(|e| {
                panic!("bundled preset {}: compile failed: {e:?}", type_id.as_str())
            });
        }
    }

    #[test]
    fn bundled_preset_json_returns_embedded_bytes() {
        let raw = bundled_preset_json(&EffectTypeId::MIRROR).expect("Mirror preset registered");
        // Sanity: the embedded JSON must parse as a valid def and name itself "Mirror".
        let def: EffectGraphDef = serde_json::from_str(raw).expect("Mirror preset parses");
        assert_eq!(def.name.as_deref(), Some("Mirror"));
    }

    #[test]
    fn bundled_preset_lookup_returns_none_for_unknown_type() {
        let unknown = EffectTypeId::new("DefinitelyNotARealEffect");
        assert!(bundled_preset_def(&unknown).is_none());
        assert!(bundled_preset_json(&unknown).is_none());
    }

    /// §11 block 4 parity invariant: for every effect whose JSON file
    /// carries `presetMetadata`, the EffectDef built from that
    /// metadata must match the EffectDef built from the same effect's
    /// `inventory::submit!(EffectMetadata)` block. This is the test
    /// that catches drift during per-effect migration — if any
    /// metadata field gets transcribed wrong from Rust to JSON, the
    /// dual-source registry would silently start serving different
    /// answers to OSC/MIDI/UI consumers.
    ///
    /// As blocks 4b-4z migrate each effect, this test's coverage
    /// widens automatically (it walks every JSON file with metadata).
    #[test]
    fn json_metadata_matches_inventory_for_every_migrated_effect() {
        use manifold_core::effect_definition_registry::preset_metadata_to_effect_def;
        use manifold_core::effect_registration::EffectMetadata;

        // Build a map of inventory-submitted EffectMetadata by id so
        // we can compare each migrated JSON against its peer.
        let mut inventory_by_id: AHashMap<&'static str, &'static EffectMetadata> =
            AHashMap::default();
        for meta in inventory::iter::<EffectMetadata> {
            inventory_by_id.insert(meta.id.as_str(), meta);
        }

        let mut compared = 0usize;
        for (id, json) in BUNDLED_PRESETS {
            let def: EffectGraphDef = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("preset {id}: parse failed: {e}"));
            let Some(preset_meta) = def.preset_metadata else {
                continue; // v1 entry — not yet migrated
            };

            let inv_meta = inventory_by_id.get(id).unwrap_or_else(|| {
                panic!(
                    "preset {id} has presetMetadata in JSON but no matching \
                     inventory::submit!(EffectMetadata) — either migrate now or \
                     remove the JSON metadata until block 8 deletes the inventory entry"
                )
            });
            let inv_def = inv_meta.to_effect_def();
            let json_def = preset_metadata_to_effect_def(&preset_meta);

            assert_eq!(
                inv_def.display_name, json_def.display_name,
                "{id}: display_name drift"
            );
            assert_eq!(inv_def.osc_prefix, json_def.osc_prefix, "{id}: osc_prefix drift");
            assert_eq!(
                inv_def.param_count, json_def.param_count,
                "{id}: param_count drift"
            );
            assert_eq!(
                inv_def.param_defs.len(),
                json_def.param_defs.len(),
                "{id}: param_defs.len drift"
            );
            for (i, (a, b)) in inv_def
                .param_defs
                .iter()
                .zip(json_def.param_defs.iter())
                .enumerate()
            {
                assert_eq!(a.id, b.id, "{id}.params[{i}].id");
                assert_eq!(a.name, b.name, "{id}.params[{i}].name");
                assert!((a.min - b.min).abs() < 1e-6, "{id}.params[{i}].min");
                assert!((a.max - b.max).abs() < 1e-6, "{id}.params[{i}].max");
                assert!(
                    (a.default_value - b.default_value).abs() < 1e-6,
                    "{id}.params[{i}].default_value"
                );
                assert_eq!(a.whole_numbers, b.whole_numbers, "{id}.params[{i}].whole_numbers");
                assert_eq!(a.is_toggle, b.is_toggle, "{id}.params[{i}].is_toggle");
                assert_eq!(a.format_string, b.format_string, "{id}.params[{i}].format_string");
                assert_eq!(a.osc_suffix, b.osc_suffix, "{id}.params[{i}].osc_suffix");
            }
            assert_eq!(
                inv_def.id_to_index, json_def.id_to_index,
                "{id}: id_to_index drift"
            );
            assert_eq!(inv_def.param_ids, json_def.param_ids, "{id}: param_ids drift");

            compared += 1;
        }

        // Sanity: at least one effect has been migrated. After block
        // 4b (Bloom) lands, this is > 0; after block 4z, it's the
        // full shipping count.
        assert!(
            compared > 0,
            "no migrated effects found — block 4 should have at least one JSON \
             file with presetMetadata"
        );
    }

    /// Splicing a bundled preset into a chain via
    /// `splice_def_into_chain` is the path the runtime takes when
    /// `EffectInstance.graph = Some(def)`. Verifies every shipping
    /// preset survives that round-trip — the same data the drift test
    /// covers at the standalone-graph level, exercised against the
    /// chain-grafting code that the runtime actually calls.
    #[test]
    fn every_bundled_preset_splices_into_a_chain() {
        use crate::node_graph::boundary_nodes::Source;
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::graph::Graph;

        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids() {
            let def = bundled_preset_def(&type_id).expect("registered");
            let mut chain = Graph::new();
            let src = chain.add_node(Box::new(Source::new()));
            let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry);
            assert!(
                result.is_some(),
                "bundled preset {} failed to splice into a chain — preset and chain runtime have \
                 drifted apart",
                type_id.as_str(),
            );
        }
    }
}
