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
//! graph for that effect. Post-§11 the JSON file is authoritative —
//! the chain runtime and editor snapshot both source bindings,
//! skip-mode, and topology from the embedded
//! [`PresetMetadata`](manifold_core::effect_graph_def::PresetMetadata)
//! block via [`crate::node_graph::LoadedPresetView`].
//!
//! User-authored per-instance graphs are stored separately on the
//! [`EffectInstance`](manifold_core::effects::EffectInstance). Both
//! shapes use the same [`EffectGraphDef`] schema and the same
//! [`PrimitiveRegistry`] loader; they differ only in storage location.
//!
//! ## Add a new preset
//!
//! 1. Drop a JSON file at `assets/effect-presets/<TypeId>.json` with
//!    a populated `presetMetadata` block (display name, params,
//!    bindings, skip mode).
//! 2. Build — `build.rs` picks up the new file automatically.

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
/// one (v2 schema). Every shipping bundled preset is v2 post-§11;
/// the `Option`-returning shape is retained so test-only or
/// hand-authored v1 fixtures stay loadable as graphs without
/// breaking the metadata projection.
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
