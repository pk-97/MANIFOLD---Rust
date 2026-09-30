//! Which graph nodes are liquid domains. The scene layer asks this one
//! predicate instead of comparing type ids, so every liquid solver is found by
//! the same walk (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.5 (Scene
//! recognition)). No other file writes a domain type-id literal.

/// The FLIP liquid domain.
pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface";
/// The GPU MLS-MPM liquid domain.
pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain";

/// Every liquid domain type.
pub const LIQUID_DOMAIN_TYPE_IDS: &[&str] = &[FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID];

pub fn is_liquid_domain(type_id: &str) -> bool {
    LIQUID_DOMAIN_TYPE_IDS.contains(&type_id)
}

/// Water-panel params per domain type, under FLIP's names where the meaning is
/// shared (`docs/GPU_MPM_SOLVER_DESIGN.md` D17).
pub const LIQUID_DIAL_PARAMS: &[(&str, &[&str])] = &[
    (
        FLIP_DOMAIN_TYPE_ID,
        &[
            "seed", "domain_size", "fill_height", "liquid_density", "viscosity", "surface_tension",
            "gravity_x", "gravity", "gravity_z",
            "emission", "inflow_speed", "speed", "reset", "surface_subdivisions",
            "surface_particle_scale", "surface_smoothing", "surface_smoothing_iterations",
            "resolution", "grid_budget_mcells", "transfer", "whitewater", "whitewater_capacity",
            "whitewater_wavecrest_rate", "whitewater_turbulence_rate",
            "whitewater_min_energy", "whitewater_max_energy",
            "closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y",
            "closed_neg_z", "closed_pos_z",
        ],
    ),
    (
        MATTER_DOMAIN_TYPE_ID,
        &[
            "seed", "domain_size", "fill_height",
            "gravity_x", "gravity", "gravity_z",
            "speed", "reset",
            "resolution", "grid_budget_mcells",
            "closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y",
            "closed_neg_z", "closed_pos_z",
            "points_per_cell", "stiffness", "cohesion", "liveliness",
        ],
    ),
];

/// The water-panel params of `type_id`, or `None` when it is not a liquid
/// domain.
pub fn liquid_dial_params(type_id: &str) -> Option<&'static [&'static str]> {
    LIQUID_DIAL_PARAMS
        .iter()
        .find(|(domain, _)| *domain == type_id)
        .map(|(_, params)| *params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_domain_predicate_covers_both() {
        assert!(is_liquid_domain(FLIP_DOMAIN_TYPE_ID));
        assert!(is_liquid_domain(MATTER_DOMAIN_TYPE_ID));
        assert!(!is_liquid_domain("node.physics_world"));
        assert!(!is_liquid_domain("node.matter_state"));
    }

    #[test]
    fn liquid_dial_params_cover_every_domain() {
        for type_id in LIQUID_DOMAIN_TYPE_IDS {
            assert!(
                liquid_dial_params(type_id).is_some_and(|params| !params.is_empty()),
                "{type_id} has no water-panel row in LIQUID_DIAL_PARAMS"
            );
        }
        for (type_id, _) in LIQUID_DIAL_PARAMS {
            assert!(is_liquid_domain(type_id), "{type_id} has dials but is not a liquid domain");
        }
    }

    /// I1: the literals of `LIQUID_DOMAIN_TYPE_IDS` live only here, in the
    /// load-time rename table, and in each domain's own primitive file.
    #[test]
    fn liquid_type_ids_live_in_one_place() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates directory");
        let allowed = [
            "manifold-core/src/liquid_domain.rs",
            "manifold-core/src/type_id_migration.rs",
            "manifold-renderer/src/node_graph/primitives/fluid_surface.rs",
            "manifold-renderer/src/node_graph/primitives/matter_domain.rs",
        ];
        let patterns: Vec<String> = LIQUID_DOMAIN_TYPE_IDS
            .iter()
            .flat_map(|id| [format!("\"{id}\""), format!("\\\"{id}\\\"")])
            .collect();
        let mut offenders = Vec::new();
        let mut stack = vec![crates.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("readable crates tree") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|name| name != "target") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(crates)
                    .expect("under crates")
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowed.contains(&relative.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable source");
                for (number, line) in text.lines().enumerate() {
                    if patterns.iter().any(|pattern| line.contains(pattern.as_str())) {
                        offenders.push(format!("{relative}:{}", number + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "liquid domain type-id literals outside manifold_core::liquid_domain: {offenders:#?}"
        );
    }
}
