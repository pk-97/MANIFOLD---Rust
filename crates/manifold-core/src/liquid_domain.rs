//! Which graph nodes are liquid domains. The scene layer asks this one
//! predicate instead of comparing type ids, so a FLIP domain and a matter
//! domain are found by the same walk (`docs/GPU_MPM_SOLVER_DESIGN.md` D17).

/// The FLIP liquid domain.
pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface";
/// The GPU MLS-MPM liquid domain.
pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain";

pub fn is_liquid_domain(type_id: &str) -> bool {
    type_id == FLIP_DOMAIN_TYPE_ID || type_id == MATTER_DOMAIN_TYPE_ID
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
}
