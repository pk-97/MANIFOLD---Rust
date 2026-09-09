//! `node.water_commit` — S4 solver stage: fault-gated accept.
//!
//! Copies the candidate particle state to the accepted buffer only while
//! the sticky status word is clean; on any fault the accepted state is
//! retained byte-for-byte (design step 7 — a numerical failure keeps the
//! last valid state, it never partially applies). The output aliases the
//! accepted wire: the body is a pure per-element select (reads element idx,
//! writes element idx), so the in-place update is race-free.
//!
//! Contract: docs/WATER_SIMULATION_DESIGN.md sections 3 and 5;
//! docs/WATER_IMPLEMENTATION_PLAN.md section 2.2 (stage port table).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;

/// Generated-codegen uniform layout: only the codegen-injected
/// `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CommitUniforms {
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

crate::primitive! {
    name: WaterCommit,
    type_id: "node.water_commit",
    purpose: "Commit the Live Water candidate state (design step 7): after water_validate has OR'd its findings into the sticky status word, copy candidate to accepted only when the word is clean — on any fault the accepted buffer is retained byte-for-byte and the candidate is discarded. Fault retention is same-substep: accepted is immutable until this stage completes, which is why the candidate/accepted buffers never alias. The output aliases the accepted wire (a pure per-element select: read element idx, write element idx), so the in-place update is race-free. Subsequent substeps with a latched fault are no-ops by the region clock, not by this stage.",
    inputs: {
        accepted: Array(WaterParticle) required,
        candidate: Array(WaterParticle) required,
        status: Array(u32) required,
    },
    outputs: {
        out: Array(WaterParticle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Last stage of the repeated water region body. Wire status from water_validate's aliased status_out (the same sticky word the scatter stages fault into). Outside the region, only the water state boundary reads the committed state — intermediate wires never escape.",
    examples: [],
    picker: { label: "Water Commit", category: Atom },
    summary: "Accepts the new water state when validation is clean, or keeps the last good state when anything faulted.",
    category: Particles3D,
    role: Filter,
    aliases: ["water commit", "commit water", "water accept"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/water_commit_body.wgsl"),
    input_access: [Coincident, Coincident, BufferGather],
}

impl Primitive for WaterCommit {
    /// The commit updates the accepted buffer in place — safe because the
    /// body reads element idx and writes element idx, never a neighbour.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("accepted", "out")]
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "accepted")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(accepted) = ctx.inputs.array("accepted") else {
            return;
        };
        let Some(candidate) = ctx.inputs.array("candidate") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let Some(status) = ctx.inputs.array("status") else {
            return;
        };
        // Full capacity: live slots are selected, inactive slots pass through
        // (both inputs carry zeroed tails from the seed).
        let capacity = (accepted
            .size
            .min(candidate.size)
            .min(out_buf.size)
            / std::mem::size_of::<WaterParticle>() as u64) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel
            // is generated from the `wgsl_body`.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.water_commit standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.water_commit",
            )
        });

        let uniforms = CommitUniforms {
            dispatch_count: capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // uniform(0), accepted(1), candidate(2), status(3), out(4).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accepted,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: candidate,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: status,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.water_commit",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn commit_declares_accepted_candidate_status_in_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<WaterParticle>();
        let u32_layout = ArrayType::of_known::<u32>();
        assert_eq!(WaterCommit::TYPE_ID, "node.water_commit");
        let names: Vec<&str> = WaterCommit::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["accepted", "candidate", "status"]);
        assert_eq!(WaterCommit::INPUTS[2].ty, PortType::Array(u32_layout));
        assert_eq!(WaterCommit::OUTPUTS.len(), 1);
        assert_eq!(WaterCommit::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn commit_aliases_accepted_wire() {
        let prim = WaterCommit::new();
        assert_eq!(
            crate::node_graph::primitive::Primitive::aliased_array_io(&prim),
            &[("accepted", "out")]
        );
    }

    #[test]
    fn commit_registers_as_palette_atom() {
        let prim = WaterCommit::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.water_commit");
    }

    #[test]
    fn commit_codegen_binds_status_gather() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<WaterCommit>()
            .expect("node.water_commit standalone codegen");
        assert!(wgsl.contains("var<storage, read> buf_status: array<u32>"));
        assert!(wgsl.contains("struct Element"));
    }
}
