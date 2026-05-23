//! `node.array_unpack_vec2` — split an `Array<vec2<f32>>` into two
//! `Array<f32>`s, one per component.
//!
//! Composition glue: lets `Array<f32>` math primitives (mirror ramp,
//! shape_pow_clip, multiply, …) operate on individual components of
//! a vec2 wire — UV coordinates, 2D positions, 2D velocities — without
//! teaching each scalar-math primitive about vec2 input types.
//!
//! Both outputs are sized to the input capacity (chain build enforces).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UnpackUniforms {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ArrayUnpackVec2,
    type_id: "node.array_unpack_vec2",
    purpose: "Split an Array<vec2<f32>> into two Array<f32>s, one per component (`x`, `y`). Composition glue so Array<f32> math primitives can operate on individual axes of a vec2 wire — UV coordinates, 2D positions, 2D velocities — without each scalar-math primitive needing a vec2-input variant.",
    inputs: {
        in: Array([f32; 2]) required,
    },
    outputs: {
        x: Array(f32),
        y: Array(f32),
    },
    params: [],
    composition_notes: "Both outputs sized to the input capacity (chain build enforces). One dispatch per frame; per-element work is two scalar writes. Pair upstream with node.grid_uv_field (extract uv.y to drive a mirror_ramp on the height axis) or any other vec2-emitting producer.",
    examples: [],
    picker: { label: "Array Unpack Vec2", category: Atom },
}

impl Primitive for ArrayUnpackVec2 {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "x" && port_name != "y" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_x) = ctx.outputs.array("x") else {
            return;
        };
        let Some(out_y) = ctx.outputs.array("y") else {
            return;
        };

        let vec2_size = std::mem::size_of::<[f32; 2]>() as u64;
        let f32_size = std::mem::size_of::<f32>() as u64;
        let in_capacity = (in_buf.size / vec2_size) as u32;
        let x_capacity = (out_x.size / f32_size) as u32;
        let y_capacity = (out_y.size / f32_size) as u32;
        let count = in_capacity.min(x_capacity).min(y_capacity);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/array_unpack_vec2.wgsl"),
                "cs_main",
                "node.array_unpack_vec2",
            )
        });

        let uniforms = UnpackUniforms {
            count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_x,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_y,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.array_unpack_vec2",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn array_unpack_vec2_declares_vec2_in_two_f32_out() {
        use crate::node_graph::ports::{ArrayType, ItemKind, PortType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(ArrayUnpackVec2::TYPE_ID, "node.array_unpack_vec2");
        assert_eq!(ArrayUnpackVec2::INPUTS.len(), 1);
        assert_eq!(ArrayUnpackVec2::INPUTS[0].name, "in");
        assert!(ArrayUnpackVec2::INPUTS[0].required);
        assert_eq!(ArrayUnpackVec2::INPUTS[0].ty, PortType::Array(vec2_layout));
        assert_eq!(vec2_layout.item_kind, ItemKind::Vec2Slot);

        assert_eq!(ArrayUnpackVec2::OUTPUTS.len(), 2);
        assert_eq!(ArrayUnpackVec2::OUTPUTS[0].name, "x");
        assert_eq!(ArrayUnpackVec2::OUTPUTS[0].ty, PortType::Array(f32_layout));
        assert_eq!(ArrayUnpackVec2::OUTPUTS[1].name, "y");
        assert_eq!(ArrayUnpackVec2::OUTPUTS[1].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn array_unpack_vec2_outputs_match_input_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ArrayUnpackVec2::new();
        let params = ParamValues::default();
        let inputs = [("in", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "x", &params, &inputs),
            Some(160_000),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "y", &params, &inputs),
            Some(160_000),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "other", &params, &inputs),
            None,
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ArrayUnpackVec2::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.array_unpack_vec2");
    }
}
