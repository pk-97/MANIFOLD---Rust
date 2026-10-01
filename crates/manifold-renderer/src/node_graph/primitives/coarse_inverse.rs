//! `node.coarse_inverse` — the exact solve at the bottom of the multigrid
//! V-cycle (docs/GPU_FLIP_PRESSURE_SOLVE.md section 3 (the solve)): the
//! inverse of the masked Poisson matrix on the coarsest level, built once per
//! water lattice and applied by `node.combine_rows` in every V-cycle. A
//! barriered solve (docs/ADDING_PRIMITIVES.md exclusion 1): each elimination
//! step waits for the one before. Also the V-cycle's level rule and its
//! named refusal.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use super::cells_with_particles::{cell_count, cell_lattice};
use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/coarse_inverse.wgsl");

/// Cells the coarsest level may hold: the shader's MAX_CELLS, and the rows
/// `node.combine_rows` applies the inverse with. A larger coarsest level is
/// refused when the graph is built, never cut.
pub(crate) const MAX_COARSE_CELLS: u64 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InverseParams {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    _pad0: u32,
}

crate::primitive! {
    name: CoarseInverse,
    type_id: "node.coarse_inverse",
    purpose: "The inverse of the masked Poisson matrix A on a small lattice (nodes_x/y/z cells, cell (i, j, k) at i + nx·(j + ny·k), at most 64 cells), as a cells × cells row-major array: A[c][c] counts c's neighbours inside the box and A[c][d] is −1 for a water neighbour d, on water cells (water > 0.5) only, so the masked Laplacian is −A / h². Built in one workgroup by symmetric elimination; the result is exactly symmetric. Air rows and columns are zero. A water body that touches no air is singular: one of its cells is pinned at zero.",
    inputs: {
        water: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 4.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 4.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 4.0, 1.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The bottom of the multigrid V-cycle, once per water lattice: water is the coarsest node.coarsen_water level. Apply it with node.combine_rows: base and coef the coarsest rhs (from node.restrict_lattice), matrix this output, rows and row_length the cell count, base_scale 0, scale −h² at that level's cell size. The result solves L e = rhs exactly.",
    examples: [],
    picker: { label: "Coarse Inverse", category: Atom },
    summary: "Works out the exact pressure answer on the solver's smallest grid.",
    category: Particles3D,
    role: Filter,
    aliases: ["coarse solve", "multigrid bottom", "direct solve", "matrix inverse"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        inverse: Option<GpuComputePipeline> = None,
    },
}

/// A V-cycle halves the lattice while every side is even and one is over
/// this.
pub(crate) const COARSEST_SIDE: u32 = 4;

/// The V-cycle's lattices, finest first: halved while every side is even and
/// one is over [`COARSEST_SIDE`].
pub(crate) fn multigrid_levels(cells: [u32; 3]) -> Vec<[u32; 3]> {
    let mut levels = vec![cells];
    while let Some(&last) = levels.last().filter(|l| l.iter().all(|&n| n % 2 == 0) && l.iter().any(|&n| n > COARSEST_SIDE)) {
        levels.push(last.map(|n| n / 2));
    }
    levels
}

/// Why a lattice can't be solved: its coarsest level is past what the exact
/// solve holds. An odd side stops the halving where it stands, so a lattice
/// with an odd side over 4 is refused here.
pub(crate) fn multigrid_refusal(cells: [u32; 3]) -> Option<String> {
    let coarsest = *multigrid_levels(cells).last().expect("a level");
    let count = cell_count(coarsest);
    (count > MAX_COARSE_CELLS).then(|| {
        let odd: Vec<u32> = coarsest.iter().copied().filter(|n| n % 2 == 1).collect();
        let why = if odd.is_empty() { String::new() } else { format!(" (odd sides {odd:?} stop the halving)") };
        format!(
            "a {cells:?} cell lattice halves only to {coarsest:?}{why}, {count} cells, past the {MAX_COARSE_CELLS} the pressure solve's coarsest level solves exactly; every side must halve evenly down to {COARSEST_SIDE} or less"
        )
    })
}

/// The refusal at build and at run: a lattice past one workgroup.
pub(crate) fn coarse_refusal(params: &ParamValues) -> Option<String> {
    let Some(nodes) = cell_lattice(params) else {
        return Some("every lattice length must be 1 to 1024".into());
    };
    let cells = cell_count(nodes);
    (cells > MAX_COARSE_CELLS)
        .then(|| format!("a {nodes:?} lattice is {cells} cells, past the {MAX_COARSE_CELLS} one workgroup inverts"))
}

impl Primitive for CoarseInverse {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        // A larger lattice is refused at build (params_refusal).
        let cells = cell_count(cell_lattice(params)?);
        (port == "out" && cells <= MAX_COARSE_CELLS).then_some((cells * cells) as u32)
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        coarse_refusal(params)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(reason) = coarse_refusal(ctx.params) {
            ctx.error(format!("Coarse Inverse: {reason}"));
            return;
        }
        let nodes = cell_lattice(ctx.params).expect("checked by the refusal");
        {
            let gpu = ctx.gpu_encoder();
            if self.inverse.is_none() {
                self.inverse = Some(gpu.device.create_compute_pipeline(SHADER, "inverse_main", "node.coarse_inverse"));
            }
        }
        let (Some(water), Some(out)) = (ctx.inputs.array("water"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size || cells * cells * 4 > out.size {
            ctx.error(format!("Coarse Inverse: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = InverseParams { nodes_x: nodes[0], nodes_y: nodes[1], nodes_z: nodes[2], _pad0: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.inverse.as_ref().expect("pipeline created"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [1, 1, 1],
            "node.coarse_inverse",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The liquid conformance suite checks codegen bodies for atomics; this
    /// hand shader has no codegen body, so it is checked here.
    #[test]
    fn coarse_inverse_uses_no_atomics() {
        assert!(!SHADER.contains("atomic"));
    }

    /// Every index the shader forms stays inside `out` (cells² entries) and
    /// `water` (cells) at every lattice the refusal admits: the loops'
    /// e < entries, rows and columns i, j < cells, and i·n + k, k·n + j,
    /// j·n + i. The indices depend on the cell count alone, so the walk is
    /// every count the refusal admits. The shader's MAX_CELLS is this file's
    /// MAX_COARSE_CELLS.
    #[test]
    fn coarse_inverse_indices_stay_in_bounds() {
        assert!(SHADER.contains(&format!("const MAX_CELLS: u32 = {MAX_COARSE_CELLS}u;")));
        for cells in 1..=MAX_COARSE_CELLS {
            let entries = cells * cells;
            for e in 0..entries {
                let (i, j) = (e / cells, e % cells);
                assert!(i < cells && j < cells);
                for k in 0..cells {
                    assert!(i * cells + k < entries && k * cells + j < entries && k * cells + k < entries);
                }
                assert!(j * cells + i < entries);
            }
        }
    }

    #[test]
    fn levels_halve_to_four_and_odd_sides_are_refused() {
        assert_eq!(*multigrid_levels([64; 3]).last().unwrap(), [4; 3]);
        assert_eq!(*multigrid_levels([128; 3]).last().unwrap(), [4; 3]);
        assert_eq!(*multigrid_levels([96; 3]).last().unwrap(), [3; 3]);
        for side in [15u32, 9, 63] {
            let reason = multigrid_refusal([side; 3]).expect("refused");
            assert!(reason.contains("odd sides"), "{side}: {reason}");
        }
        assert!(multigrid_refusal([80; 3]).is_some(), "80 halves to 5³ = 125 cells");
        for side in [8u32, 16, 32, 48, 64, 96, 128] {
            assert!(multigrid_refusal([side; 3]).is_none(), "{side}");
        }
    }
}
