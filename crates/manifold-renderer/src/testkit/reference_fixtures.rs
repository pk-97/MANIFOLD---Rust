//! CPU FLIP reference graph fixtures used by conformance and GPU proofs.
//!
//! These graphs are deliberately outside `assets/generator-presets`: they
//! must never enter the product preset catalog. The module is registered only
//! for proof/test builds by `lib.rs`.

/// Return a retained CPU FLIP reference graph by its historical preset file
/// name.
pub fn cpu_flip_preset_json(name: &str) -> &'static str {
    match name {
        "WaterBasin.json" => include_str!("../../tests/fixtures/cpu-flip/WaterBasin.json"),
        "WaterDamBreak.json" => include_str!("../../tests/fixtures/cpu-flip/WaterDamBreak.json"),
        "WaterDamBreakGpu.json" => include_str!("../../tests/fixtures/cpu-flip/WaterDamBreakGpu.json"),
        _ => panic!("unknown CPU FLIP reference fixture: {name}"),
    }
}

/// Return the retired CPU FLIP scene-panel metadata for reference proofs.
///
/// The product exposure vocabulary intentionally no longer includes this
/// solver. Proofs still need the authored card shape to exercise saved CPU
/// FLIP scenes exactly as they were authored.
#[cfg(feature = "gpu-proofs")]
pub fn cpu_flip_metadata() -> Vec<manifold_core::scene_exposure::SceneParamMetadata> {
    const CPU_FLIP_DIALS: &[&str] = &[
        "seed", "domain_size", "fill_height", "liquid_density", "viscosity", "surface_tension",
        "gravity_x", "gravity", "gravity_z", "emission", "inflow_speed", "speed", "reset",
        "surface_subdivisions", "surface_particle_scale", "surface_smoothing",
        "surface_smoothing_iterations", "resolution", "grid_budget_mcells", "transfer", "whitewater",
        "whitewater_capacity", "whitewater_wavecrest_rate", "whitewater_turbulence_rate",
        "whitewater_min_energy", "whitewater_max_energy", "closed_neg_x", "closed_pos_x",
        "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z",
    ];
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_cpu_flip_reference();
    manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type_with_registry(
        &registry,
        manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
        Some(CPU_FLIP_DIALS),
    )
}
