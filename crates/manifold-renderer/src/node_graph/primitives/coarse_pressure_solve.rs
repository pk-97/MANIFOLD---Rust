//! `node.coarse_pressure_solve` — the multigrid pressure solve's coarsest
//! level (docs/GPU_FLIP_PRESSURE_SOLVE.md), solved in one workgroup by a
//! fixed count of red-black Gauss-Seidel sweeps held in workgroup memory. A
//! barriered solve (docs/ADDING_PRIMITIVES.md exclusion 1): every sweep waits
//! for the one before.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use super::cells_with_particles::{cell_count, cell_lattice};
use super::sort_particles_into_cells::{float_param, int_param};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/coarse_pressure_solve.wgsl");

/// Cells one workgroup holds: the shader's MAX_CELLS. A larger coarsest
/// level is refused when the graph is built, never cut.
pub(crate) const MAX_COARSE_CELLS: u64 = 4096;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CoarseParams {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    sweeps: u32,
    cell_size: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CoarsePressureSolve,
    type_id: "node.coarse_pressure_solve",
    purpose: "Solve the masked Poisson equation L p = rhs on a small lattice (nodes_x/y/z cells, cell (i, j, k) at i + nx·(j + ny·k), at most 4,096 cells) in one workgroup: from zero, `sweeps` red-black Gauss-Seidel sweeps (color 0 then 1), then `sweeps` more (1 then 0). A water cell (water > 0.5) becomes (Σ of its water neighbours − cell_size² · rhs) / (its neighbours inside the box); air holds zero and the box walls are closed.",
    inputs: {
        water: Array(f32) required,
        rhs: Array(f32) required,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 8.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 8.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 8.0, 1.0, 1024.0),
        float_param!("cell_size", "Cell Size", 0.5, 1.0e-4, 100.0),
        int_param!("sweeps", "Sweeps", 8.0, 1.0, 256.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The bottom of the multigrid V-cycle: rhs from node.restrict_lattice on the coarsest node.coarsen_water level; its output goes up through node.prolong_lattice. The level has a few dozen water cells, so a handful of sweeps solves it.",
    examples: [],
    picker: { label: "Coarse Pressure Solve", category: Atom },
    summary: "Solves the water's pressure exactly on the smallest grid of the solver.",
    category: Particles3D,
    role: Filter,
    aliases: ["coarse solve", "multigrid bottom", "gauss seidel"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        solve: Option<GpuComputePipeline> = None,
    },
}

fn whole(params: &ParamValues, name: &str, default: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => default,
    }
}

/// A V-cycle halves the lattice while every side is even and one is over
/// this.
pub(crate) const COARSEST_SIDE: u32 = 8;

/// The V-cycle's lattices, finest first: halved while every side is even and
/// one is over [`COARSEST_SIDE`].
pub(crate) fn multigrid_levels(cells: [u32; 3]) -> Vec<[u32; 3]> {
    let mut levels = vec![cells];
    while let Some(&last) = levels.last().filter(|l| l.iter().all(|&n| n % 2 == 0) && l.iter().any(|&n| n > COARSEST_SIDE)) {
        levels.push(last.map(|n| n / 2));
    }
    levels
}

/// Why a lattice can't be solved: its coarsest level is past one workgroup.
pub(crate) fn multigrid_refusal(cells: [u32; 3]) -> Option<String> {
    let coarsest = *multigrid_levels(cells).last().expect("a level");
    let count = cell_count(coarsest);
    (count > MAX_COARSE_CELLS).then(|| {
        format!(
            "a {cells:?} cell lattice halves only to {coarsest:?}, {count} cells, past the {MAX_COARSE_CELLS} the pressure solve's coarsest level holds; every side must halve evenly down to {COARSEST_SIDE} or so"
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
        .then(|| format!("a {nodes:?} lattice is {cells} cells, past the {MAX_COARSE_CELLS} one workgroup solves"))
}

impl Primitive for CoarsePressureSolve {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "rhs").map(|&(_, n)| n)).flatten()
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        coarse_refusal(params)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(reason) = coarse_refusal(ctx.params) {
            ctx.error(format!("Coarse Pressure Solve: {reason}"));
            return;
        }
        let nodes = cell_lattice(ctx.params).expect("checked by the refusal");
        let cell_size = ctx.scalar_or_param("cell_size", 0.5);
        if !(cell_size.is_finite() && cell_size > 0.0) {
            ctx.error("Coarse Pressure Solve: cell_size must be positive".to_string());
            return;
        }
        let sweeps = whole(ctx.params, "sweeps", 8).clamp(1, 256);
        {
            let gpu = ctx.gpu_encoder();
            if self.solve.is_none() {
                self.solve = Some(gpu.device.create_compute_pipeline(SHADER, "solve_main", "node.coarse_pressure_solve"));
            }
        }
        let (Some(water), Some(rhs), Some(out)) = (ctx.inputs.array("water"), ctx.inputs.array("rhs"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > water.size.min(rhs.size).min(out.size) {
            ctx.error(format!("Coarse Pressure Solve: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = CoarseParams {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            sweeps,
            cell_size,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            self.solve.as_ref().expect("pipeline created"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: rhs, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [1, 1, 1],
            "node.coarse_pressure_solve",
        );
    }
}

#[cfg(test)]
mod tests {
    /// The liquid conformance suite checks codegen bodies for atomics; this
    /// hand shader has no codegen body, so it is checked here.
    #[test]
    fn coarse_solve_uses_no_atomics() {
        assert!(!super::SHADER.contains("atomic"));
    }
}
