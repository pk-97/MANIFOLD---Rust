//! Runtime view of a catalog preset.
//!
//! A [`LoadedPresetView`] pairs the canonical [`EffectGraphDef`] loaded from
//! the current catalog with its renderer-side [`ParamBinding`] values. The
//! effect chain consumes the graph through
//! [`crate::load::chain_spec::splice_def_into_chain`], while editor routing
//! uses the same bindings.
//!
//! Views are rebuilt when the catalog generation changes. Callers retain owned
//! snapshots, so replaced views are released when no longer in use.

use std::borrow::Cow;
use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, PresetMetadata,
};

use crate::load::catalog_source::preset_def as bundled_preset_def;
use crate::scene::mesh_change::PreparedMeshRules;
use crate::param_binding::{ParamBinding, ParamId, ParamTarget};
use crate::snapshot::{GraphSnapshot, OuterParamRouting, OuterParamSource};

/// Runtime view assembled from a catalog preset. The chain builder splices
/// `canonical_def` into the active graph and uses `bindings` for parameter
/// routing.
pub struct LoadedPresetView {
    pub type_id: PresetTypeId,
    /// Canonical graph loaded from the current catalog. The `Arc` shares this
    /// graph between consumers of one view.
    pub canonical_def: Arc<EffectGraphDef>,
    /// Outer-card slider bindings reconstructed from `presetMetadata`.
    pub bindings: Vec<ParamBinding>,
    /// Fusion binding-retarget map, populated only on fused views. It maps
    /// original node parameters to their fused uniform fields so per-instance
    /// user bindings continue to resolve after fusion.
    pub fused_retarget: AHashMap<(String, String), (NodeId, String)>,
    /// Mesh-revision rules keyed by generated node id. Empty on canonical
    /// views and regenerated for fused views; never serialized.
    pub mesh_rules: PreparedMeshRules,
}

/// The generation and its views are published together as one snapshot.
struct ViewCache {
    generation: u64,
    map: AHashMap<PresetTypeId, Arc<LoadedPresetView>>,
}

static VIEW_CACHE: std::sync::LazyLock<ArcSwap<ViewCache>> = std::sync::LazyLock::new(|| {
    ArcSwap::from_pointee(ViewCache {
        generation: u64::MAX,
        map: AHashMap::default(),
    })
});

/// Look up a view in the current catalog generation. Returns `None` when the
/// definition lacks `presetMetadata` or cannot be prepared.
pub fn loaded_preset_view_by_id(id: &PresetTypeId) -> Option<Arc<LoadedPresetView>> {
    let generation = crate::load::preset_loader::catalog_generation();
    let cache = VIEW_CACHE.load();
    if cache.generation == generation {
        cache.map.get(id).cloned()
    } else {
        rebuild_view_cache(generation).map.get(id).cloned()
    }
}

#[cold]
fn rebuild_view_cache(generation: u64) -> Arc<ViewCache> {
    let cache = Arc::new(ViewCache {
        generation,
        map: build_view_map(),
    });
    VIEW_CACHE.store(Arc::clone(&cache));
    cache
}

fn build_view_map() -> AHashMap<PresetTypeId, Arc<LoadedPresetView>> {
    let mut m: AHashMap<PresetTypeId, Arc<LoadedPresetView>> = AHashMap::default();
    // Effect and generator ids share one map because their type ids are
    // globally disjoint. Definitions without `presetMetadata` have no view.
    use crate::load::catalog_source::preset_type_ids as bundled_preset_type_ids;
    use manifold_core::preset_def::PresetKind;
    for type_id in bundled_preset_type_ids(PresetKind::Effect)
        .chain(bundled_preset_type_ids(PresetKind::Generator))
    {
        if let Some(view) = build_view(&type_id) {
            m.insert(type_id, Arc::new(view));
        }
    }
    m
}

fn build_view(type_id: &PresetTypeId) -> Option<LoadedPresetView> {
    let def = bundled_preset_def(type_id)?;
    let prepared;
    let metadata = if manifold_core::scene_modifier_preset::has_scene_modifier_data(&def) {
        prepared = match crate::load::expand::prepare_scene_modifiers(
            &def, &crate::persistence::PrimitiveRegistry::with_builtin(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                log::error!("preset `{type_id}` scene modifier preparation failed: {error}");
                return None;
            }
        };
        prepared.def.preset_metadata.as_ref()?
    } else { def.preset_metadata.as_ref()? };
    let bindings = owned_bindings(metadata)?;
    Some(LoadedPresetView {
        type_id: type_id.clone(),
        canonical_def: def,
        bindings,
        // Canonical view: user bindings resolve directly against inner nodes.
        fused_retarget: AHashMap::default(),
        mesh_rules: PreparedMeshRules::default(),
    })
}

fn owned_bindings(meta: &PresetMetadata) -> Option<Vec<ParamBinding>> {
    meta.bindings
        .iter()
        .map(|b| binding_def_to_runtime(b, meta.params.iter().find(|p| p.id == b.id)))
        .collect()
}

fn binding_def_to_runtime(
    def: &BindingDef,
    param: Option<&manifold_core::effect_graph_def::ParamSpecDef>,
) -> Option<ParamBinding> {
    let target = target_def_to_runtime(&def.target)?;
    // Slider response and range come from the owning card parameter. Composite
    // bindings without a matching parameter use the identity response.
    let (min, max, curve, invert) = param
        .map(|p| (p.min, p.max, p.curve, p.invert))
        .unwrap_or((0.0, 1.0, Default::default(), false));
    Some(ParamBinding {
        id: ParamId::Owned(def.id.clone()),
        label: Cow::Owned(def.label.clone()),
        default_value: def.default_value,
        target,
        convert: def.convert,
        scale: def.scale,
        offset: def.offset,
        min,
        max,
        curve,
        invert,
        default_mirrors_node_param: def.default_mirrors_node_param,
    })
}

fn target_def_to_runtime(def: &BindingTarget) -> Option<ParamTarget> {
    Some(match def {
        BindingTarget::Node { node_id, param } => ParamTarget::Node {
            node_id: node_id.clone(),
            // Names from the catalog are owned by this binding.
            param: Cow::Owned(param.clone()),
        },
        BindingTarget::Composite { outer_name } => ParamTarget::Composite {
            outer_name: Cow::Owned(outer_name.clone()),
        },
        BindingTarget::SceneModifier { .. } => {
            log::error!("scene modifier bindings require host attachment expansion before runtime view creation");
            return None;
        }
    })
}

/// Build the editor-canvas snapshot for a catalog preset and overlay the
/// outer-to-inner routings used to mark driven rows. Returns `None` if the
/// canonical definition cannot materialize.
pub fn snapshot_for_view(view: &LoadedPresetView) -> Option<GraphSnapshot> {
    let mut snap = GraphSnapshot::from_def(&view.canonical_def)?;
    snap.outer_routings = outer_routings_from_view(view);
    Some(snap)
}

/// Collect `node_id → handle` for every node in `nodes`, including group
/// bodies. Handles are read from the display definition; boundary nodes with
/// no handle are skipped.
pub fn collect_node_handles<'a>(
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    out: &mut std::collections::HashMap<&'a str, &'a str>,
) {
    for n in nodes {
        if let Some(h) = n.handle.as_deref() {
            out.insert(n.node_id.as_str(), h);
        }
        if let Some(group) = n.group.as_deref() {
            collect_node_handles(&group.nodes, out);
        }
    }
}

/// Translate a [`LoadedPresetView`]'s bindings into editor
/// [`OuterParamRouting`]s. Bindings whose target has no named inner handle are
/// skipped.
pub fn outer_routings_from_view(view: &LoadedPresetView) -> Vec<OuterParamRouting> {
    // The editor keys rows by handle while bindings address nodes by id.
    // Group bodies use the same raw handles in the display definition, so the
    // recursive map resolves grouped targets too.
    let mut handle_by_id: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    collect_node_handles(&view.canonical_def.nodes, &mut handle_by_id);
    let mut out = Vec::with_capacity(view.bindings.len());
    for binding in &view.bindings {
        let (node_id, inner_param) = match &binding.target {
            ParamTarget::Node { node_id, param } => (node_id, param.clone()),
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

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testkit;
