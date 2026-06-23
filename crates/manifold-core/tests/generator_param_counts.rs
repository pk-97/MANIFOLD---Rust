//! Generator param-count parity: every generator preset's `param_count` in the
//! `preset_definition_registry` matches its spec.
//!
//! These exercise the engine's preset registries (not the UI), so they live in
//! `manifold-core` where the `inventory` submissions are linked. (Moved here
//! from `manifold-ui/tests/parity.rs` in the Phase 5 layering inversion, when
//! the UI stopped depending on `manifold-core`. See
//! `docs/UI_LAYERING_INVERSION.md`.)

use manifold_core::PresetTypeId;
use manifold_core::preset_definition_registry;

#[test]
fn generator_param_count_plasma() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::PLASMA).param_count,
        6
    );
}
// `generator_param_count_basic_shapes` intentionally absent — post-§11
// BasicShapes lives entirely in `assets/generator-presets/BasicShapes.json` and
// is only registered when manifold-renderer is linked (via the
// `LoadedPresetSource` inventory submission). manifold-core alone doesn't
// register it, so the registry lookup would panic here. The JSON file IS the
// schema; renderer-side `every_bundled_preset_binding_resolves_to_an_outer_param`
// catches param-surface regressions.
#[test]
fn generator_param_count_concentric_tunnel() {
    // 6 outer-card params: shape (Circle/Triangle/Square/Pentagon/Hexagon —
    // Star removed), line, rate, ring_spacing (renamed from legacy `scale`),
    // clip_trigger, trigger_mode (Shape/Spawn/Both, restored from the legacy
    // `clip_trigger_mode`). JSON: `assets/generator-presets/ConcentricTunnel.json`.
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::CONCENTRIC_TUNNEL).param_count,
        6
    );
}
#[test]
fn generator_param_count_tesseract() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::TESSERACT).param_count,
        11
    );
}
#[test]
fn generator_param_count_duocylinder() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::DUOCYLINDER).param_count,
        11
    );
}
#[test]
fn generator_param_count_lissajous() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::LISSAJOUS).param_count,
        11
    );
}
#[test]
fn generator_param_count_wireframe_zoo() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::WIREFRAME_ZOO).param_count,
        9
    );
}
#[test]
fn generator_param_count_fluid_sim() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::FLUID_SIMULATION).param_count,
        14
    );
}
#[test]
fn generator_param_count_fluid_sim_3d() {
    assert_eq!(
        preset_definition_registry::get(&PresetTypeId::FLUID_SIMULATION_3D).param_count,
        20
    );
}

#[test]
fn generator_all_types_have_params() {
    // Every generator type (except None) must have at least 1 param defined.
    use manifold_core::{preset_def::PresetKind, preset_type_registry};
    for reg in preset_type_registry::all_of_kind(PresetKind::Generator) {
        assert!(
            preset_definition_registry::get(&reg.id).param_count > 0,
            "{:?} has no param definitions",
            reg.id
        );
    }
}

#[test]
fn generator_max_param_count() {
    // FluidSimulation3D has the most params (20).
    use manifold_core::{preset_def::PresetKind, preset_type_registry};
    let max = preset_type_registry::all_of_kind(PresetKind::Generator)
        .iter()
        .map(|reg| preset_definition_registry::get(&reg.id).param_count)
        .max()
        .unwrap_or(0);
    assert_eq!(max, 20);
}
