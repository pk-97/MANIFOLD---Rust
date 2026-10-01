//! `node.coarse_inverse` — an exact solve for the bottom of a multigrid
//! V-cycle: the inverse of the masked Poisson matrix on a lattice of at most
//! 64 cells, built once per water lattice and applied by `node.combine_rows`
//! in every V-cycle. GPU FLIP smooths its coarsest level instead, since a
//! fixed level count leaves it past 64 cells at 128³
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 3 (the solve)). A barriered solve
//! (docs/ADDING_PRIMITIVES.md exclusion 1): each elimination step waits for
//! the one before.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use super::cells_with_particles::{cell_count, cell_lattice};
use super::particles_to_faces::face_count;
use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
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
    purpose: "The inverse of the masked Poisson matrix A on a small lattice (nodes_x/y/z cells, cell (i, j, k) at i + nx·(j + ny·k), at most 64 cells), as a cells × cells row-major array: A[c][c] is the sum of the open fractions w of c's faces and A[c][d] is −w for a water neighbour d across a face of open fraction w (solid_faces, node.solid_faces' face grid; box walls 0), on water cells (water > 0.5) only, so the weighted Laplacian is −A / h². Built in one workgroup by symmetric elimination; the result is exactly symmetric. Air rows and columns are zero. A water body that touches no air is singular: one of its cells is pinned at zero; so is a water cell with no open face.",
    inputs: {
        water: Array(f32) required,
        solid_faces: Array(FaceSample) required,
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
    composition_notes: "The bottom of the multigrid V-cycle, once per water lattice: water is the coarsest node.coarsen_water level, solid_faces the coarsest node.coarsen_solid_faces level. Apply it with node.combine_rows: base and coef the coarsest rhs (from node.restrict_lattice), matrix this output, rows and row_length the cell count, base_scale 0, scale −h² at that level's cell size. The result solves L e = rhs exactly.",
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
        let (Some(water), Some(solid_faces), Some(out)) =
            (ctx.inputs.array("water"), ctx.inputs.array("solid_faces"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size
            || cells * cells * 4 > out.size
            || face_count(nodes) * size_of::<FaceSample>() as u64 > solid_faces.size
        {
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
                GpuBinding::Buffer { binding: 2, buffer: solid_faces, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
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
}
