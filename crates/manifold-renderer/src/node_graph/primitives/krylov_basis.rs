//! `node.krylov_basis` — the substep boundary of a GMRES solve
//! (docs/FFT_WATER_SOLVER_DESIGN.md D10). It owns the Krylov basis and the
//! current vector as provided arrays, runs its region `passes` times with the
//! pass index, and after each pass files the new vector as the next basis
//! row. Every copy is a blit; nothing reads back to the CPU.

use manifold_gpu::GpuBuffer;

use super::krylov_givens::{pass_count, residual_offset, state_len};
use super::sort_particles_into_cells::int_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use std::borrow::Cow;

/// `next_in` closes the new vector into the loop; `last` is its final value,
/// which nothing needs to read.
const RESULTS: &[SubstepResultPorts] = &[SubstepResultPorts { capture: "next_in", output: "last" }];

/// Iteration scalars, in order: the pass index j, and j + 1 (the basis rows
/// the pass projects against).
pub const KRYLOV_BASIS_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &["pass", "rows"],
    results: RESULTS,
    // The passes are one solve, not simulated time: no host syncs.
    clock: None,
};

crate::primitive! {
    name: KrylovBasis,
    type_id: "node.krylov_basis",
    purpose: "Run a GMRES solve's passes as a substep region. Each frame: basis row 0 and `current` become `start` (the first vector, already unit length), the small solver state `out` is cleared with g[0] = seed[0] (the first vector's length), then the region runs `passes` times with the pass index and pass + 1. After each pass the new state is taken from `in` and the new vector from `next_in`, which becomes the next basis row and the next `current`. The basis is (passes + 1) rows of `row_length`, row-major.",
    inputs: {
        seed: Array(f32) required,
        start: Array(f32) required,
        in: Array(f32) required,
        next_in: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
        basis: Array(f32),
        current: Array(f32),
        last: Array(f32),
        pass: ScalarF32,
        rows: ScalarF32,
        passes: ScalarF32,
    },
    params: [
        int_param!("passes", "Passes", 24.0, 1.0, 32.0),
        int_param!("row_length", "Row Length", 1024.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The Krylov loop of the FFT water solver. seed is node.dot_products' length of the right-hand side (root 1), start is the right-hand side over node.divide_by_value. The region body applies the operator to current, projects against basis with node.dot_products and node.combine_rows (rows from `rows`), normalises into next_in, and updates the state with node.krylov_givens (column from `pass`) into in. After the loop, node.krylov_solve reads out and node.combine_rows forms the solution from basis.",
    examples: [],
    picker: { label: "Krylov Basis", category: Atom },
    summary: "Runs the pressure solver's rounds and remembers every round's result.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["gmres", "krylov", "solver loop", "arnoldi"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        basis: Option<GpuBuffer> = None,
        current: Option<GpuBuffer> = None,
        passes: u32 = 0,
        row_length: u32 = 0,
        captures: u32 = 0,
    },
}

fn row_length(params: &ParamValues) -> u32 {
    match params.get("row_length") {
        Some(ParamValue::Float(v)) => v.round().max(1.0) as u32,
        _ => 1024,
    }
}

impl Primitive for KrylovBasis {
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &["in", "next_in"]
    }

    fn persistent_output_ports(&self) -> &'static [&'static str] {
        &["out", "last"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(KRYLOV_BASIS_PORTS)
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "basis" | "current")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "basis" => self.basis.as_ref(),
            "current" => self.current.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        match port {
            "out" => Some(state_len(pass_count(params))),
            // Provided storage, allocated by run() at these sizes. Declaring
            // them keeps every downstream capacity right in the plan itself.
            "last" | "current" => Some(row_length(params)),
            "basis" => Some(row_length(params).saturating_mul(pass_count(params) + 1)),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let passes = pass_count(ctx.params);
        let length = row_length(ctx.params);
        let row_bytes = u64::from(length) * 4;
        self.passes = passes;
        self.row_length = length;
        self.captures = 0;
        ctx.outputs.set_scalar("passes", ParamValue::Float(passes as f32));
        {
            let gpu = ctx.gpu_encoder();
            let basis_bytes = row_bytes * u64::from(passes + 1);
            if self.basis.as_ref().is_none_or(|b| b.size < basis_bytes) {
                self.basis = Some(gpu.device.create_buffer(basis_bytes));
            }
            if self.current.as_ref().is_none_or(|b| b.size < row_bytes) {
                self.current = Some(gpu.device.create_buffer(row_bytes));
            }
        }
        let (Some(seed), Some(start), Some(out)) =
            (ctx.inputs.array("seed"), ctx.inputs.array("start"), ctx.outputs.array("out"))
        else {
            return;
        };
        if start.size < row_bytes || seed.size < 4 || out.size < u64::from(state_len(passes)) * 4 {
            ctx.error(format!("Krylov Basis: {passes} passes of {length} are larger than the arrays"));
            self.passes = 0;
            return;
        }
        let (basis, current) = (self.basis.as_ref().expect("basis"), self.current.as_ref().expect("current"));
        let enc = &mut *ctx.gpu_encoder().native_enc;
        enc.copy_buffer_range(start, 0, basis, 0, row_bytes);
        enc.copy_buffer_range(start, 0, current, 0, row_bytes);
        enc.clear_buffer(out);
        enc.copy_buffer_range(seed, 0, out, u64::from(residual_offset(passes)) * 4, 4);
    }

    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.passes {
            return false;
        }
        scalars[0] = iteration as f32;
        scalars[1] = (iteration + 1) as f32;
        true
    }

    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let row = self.captures + 1;
        self.captures += 1;
        let row_bytes = u64::from(self.row_length) * 4;
        let state_bytes = u64::from(state_len(self.passes)) * 4;
        let (Some(basis), Some(current)) = (self.basis.as_ref(), self.current.as_ref()) else {
            return;
        };
        let (candidate, state, next, last) = (
            ctx.inputs.array("in"),
            ctx.outputs.array("out"),
            ctx.inputs.array("next_in"),
            ctx.outputs.array("last"),
        );
        // run() checked the seed side; the captures are checked here, so a
        // short array stops the solve instead of tripping the copy asserts.
        let short = candidate.is_some_and(|c| c.size < state_bytes)
            || state.is_some_and(|s| s.size < state_bytes)
            || next.is_some_and(|n| n.size < row_bytes)
            || last.is_some_and(|l| l.size < row_bytes);
        if short {
            self.passes = 0;
            ctx.error(format!("Krylov Basis: a captured array is shorter than {} values", self.row_length));
            return;
        }
        let enc = &mut *ctx.gpu_encoder().native_enc;
        if let (Some(candidate), Some(state)) = (candidate, state)
            && !candidate.ptr_eq(state)
        {
            enc.copy_buffer_range(candidate, 0, state, 0, state_bytes);
        }
        let Some(next) = next else { return };
        if row <= self.passes {
            enc.copy_buffer_range(next, 0, basis, u64::from(row) * row_bytes, row_bytes);
        }
        enc.copy_buffer_range(next, 0, current, 0, row_bytes);
        if row == self.passes
            && let Some(last) = last
        {
            enc.copy_buffer_range(next, 0, last, 0, row_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn krylov_basis_serves_pass_and_rows() {
        let mut node = KrylovBasis::new();
        node.passes = 3;
        let mut s = [0.0f32; 2];
        let mut seen = Vec::new();
        for i in 0.. {
            if !EffectNode::substep_iteration(&mut node, i, &mut s) {
                break;
            }
            seen.push(s);
        }
        assert_eq!(seen, vec![[0.0, 1.0], [1.0, 2.0], [2.0, 3.0]]);
    }
}
