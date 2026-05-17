//! Bundled effect preset registry.
//!
//! Each shipping effect ships with one **bundled preset** — a JSON
//! [`EffectGraphDef`] living in
//! `crates/manifold-renderer/assets/effect-presets/<EffectTypeId>.json`.
//! The file is the on-disk source of truth; this module embeds the bytes
//! at compile time via [`include_str!`] and parses lazily on first
//! lookup.
//!
//! The bundled preset for `EffectTypeId::X` is the canonical default
//! graph for that effect. Today it equals the output of
//! `chain_spec_by_id(X).build_canonical_graph()` (verified by the
//! `bundled_presets_match_canonical_splices` test in
//! `tests/bundled_presets_drift.rs`); after the §6.6 cutover the JSON
//! file becomes authoritative and the splice fn-pointer is deleted.
//!
//! User-authored per-instance graphs are stored separately on the
//! [`EffectInstance`](manifold_core::effects::EffectInstance) (landing
//! in §6.6 item #27). Both shapes use the same [`EffectGraphDef`]
//! schema and the same [`PrimitiveRegistry`] loader; they differ only
//! in storage location.
//!
//! ## Add a new preset
//!
//! 1. Add a [`ChainSpec`](crate::node_graph::ChainSpec) submission via
//!    `inventory::submit!` (existing pattern).
//! 2. Regenerate the JSON file:
//!    `cargo test -p manifold-renderer --test bundled_presets_drift -- --ignored`.
//! 3. Add the new entry to [`BUNDLED_PRESETS`] below; without it the
//!    `every_chain_spec_has_a_bundled_preset` test fails.

use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;

/// Compile-time table: each shipping [`EffectTypeId`] mapped to the
/// embedded JSON for its bundled preset.
///
/// Order doesn't matter — the runtime cache is an [`AHashMap`]. Keep
/// entries alphabetical by `EffectTypeId.as_str()` for diff hygiene.
const BUNDLED_PRESETS: &[(&str, &str)] = &[
    (
        "AutoGain",
        include_str!("../../assets/effect-presets/AutoGain.json"),
    ),
    (
        "BlobTracking",
        include_str!("../../assets/effect-presets/BlobTracking.json"),
    ),
    (
        "Bloom",
        include_str!("../../assets/effect-presets/Bloom.json"),
    ),
    (
        "ChromaticAberration",
        include_str!("../../assets/effect-presets/ChromaticAberration.json"),
    ),
    (
        "ColorGrade",
        include_str!("../../assets/effect-presets/ColorGrade.json"),
    ),
    (
        "DepthOfField",
        include_str!("../../assets/effect-presets/DepthOfField.json"),
    ),
    (
        "Dither",
        include_str!("../../assets/effect-presets/Dither.json"),
    ),
    (
        "EdgeGlow",
        include_str!("../../assets/effect-presets/EdgeGlow.json"),
    ),
    (
        "EdgeStretch",
        include_str!("../../assets/effect-presets/EdgeStretch.json"),
    ),
    (
        "Glitch",
        include_str!("../../assets/effect-presets/Glitch.json"),
    ),
    (
        "Halation",
        include_str!("../../assets/effect-presets/Halation.json"),
    ),
    (
        "HdrBoost",
        include_str!("../../assets/effect-presets/HdrBoost.json"),
    ),
    (
        "Infrared",
        include_str!("../../assets/effect-presets/Infrared.json"),
    ),
    (
        "InvertColors",
        include_str!("../../assets/effect-presets/InvertColors.json"),
    ),
    (
        "Kaleidoscope",
        include_str!("../../assets/effect-presets/Kaleidoscope.json"),
    ),
    (
        "Mirror",
        include_str!("../../assets/effect-presets/Mirror.json"),
    ),
    (
        "NodeGraphTest",
        include_str!("../../assets/effect-presets/NodeGraphTest.json"),
    ),
    (
        "QuadMirror",
        include_str!("../../assets/effect-presets/QuadMirror.json"),
    ),
    (
        "SoftFocusGraph",
        include_str!("../../assets/effect-presets/SoftFocusGraph.json"),
    ),
    (
        "Strobe",
        include_str!("../../assets/effect-presets/Strobe.json"),
    ),
    (
        "StylizedFeedback",
        include_str!("../../assets/effect-presets/StylizedFeedback.json"),
    ),
    (
        "Transform",
        include_str!("../../assets/effect-presets/Transform.json"),
    ),
    (
        "VoronoiPrism",
        include_str!("../../assets/effect-presets/VoronoiPrism.json"),
    ),
    (
        "Watercolor",
        include_str!("../../assets/effect-presets/Watercolor.json"),
    ),
    (
        "WireframeDepth",
        include_str!("../../assets/effect-presets/WireframeDepth.json"),
    ),
];

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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::ChainSpec;
    use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use crate::node_graph::validation::validate;
    use crate::node_graph::execution_plan::compile;

    #[test]
    fn every_chain_spec_has_a_bundled_preset() {
        let mut missing: Vec<String> = Vec::new();
        for spec in inventory::iter::<ChainSpec> {
            if bundled_preset_def(&spec.type_id).is_none() {
                missing.push(spec.type_id.as_str().to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "ChainSpec(s) without a bundled preset entry — add them to BUNDLED_PRESETS \
             after regenerating with `cargo test -p manifold-renderer --test \
             bundled_presets_drift -- --ignored`: {missing:?}"
        );
    }

    #[test]
    fn every_bundled_preset_targets_a_real_chain_spec() {
        use crate::node_graph::chain_spec::chain_spec_by_id;
        let mut orphans: Vec<String> = Vec::new();
        for type_id in bundled_preset_type_ids() {
            if chain_spec_by_id(&type_id).is_none() {
                orphans.push(type_id.as_str().to_string());
            }
        }
        assert!(
            orphans.is_empty(),
            "Bundled preset(s) without a matching ChainSpec — remove from \
             BUNDLED_PRESETS and delete the JSON file: {orphans:?}"
        );
    }

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
