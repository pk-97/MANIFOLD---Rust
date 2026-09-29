//! What the matter atoms share at run time (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 3.2 (Atoms and ports)): the lattice arrives as scalar wires
//! (`lattice_min_x/y/z`, `cell_size`, `nodes_x/y/z`) because generated
//! uniforms pack scalar params only; a `Transform` has no WGSL layout.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::matter::MatterLattice;

/// The lattice wires as a [`MatterLattice`]. Defaults are the 4 m Dam Break
/// lattice at resolution 64, matching each atom's param defaults.
pub(super) fn read_lattice(ctx: &EffectNodeContext<'_, '_>) -> MatterLattice {
    let nodes = |name: &str| ctx.scalar_or_param(name, 71.0).round().max(1.0) as u32;
    let nodes = [nodes("nodes_x"), nodes("nodes_y"), nodes("nodes_z")];
    MatterLattice {
        min: [
            ctx.scalar_or_param("lattice_min_x", -2.1875),
            ctx.scalar_or_param("lattice_min_y", -0.1875),
            ctx.scalar_or_param("lattice_min_z", -2.1875),
        ],
        nodes,
        cell_size: ctx.scalar_or_param("cell_size", 0.0625),
        cells: nodes.map(|n| n.saturating_sub(1 + 2 * crate::node_graph::matter::PADDING_NODES)),
    }
}

/// Fused codegen does not namespace member helpers, so helpers shared by
/// several matter bodies are copied with an atom prefix
/// (GPU_MPM_SOLVER_DESIGN.md section 9 (Section 2.5 audit and codegen
/// classification)). These tests keep the copies identical.
#[cfg(test)]
mod tests {
    const P2G: &str = include_str!("shaders/matter_to_grid.wgsl");
    const G2P: &str = include_str!("shaders/grid_to_matter_body.wgsl");
    const M2P: &str = include_str!("shaders/matter_to_particles_body.wgsl");

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
        assert_eq!(p2g, helper(M2P, "m2p", "finite3"));
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
