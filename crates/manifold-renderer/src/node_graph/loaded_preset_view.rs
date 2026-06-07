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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ahash::AHashMap;
use arc_swap::ArcSwap;
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, PresetMetadata, SkipModeDef,
};
use manifold_core::effects::ParamConvert;

use crate::node_graph::bundled_presets::bundled_preset_def;
use crate::node_graph::chain_spec::SkipMode;
use crate::node_graph::param_binding::{ParamBinding, ParamId, ParamTarget};
use crate::node_graph::snapshot::{GraphSnapshot, OuterParamRouting, OuterParamSource};

/// Runtime view assembled from a JSON-loaded preset. Replaces the
/// inventory-submitted `ChainSpec` that used to ship as a static
/// `splice` fn plus a canonical graph builder; this view keeps the
/// same effective shape but sources `canonical_def` and `bindings`
/// from JSON. The chain builder uses
/// [`crate::node_graph::splice_def_into_chain`] with `canonical_def`
/// to produce equivalent worker nodes.
pub struct LoadedPresetView {
    pub type_id: PresetTypeId,
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
    /// Fusion binding-retarget map, populated only on **fused** views
    /// (empty for the plain JSON-loaded view). `(original stable
    /// node_id, original param) → (fused node id, fused uniform field
    /// `n{idx}_<param>`)`. Static card bindings on this view are already
    /// retargeted; this map exists so the chain builder can retarget a
    /// *per-instance* user binding (`PresetInstance.user_param_bindings`,
    /// which lives off the def and so is invisible to content-keyed
    /// fusion) onto the fused node, exactly as the static bindings were.
    /// Without it a user-exposed slider resolves against a node the fuse
    /// collapsed away and silently goes inert once the effect re-fuses.
    pub fused_retarget: AHashMap<(String, String), (NodeId, String)>,
}

/// Generation-stamped cache of leaked `&'static LoadedPresetView`s. Keeps
/// the `&'static` return (the render path stores `view.canonical_def:
/// &'static EffectGraphDef` and `view.bindings: &'static [_]`) while
/// allowing a hot-reload (step 10) to rebuild the map from the new catalog.
///
/// At rest the generation never moves, so [`loaded_preset_view_by_id`] is
/// one relaxed atomic compare + an `ArcSwap` pointer load + an
/// `AHashMap::get` — the same cost class as the old `OnceLock`. This sits
/// on the per-frame path (`compute_topology_hash` reads `view.skip_mode`),
/// so the at-rest cost is the single atomic load the prime directive
/// permits.
struct ViewCache {
    generation: AtomicU64,
    map: ArcSwap<AHashMap<PresetTypeId, &'static LoadedPresetView>>,
}

static VIEW_CACHE: std::sync::LazyLock<ViewCache> = std::sync::LazyLock::new(|| ViewCache {
    generation: AtomicU64::new(u64::MAX),
    map: ArcSwap::from_pointee(AHashMap::default()),
});

/// Lookup a [`LoadedPresetView`] by effect type id, building it on
/// first call (and after each hot-reload generation bump) and caching for
/// the process lifetime. Returns `None` for effects whose JSON file doesn't
/// carry `presetMetadata` (i.e., v1 entries — not yet migrated by §11
/// block 4).
pub fn loaded_preset_view_by_id(id: &PresetTypeId) -> Option<&'static LoadedPresetView> {
    let generation = crate::preset_loader::catalog_generation();
    if VIEW_CACHE.generation.load(Ordering::Acquire) != generation {
        rebuild_view_cache(generation);
    }
    VIEW_CACHE.map.load().get(id).copied()
}

#[cold]
fn rebuild_view_cache(generation: u64) {
    VIEW_CACHE.map.store(Arc::new(build_view_map()));
    VIEW_CACHE.generation.store(generation, Ordering::Release);
}

fn build_view_map() -> AHashMap<PresetTypeId, &'static LoadedPresetView> {
    let mut m: AHashMap<PresetTypeId, &'static LoadedPresetView> = AHashMap::default();
    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
        manifold_core::preset_def::PresetKind::Effect,
    ) {
        if let Some(view) = build_view(&type_id) {
            m.insert(type_id, Box::leak(Box::new(view)));
        }
    }
    m
}

fn build_view(type_id: &PresetTypeId) -> Option<LoadedPresetView> {
    let def = bundled_preset_def(type_id)?;
    let metadata = def.preset_metadata.as_ref()?;
    Some(LoadedPresetView {
        type_id: type_id.clone(),
        canonical_def: def,
        bindings: leak_bindings(metadata),
        skip_mode: skip_mode_from_def(&metadata.skip_mode),
        // Unfused view — no retargeting; user bindings resolve directly
        // against the canonical inner nodes.
        fused_retarget: AHashMap::default(),
    })
}

fn leak_bindings(meta: &PresetMetadata) -> &'static [ParamBinding] {
    let owned: Vec<ParamBinding> = meta
        .bindings
        .iter()
        .map(|b| binding_def_to_runtime(b, meta.params.iter().find(|p| p.id == b.id)))
        .collect();
    Box::leak(owned.into_boxed_slice())
}

fn binding_def_to_runtime(
    def: &BindingDef,
    param: Option<&manifold_core::effect_graph_def::ParamSpecDef>,
) -> ParamBinding {
    let label: &'static str = Box::leak(def.label.clone().into_boxed_str());
    // Slider response + range come from the owning card param (Phase 2's
    // `ParamSpecDef.curve`/`.invert` + min/max). Composite/fan-out bindings with
    // no matching param fall back to identity (0..1, Linear, no invert).
    let (min, max, curve, invert) = param
        .map(|p| (p.min, p.max, p.curve, p.invert))
        .unwrap_or((0.0, 1.0, Default::default(), false));
    ParamBinding {
        id: ParamId::Owned(def.id.clone()),
        label,
        default_value: def.default_value,
        target: target_def_to_runtime(&def.target),
        convert: def.convert,
        scale: def.scale,
        offset: def.offset,
        min,
        max,
        curve,
        invert,
    }
}

fn target_def_to_runtime(def: &BindingTarget) -> ParamTarget {
    match def {
        BindingTarget::Node { node_id, param } => {
            let param: &'static str = Box::leak(param.clone().into_boxed_str());
            ParamTarget::Node {
                node_id: node_id.clone(),
                param,
            }
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

/// Build the editor-canvas snapshot for a loaded preset. Reconstructs
/// a temporary `Graph` from the JSON's canonical def via
/// [`GraphSnapshot::from_def`] (same path the per-card-override
/// snapshot already uses) and overlays the outer→inner routings the
/// inspector needs to gray out driven rows. Returns `None` if the
/// canonical def fails to materialize (mismatched primitives,
/// unsupported version) — caller treats that as "no active graph".
pub fn snapshot_for_view(view: &LoadedPresetView) -> Option<GraphSnapshot> {
    let mut snap = GraphSnapshot::from_def(view.canonical_def)?;
    snap.outer_routings = outer_routings_from_view(view);
    Some(snap)
}

/// Translate a [`LoadedPresetView`]'s bindings into editor
/// [`OuterParamRouting`]s — same projection
/// `EffectRegistry::outer_routings_for` used to perform off a
/// `ChainSpec`, sourced from the JSON-loaded bindings instead. One
/// entry per binding whose target is a named-handle inner node
/// (composite/custom variants don't surface a handle and are
/// skipped).
pub fn outer_routings_from_view(view: &LoadedPresetView) -> Vec<OuterParamRouting> {
    // node_id → display handle. The routing carries the *handle* because
    // the editor keys its per-node rows by handle within a single
    // snapshot (where handles are unique); the binding addresses by id,
    // resolved here against the canonical def's nodes.
    let handle_by_id: std::collections::HashMap<&str, &str> = view
        .canonical_def
        .nodes
        .iter()
        .filter_map(|n| n.handle.as_deref().map(|h| (n.node_id.as_str(), h)))
        .collect();
    let mut out = Vec::with_capacity(view.bindings.len());
    for binding in view.bindings {
        let (node_id, inner_param) = match &binding.target {
            ParamTarget::Node { node_id, param } => (node_id, *param),
            _ => continue,
        };
        let Some(handle) = handle_by_id.get(node_id.as_str()) else {
            continue;
        };
        out.push(OuterParamRouting {
            outer_label: binding.label.to_string(),
            outer_param_id: binding.id.to_string(),
            node_handle: handle.to_string(),
            inner_param: inner_param.to_string(),
            source: OuterParamSource::Static,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: looking up an unknown id returns None, not a panic or
    /// stale result.
    #[test]
    fn loaded_preset_view_returns_none_for_unknown_id() {
        let unknown = PresetTypeId::from_string("NotARealEffect".to_string());
        assert!(loaded_preset_view_by_id(&unknown).is_none());
    }
}
