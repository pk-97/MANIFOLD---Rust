//! `node.zero_array` — clear a signed-integer array in place. Resets the
//! matter grid's fixed-point accumulator before each substep's scatter
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 (One substep) step 1).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::standalone_pipeline;

/// Generated uniform: no params, then `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ZeroUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ZeroArray,
    type_id: "node.zero_array",
    purpose: "Set every element of a signed-integer array to zero, in place. Clears an accumulator before a scatter adds into it again.",
    inputs: {
        in: Array(i32) required,
    },
    outputs: {
        out: Array(i32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Aliased in/out: the array is cleared in place and keeps its identity, so a scatter downstream adds into the same buffer. In a substep region, clear the grid accumulator at the start of every substep, before node.matter_to_grid.",
    examples: ["WaterDamBreakMatter"],
    picker: { label: "Clear Array", category: Atom },
    summary: "Resets a list of whole numbers to zero so it can be added into again.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["zero", "clear", "reset array", "clear accumulator"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/zero_array_body.wgsl"),
}

impl Primitive for ZeroArray {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // In place on the input; the GPU is touched on every path.
        let array = ctx.inputs.array("in");
        let gpu = ctx.gpu_encoder();
        let Some(array) = array else {
            return;
        };
        let count = (array.size / 4) as u32;
        if count == 0 {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = ZeroUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: array, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: array, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.zero_array",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn matter_zero_array_generates_an_in_place_i32_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ZeroArray>()
            .expect("zero_array codegen");
        assert!(wgsl.contains("var<storage, read> buf_in: array<i32>"), "{wgsl}");
        assert!(wgsl.contains("var<storage, read_write> buf_out: array<i32>"), "{wgsl}");
        assert!(wgsl.contains("dispatch_count: u32"), "{wgsl}");
        assert_eq!(std::mem::size_of::<ZeroUniforms>(), 16);
        assert_eq!(ZeroArray::TYPE_ID, "node.zero_array");
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
