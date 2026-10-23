//! Graph scenes that exercise the water, physics and ocean engines. They are
//! proof fixtures, not presets: only the base Scene ships in the generator
//! browser (SCENE_DOCUMENT_DESIGN.md section 1 (Rulings), ruling 13), so these
//! live in `tests/fixtures/presets/` and load by name from the source tree.

use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;

/// Every engine scene, by file stem.
pub const NAMES: [&str; 9] = [
    "Ocean",
    "OceanCliff",
    "PhysicsSolids",
    "WaterDamBreakGpuFlip",
    "WaterDamBreakMatter",
    "WaterDamBreakParticles",
    "WaterFloatingBoxMatter",
    "WaterSeaWallGpuFlip",
    "WaterStillPoolMatter",
];

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/presets").join(format!("{name}.json"))
}

/// The scene's JSON, as saved.
pub fn json(name: &str) -> String {
    std::fs::read_to_string(path(name)).unwrap_or_else(|e| panic!("engine scene {name}: {e}"))
}

/// The scene as the preset catalog would load it.
pub fn def(name: &str) -> Arc<EffectGraphDef> {
    Arc::new(crate::bundled_presets::parse_bundled_preset(PresetKind::Generator, name, &json(name)))
}

/// Every engine scene, parsed.
pub fn all() -> impl Iterator<Item = (&'static str, Arc<EffectGraphDef>)> {
    NAMES.into_iter().map(|name| (name, def(name)))
}

/// The scene named by `id`, as saved, or `None` for any other preset.
pub fn scene_json(id: &manifold_core::PresetTypeId) -> Option<Arc<str>> {
    NAMES.contains(&id.as_str()).then(|| Arc::from(json(id.as_str())))
}

/// The scene named by `id`, as the catalog would load it, or `None`.
pub fn scene_def(id: &manifold_core::PresetTypeId) -> Option<Arc<EffectGraphDef>> {
    NAMES.contains(&id.as_str()).then(|| def(id.as_str()))
}

/// Make every engine scene resolve by type id, for a project that plays one
/// (a layer of type `WaterDamBreakGpuFlip`). They join the catalog as a
/// project overlay, so the generator browser still lists only the base Scene.
/// It fills the existing project overlay slot, so a project load replaces it.
/// Call before the layer is created; later calls do nothing.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        use manifold_core::project::EmbeddedOrigin;
        let scenes = NAMES.iter().map(|name| ((*name).to_owned(), json(name), EmbeddedOrigin::Snapshot)).collect();
        manifold_node_engine::load::preset_loader::set_project_presets(Vec::new(), scenes, Vec::new());
    });
}
