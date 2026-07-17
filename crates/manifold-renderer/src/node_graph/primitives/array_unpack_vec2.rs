//! `node.split_xy` — split an `Array<vec2<f32>>` into two
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

/// Generated-codegen uniform layout: no params, just the codegen-injected
/// `dispatch_count` (u32, the element-count guard) + 16-byte pad. 1 word + 3
/// pad = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UnpackUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ArrayUnpackVec2,
    type_id: "node.split_xy",
    purpose: "Split an Array<vec2<f32>> into two Array<f32>s, one per component (`x`, `y`). Composition glue so Array<f32> math primitives can operate on individual axes of a vec2 wire — UV coordinates, 2D positions, 2D velocities — without each scalar-math primitive needing a vec2-input variant.",
    inputs: {
        in: Array([f32; 2]) required,
    },
    outputs: {
        x: Array(f32),
        y: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Both outputs sized to the input capacity (chain build enforces). One dispatch per frame; per-element work is two scalar writes. Pair upstream with node.grid_uv_field (extract uv.y to drive a mirror_ramp on the height axis) or any other vec2-emitting producer.",
    examples: [],
    picker: { label: "Split XY", category: Atom },
    summary: "Splits a list of 2D points into two separate number lists, one for X and one for Y. The inverse of combining them.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["split xy", "array unpack vec2", "unpack", "unzip"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/array_unpack_vec2_body.wgsl"),
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
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // MULTI-OUTPUT — the body returns a BufferOutputs struct the wrapper
            // unpacks into buf_x[idx] / buf_y[idx]). array_unpack_vec2.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.split_xy standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.split_xy",
            )
        });

        let uniforms = UnpackUniforms {
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Generated binding order matches the hand kernel: uniform(0), in(1, read),
        // x(2, read_write), y(3, read_write).
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
            "node.split_xy",
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
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(ArrayUnpackVec2::TYPE_ID, "node.split_xy");
        assert_eq!(ArrayUnpackVec2::INPUTS.len(), 1);
        assert_eq!(ArrayUnpackVec2::INPUTS[0].name, "in");
        assert!(ArrayUnpackVec2::INPUTS[0].required);
        assert_eq!(ArrayUnpackVec2::INPUTS[0].ty, PortType::Array(vec2_layout));

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
        assert_eq!(node.type_id().as_str(), "node.split_xy");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain MULTI-OUTPUT parity oracle (freeze §12). The generated
    //! kernel's body returns a `BufferOutputs` struct the wrapper unpacks into
    //! two separate output arrays. Dispatches it over a known [f32;2] input and
    //! asserts x[i] == v[i].x and y[i] == v[i].y per element.
    use super::*;

    #[test]
    fn generated_unpack_splits_vec2_into_two_arrays() {
        let device = crate::test_device();
        let input: Vec<[f32; 2]> = vec![
            [0.0, 1.0],
            [2.5, -3.5],
            [10.0, 0.25],
            [-1.0, 7.0],
            [0.125, -0.125],
        ];
        let n = input.len() as u32;

        let in_buf = device.create_buffer_shared(std::mem::size_of_val(input.as_slice()) as u64);
        let x_buf = device.create_buffer_shared((input.len() * 4) as u64);
        let y_buf = device.create_buffer_shared((input.len() * 4) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(&input));
        }

        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<ArrayUnpackVec2>()
            .expect("array_unpack_vec2 codegen");
        assert!(gen_wgsl.contains("struct BufferOutputs"), "multi-output struct emitted");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "unpack-oracle",
        );

        let uniforms = UnpackUniforms {
            dispatch_count: n,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device.create_encoder("unpack-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &x_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &y_buf, offset: 0 },
            ],
            [n.div_ceil(256), 1, 1],
            "unpack-oracle",
        );
        enc.commit_and_wait_completed();

        let xptr = x_buf.mapped_ptr().expect("shared x buffer");
        let yptr = y_buf.mapped_ptr().expect("shared y buffer");
        let xs = unsafe { std::slice::from_raw_parts(xptr as *const f32, input.len()) };
        let ys = unsafe { std::slice::from_raw_parts(yptr as *const f32, input.len()) };
        for (i, v) in input.iter().enumerate() {
            assert_eq!(xs[i], v[0], "x[{i}]");
            assert_eq!(ys[i], v[1], "y[{i}]");
        }
    }
}
