//! `node.divide_by_value` — divide every element by one value held on the
//! GPU, such as a ratio of two node.dot_products sums. A per-element atom on
//! the codegen path.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: no params, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DivideByValueUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: DivideByValue,
    type_id: "node.divide_by_value",
    purpose: "Divide every element of an Array<f32> by divisor[0], a value computed on the GPU (a length from node.dot_products). A divisor under 1e-30 in size gives zeros instead of infinities, so a solve that has already converged carries zeros forward.",
    inputs: {
        values: Array(f32) required,
        divisor: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "node.dot_products (root 1) → divide_by_value normalises a vector without reading its length back to the CPU.",
    examples: [],
    picker: { label: "Divide By Value", category: Atom },
    summary: "Divides a list of numbers by one number the GPU just worked out.",
    category: MathAndConvert,
    role: Map,
    aliases: ["normalize", "normalise", "scale by inverse"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/divide_by_value_body.wgsl"),
    input_access: [Coincident, BufferGather],
    // The output follows values; the one-element divisor must not count it.
    output_capacity: FusedOutputCapacity::FromInput { input: "values" },
}

impl Primitive for DivideByValue {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(divisor), Some(out)) =
            (ctx.inputs.array("values"), ctx.inputs.array("divisor"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (values.size.min(out.size) / 4) as u32;
        if divisor.size < 4 {
            ctx.error("Divide By Value: the divisor array is empty".to_string());
            return;
        }
        let uniforms = DivideByValueUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: divisor, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.divide_by_value",
        );
    }
}
