//! Construct the same pristine view for an imported definition.
use super::*;
pub fn imported_view(type_id: PresetTypeId, def: EffectGraphDef) -> LoadedPresetView {
    let metadata = def.preset_metadata.as_ref().expect("import def carries metadata");
    let bindings = owned_bindings(metadata).expect("ordinary graph bindings");
    LoadedPresetView { type_id, canonical_def: Arc::new(def), bindings, fused_retarget: AHashMap::default(), mesh_rules: PreparedMeshRules::default() }
}
