//! Registered access to family-owned preset catalogs and their caches.
use std::sync::{Arc, LazyLock};
use manifold_core::{PresetTypeId, effect_graph_def::EffectGraphDef, preset_def::PresetKind};

pub struct PresetCatalogSource {
    pub name: &'static str,
    pub json: fn(&PresetTypeId) -> Option<Arc<str>>,
    pub def: fn(&PresetTypeId) -> Option<&'static EffectGraphDef>,
    pub visit: fn(PresetKind, &mut dyn FnMut(PresetTypeId)),
}
inventory::collect!(PresetCatalogSource);

fn sources() -> &'static [&'static PresetCatalogSource] {
    static SOURCES: LazyLock<Vec<&'static PresetCatalogSource>> = LazyLock::new(|| {
        let mut entries: Vec<_> = inventory::iter::<PresetCatalogSource>.into_iter().collect();
        entries.sort_unstable_by_key(|entry| entry.name);
        assert!(entries.windows(2).all(|pair| pair[0].name != pair[1].name), "duplicate preset catalog provider name");
        entries
    });
    &SOURCES
}

pub fn preset_json(id: &PresetTypeId) -> Option<Arc<str>> {
    sources().iter().find_map(|source| (source.json)(id))
}

pub fn preset_def(id: &PresetTypeId) -> Option<&'static EffectGraphDef> {
    sources().iter().find_map(|source| (source.def)(id))
}

pub fn visit_presets(kind: PresetKind, visitor: &mut dyn FnMut(PresetTypeId)) {
    for source in sources() {
        (source.visit)(kind, visitor);
    }
}

pub fn preset_type_ids(kind: PresetKind) -> impl Iterator<Item = PresetTypeId> {
    let mut ids = Vec::new();
    visit_presets(kind, &mut |id| ids.push(id));
    ids.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_catalog_providers_have_disjoint_type_ids() {
        let mut ids = std::collections::HashSet::new();
        for kind in [PresetKind::Effect, PresetKind::Generator, PresetKind::SceneModifier] {
            visit_presets(kind, &mut |id| assert!(ids.insert(id.clone()), "duplicate preset type id: {id}"));
        }
    }
}
