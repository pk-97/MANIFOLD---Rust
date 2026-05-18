//! Runtime view of a JSON-loaded preset — the §11 replacement for
//! [`crate::node_graph::chain_spec::ChainSpec`].
//!
//! A [`LoadedPresetView`] pairs the canonical [`EffectGraphDef`] (from
//! `assets/effect-presets/<TypeId>.json`) with the renderer-side
//! runtime types ([`ParamBinding`], [`SkipMode`]) reconstructed from
//! the JSON's `presetMetadata` block. It carries everything the chain
//! builder needs to graft an effect's worker subgraph into a chain
//! and to wire up parameter routing — exactly the same surface
//! [`ChainSpec`] provides, sourced from JSON instead of an inventory
//! submission.
//!
//! Today this module is **parallel infrastructure**: views are built
//! lazily on demand and cached, but the chain runtime
//! ([`crate::effect_chain_graph`]) still consults [`chain_spec_by_id`]
//! for bindings + skip. Block 6b/6c rewires the chain build loop to
//! use [`loaded_preset_view_by_id`] instead; block 8 deletes the
//! inventory `ChainSpec` submissions once that switch is complete.
//!
//! ## Lifecycle
//!
//! Views are built once on first lookup per effect id, leaking owned
//! `String`s into `&'static str` so the resulting [`ParamBinding`] and
//! [`SkipMode`] match the lifetime contract the renderer-side types
//! already use. The leak is bounded — at most one view per shipping
//! effect, ~30 strings each — and amortised over the process lifetime.
//! Same pattern as
//! [`crate::node_graph::persistence::PrimitiveRegistry`] builds its
//! constructor map.

use std::borrow::Cow;
use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, PresetMetadata, SkipModeDef,
};
use manifold_core::effects::ParamConvert;

use crate::node_graph::bundled_presets::bundled_preset_def;
use crate::node_graph::chain_spec::SkipMode;
use crate::node_graph::param_binding::{ParamBinding, ParamId, ParamTarget};

/// Runtime view assembled from a JSON-loaded preset. Same field shape
/// as [`crate::node_graph::ChainSpec`] minus the static `splice` fn —
/// the chain builder uses
/// [`crate::node_graph::splice_def_into_chain`] with `canonical_def`
/// to produce equivalent worker nodes.
#[allow(dead_code)] // §11 block 6a: parallel infrastructure, consumer wired in 6c.
pub struct LoadedPresetView {
    pub type_id: EffectTypeId,
    /// The canonical default graph for this effect, loaded from
    /// `assets/effect-presets/<id>.json`. Identical to what the
    /// existing `ChainSpec::build_canonical_graph()` produces today —
    /// drift would be caught by the
    /// `bundled_presets_match_canonical_splices` test.
    pub canonical_def: &'static EffectGraphDef,
    /// Outer-card slider bindings, with all string fields converted
    /// from owned to `&'static str` via [`Box::leak`].
    pub bindings: &'static [ParamBinding],
    pub skip_mode: SkipMode,
}

/// Lookup a [`LoadedPresetView`] by effect type id, building it on
/// first call and caching for the process lifetime. Returns `None`
/// for effects whose JSON file doesn't carry `presetMetadata` (i.e.,
/// v1 entries — not yet migrated by §11 block 4).
#[allow(dead_code)] // §11 block 6a: parallel infrastructure, consumer wired in 6c.
pub fn loaded_preset_view_by_id(id: &EffectTypeId) -> Option<&'static LoadedPresetView> {
    static MAP: OnceLock<AHashMap<EffectTypeId, &'static LoadedPresetView>> = OnceLock::new();
    let map = MAP.get_or_init(build_view_map);
    map.get(id).copied()
}

fn build_view_map() -> AHashMap<EffectTypeId, &'static LoadedPresetView> {
    let mut m: AHashMap<EffectTypeId, &'static LoadedPresetView> = AHashMap::default();
    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids() {
        if let Some(view) = build_view(&type_id) {
            m.insert(type_id, Box::leak(Box::new(view)));
        }
    }
    m
}

fn build_view(type_id: &EffectTypeId) -> Option<LoadedPresetView> {
    let def = bundled_preset_def(type_id)?;
    let metadata = def.preset_metadata.as_ref()?;
    Some(LoadedPresetView {
        type_id: type_id.clone(),
        canonical_def: def,
        bindings: leak_bindings(metadata),
        skip_mode: skip_mode_from_def(&metadata.skip_mode),
    })
}

fn leak_bindings(meta: &PresetMetadata) -> &'static [ParamBinding] {
    let owned: Vec<ParamBinding> = meta.bindings.iter().map(binding_def_to_runtime).collect();
    Box::leak(owned.into_boxed_slice())
}

fn binding_def_to_runtime(def: &BindingDef) -> ParamBinding {
    let label: &'static str = Box::leak(def.label.clone().into_boxed_str());
    ParamBinding {
        id: ParamId::Owned(def.id.clone()),
        label,
        default_value: def.default_value,
        target: target_def_to_runtime(&def.target),
        convert: def.convert,
    }
}

fn target_def_to_runtime(def: &BindingTarget) -> ParamTarget {
    match def {
        BindingTarget::HandleNode { handle, param } => {
            let handle: &'static str = Box::leak(handle.clone().into_boxed_str());
            let param: &'static str = Box::leak(param.clone().into_boxed_str());
            ParamTarget::HandleNode { handle, param }
        }
        BindingTarget::Composite { outer_name } => ParamTarget::Composite {
            outer_name: Cow::Owned(outer_name.clone()),
        },
    }
}

fn skip_mode_from_def(def: &SkipModeDef) -> SkipMode {
    match def {
        SkipModeDef::Never => SkipMode::Never,
        SkipModeDef::OnZero { param_id } => {
            let leaked: &'static str = Box::leak(param_id.clone().into_boxed_str());
            SkipMode::OnZero { param_id: leaked }
        }
    }
}

// Silence unused-warnings during the parallel-infrastructure phase
// — ParamConvert is re-exported for symmetry with the runtime
// param_binding module but isn't yet consumed by an external caller.
#[allow(dead_code)]
fn _phase_keepalive(_: ParamConvert) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::chain_spec::chain_spec_by_id;

    /// §11 block 6a parity invariant: for every effect that has both a
    /// `ChainSpec` (inventory) and a `LoadedPresetView` (JSON), the
    /// bindings + skip_mode must agree. This is the safety net for the
    /// upcoming block 6c rewire of the chain runtime.
    #[test]
    fn loaded_preset_view_matches_chain_spec_for_every_migrated_effect() {
        let mut compared = 0usize;
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids() {
            let Some(view) = loaded_preset_view_by_id(&type_id) else {
                continue; // not yet migrated to v2 metadata
            };
            let Some(spec) = chain_spec_by_id(&type_id) else {
                panic!(
                    "{}: LoadedPresetView exists but no ChainSpec — block 8 should \
                     delete inventory submissions only after block 6c rewires the chain runtime",
                    type_id.as_str()
                );
            };

            assert_eq!(
                view.bindings.len(),
                spec.bindings.len(),
                "{}: bindings count drift",
                type_id.as_str()
            );
            for (i, (a, b)) in view.bindings.iter().zip(spec.bindings.iter()).enumerate() {
                assert_eq!(
                    a.id.as_ref(),
                    b.id.as_ref(),
                    "{}.bindings[{i}].id drift",
                    type_id.as_str()
                );
                assert_eq!(
                    a.label,
                    b.label,
                    "{}.bindings[{i}].label drift",
                    type_id.as_str()
                );
                assert!(
                    (a.default_value - b.default_value).abs() < 1e-6,
                    "{}.bindings[{i}].default_value drift ({} vs {})",
                    type_id.as_str(),
                    a.default_value,
                    b.default_value,
                );
                match (&a.target, &b.target) {
                    (
                        ParamTarget::HandleNode { handle: ah, param: ap },
                        ParamTarget::HandleNode { handle: bh, param: bp },
                    ) => {
                        assert_eq!(ah, bh, "{}.bindings[{i}].target.handle", type_id.as_str());
                        assert_eq!(ap, bp, "{}.bindings[{i}].target.param", type_id.as_str());
                    }
                    (
                        ParamTarget::Composite { outer_name: an },
                        ParamTarget::Composite { outer_name: bn },
                    ) => {
                        assert_eq!(
                            an.as_ref(),
                            bn.as_ref(),
                            "{}.bindings[{i}].target.outer_name",
                            type_id.as_str()
                        );
                    }
                    (a, b) => panic!(
                        "{}.bindings[{i}].target variant drift: view={a:?} spec={b:?}",
                        type_id.as_str()
                    ),
                }
                assert_eq!(
                    a.convert, b.convert,
                    "{}.bindings[{i}].convert drift",
                    type_id.as_str()
                );
            }

            // Skip mode parity.
            match (&view.skip_mode, &spec.skip) {
                (SkipMode::Never, SkipMode::Never) => {}
                (SkipMode::OnZero { param_id: a }, SkipMode::OnZero { param_id: b }) => {
                    assert_eq!(a, b, "{}: skip_mode.param_id drift", type_id.as_str());
                }
                (a, b) => panic!(
                    "{}: skip_mode variant drift: view={a:?} spec={b:?}",
                    type_id.as_str()
                ),
            }

            compared += 1;
        }
        assert!(
            compared > 0,
            "no migrated effects found — block 4 should have populated every JSON's presetMetadata"
        );
    }

    /// Sanity: looking up an unknown id returns None, not a panic or
    /// stale result.
    #[test]
    fn loaded_preset_view_returns_none_for_unknown_id() {
        let unknown = EffectTypeId::from_string("NotARealEffect".to_string());
        assert!(loaded_preset_view_by_id(&unknown).is_none());
    }
}
