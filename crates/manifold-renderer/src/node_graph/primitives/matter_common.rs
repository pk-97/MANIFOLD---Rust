//! What the matter atoms share at run time (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 3.2 (Atoms and ports)): the walls shader. The lattice itself is
//! every liquid's (`liquid::lattice::LiquidLattice::from_wires`).

/// The lattice's closed walls, three padding nodes deep.
pub(super) const MATTER_WALLS: &str = include_str!("shaders/matter_walls.wgsl");

/// Fused codegen does not namespace member helpers, so helpers shared by
/// several matter bodies are copied with an atom prefix
/// (GPU_MPM_SOLVER_DESIGN.md section 9 (Section 2.5 audit and codegen
/// classification)). These tests keep the copies identical.
#[cfg(test)]
mod tests {
    const P2G: &str = include_str!("shaders/matter_to_grid.wgsl");
    const G2P: &str = include_str!("shaders/grid_to_matter_body.wgsl");
    const SORT: &str = include_str!("shaders/sort_particles_into_cells.wgsl");

    /// The text of `fn <prefix>_<name>` (or `fn <name>` for the hand kernel's
    /// own module) up to its closing brace, with the prefix removed.
    fn helper(source: &str, prefix: &str, name: &str) -> String {
        let head = if prefix.is_empty() { format!("fn {name}(") } else { format!("fn {prefix}_{name}(") };
        let start = source.find(&head).unwrap_or_else(|| panic!("no {head}"));
        let end = start + source[start..].find("\n}\n").expect("helper ends") + 3;
        if prefix.is_empty() {
            source[start..end].to_string()
        } else {
            source[start..end].replace(&format!("{prefix}_"), "")
        }
    }

    #[test]
    fn matter_finite_helpers_are_identical() {
        let p2g = helper(P2G, "", "finite3");
        assert_eq!(p2g, helper(G2P, "g2m", "finite3"));
        assert_eq!(p2g, helper(SORT, "", "finite3"));
    }

    #[test]
    fn matter_reaction_rounding_matches_p2g() {
        const REACTION: &str = include_str!("shaders/matter_body_reaction_body.wgsl");
        for name in ["hash", "encode"] {
            assert_eq!(helper(P2G, "", name), helper(REACTION, "reaction", name));
            assert_eq!(helper(P2G, "", name), helper(G2P, "g2m", name));
        }
        // The body-reaction words take the push-out's share the same way.
        assert_eq!(helper(REACTION, "", "reaction_add").replace("reaction_", ""), helper(G2P, "g2m", "reaction_add").replace("reaction_", ""));
    }

    #[test]
    fn matter_stencil_weights_are_identical() {
        let lines = |source: &str| {
            source
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("let base = ") || l.starts_with("let f = ") || l.starts_with("let w0 = ") || l.starts_with("let w1 = ") || l.starts_with("let w2 = "))
                .map(String::from)
                .collect::<Vec<_>>()
        };
        let p2g = lines(P2G);
        assert_eq!(p2g.len(), 5, "{p2g:?}");
        assert_eq!(p2g, lines(G2P));
    }
}
