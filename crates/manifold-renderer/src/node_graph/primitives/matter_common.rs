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
