//! `node.conjugate_gradient` — the substep boundary of a preconditioned
//! conjugate gradient solve (docs/GPU_FLIP_PRESSURE_SOLVE.md). It holds the
//! solve's vectors between iterations and runs its region a fixed number of
//! times; nothing reads back to the CPU. Every copy is a blit.

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use std::borrow::Cow;

use super::sort_particles_into_cells::int_param;

const RESULTS: &[SubstepResultPorts] = &[
    SubstepResultPorts { capture: "solution_in", output: "solution" },
    SubstepResultPorts { capture: "direction_in", output: "direction" },
    SubstepResultPorts { capture: "rz_in", output: "rz" },
];

/// The residual is the region's primary state; the solution, the search
/// direction and r·z ride beside it. No iteration scalars: every iteration
/// runs the same body.
pub const CONJUGATE_GRADIENT_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "rhs",
    capture: "residual_in",
    state: "residual",
    iteration_scalars: &[],
    results: RESULTS,
    // One solve, not simulated time: no host syncs.
    clock: None,
};

/// (capture, output) pairs late_capture copies, the primary state first.
const CARRIED: [(&str, &str); 4] =
    [("residual_in", "residual"), ("solution_in", "solution"), ("direction_in", "direction"), ("rz_in", "rz")];

crate::primitive! {
    name: ConjugateGradient,
    type_id: "node.conjugate_gradient",
    purpose: "Run a preconditioned conjugate gradient solve's iterations as a substep region. Each evaluation: residual becomes rhs, and solution, direction and rz (the last r·z, one value) become zero; then the region runs `iterations` times, and after each the new residual, solution, direction and rz are taken from residual_in, solution_in, direction_in and rz_in. solution leaves the region as the answer.",
    inputs: {
        rhs: Array(f32) required,
        residual_in: Array(f32) required,
        solution_in: Array(f32) required,
        direction_in: Array(f32) required,
        rz_in: Array(f32) required,
    },
    outputs: {
        residual: Array(f32),
        solution: Array(f32),
        direction: Array(f32),
        rz: Array(f32),
    },
    params: [
        int_param!("iterations", "Iterations", 8.0, 1.0, 64.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The loop of the water's multigrid pressure solve. One iteration's body: z = the V-cycle of residual; r·z by node.dot_products into rz_in; β = r·z / rz by node.divide_by_value (0 on the first iteration, when rz is zero); direction_in = z + β · direction by node.combine_rows; s = −L direction by node.pressure_residual with rhs zero; α = r·z / (direction_in · s); solution_in = solution − α · direction_in and residual_in = residual − α · s, both node.combine_rows with scale −1.",
    examples: [],
    picker: { label: "Conjugate Gradient", category: Atom },
    summary: "Runs the pressure solver's rounds, each one closing in on the answer.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["cg", "pcg", "mgpcg", "solver loop", "iterative solve"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        iterations: u32 = 0,
    },
}

fn iterations(params: &ParamValues) -> u32 {
    match params.get("iterations") {
        Some(ParamValue::Float(v)) => v.round().max(1.0) as u32,
        _ => 8,
    }
}

impl Primitive for ConjugateGradient {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["residual_in", "solution_in", "direction_in", "rz_in"]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &["residual", "solution", "direction", "rz"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(CONJUGATE_GRADIENT_PORTS)
    }

    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        match port {
            "rz" => Some(1),
            "residual" | "solution" | "direction" => inputs.iter().find(|(name, _)| *name == "rhs").map(|&(_, n)| n),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.iterations = 0;
        let (Some(rhs), Some(residual), Some(solution), Some(direction), Some(rz)) = (
            ctx.inputs.array("rhs"),
            ctx.outputs.array("residual"),
            ctx.outputs.array("solution"),
            ctx.outputs.array("direction"),
            ctx.outputs.array("rz"),
        ) else {
            return;
        };
        if residual.size.min(solution.size).min(direction.size) < rhs.size || rz.size < 4 {
            ctx.error(format!("Conjugate Gradient: its vectors are shorter than the {} bytes of rhs", rhs.size));
            return;
        }
        let enc = &mut *ctx.gpu_encoder().native_enc;
        enc.copy_buffer_to_buffer(rhs, residual, rhs.size);
        enc.clear_buffer(solution);
        enc.clear_buffer(direction);
        enc.clear_buffer(rz);
        self.iterations = iterations(ctx.params);
    }

    fn substep_iteration(&mut self, iteration: u32, _scalars: &mut [f32]) -> bool {
        iteration < self.iterations
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        for (capture, state) in CARRIED {
            let (Some(candidate), Some(state)) = (ctx.inputs.array(capture), ctx.outputs.array(state)) else {
                continue;
            };
            if candidate.ptr_eq(state) {
                continue;
            }
            if candidate.size < state.size {
                self.iterations = 0;
                ctx.error(format!("Conjugate Gradient: {capture} is shorter than the vector it carries"));
                return;
            }
            ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(candidate, state, state.size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn conjugate_gradient_runs_its_iterations() {
        let mut node = ConjugateGradient::new();
        node.iterations = 3;
        let mut scalars = [0.0f32; 0];
        let runs = (0..).take_while(|&i| EffectNode::substep_iteration(&mut node, i, &mut scalars)).count();
        assert_eq!(runs, 3);
        assert!(CONJUGATE_GRADIENT_PORTS.iteration_scalars.is_empty());
    }
}
