//! `node.mix_arrays` — display-time interpolation of two GPU f32 arrays.
//!
//! This is the GPU sibling of the `Mix` operation in `node.array_math`.
//! `array_math` intentionally stays CPU-side so curve consumers can observe
//! same-frame writes; this atom keeps the particle-surface solid interpolation
//! on the GPU and remains eligible for buffer fusion.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: `amount`, then the injected element
/// count and padding required by the buffer standalone wrapper.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    amount: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: MixArrays,
    type_id: "node.mix_arrays",
    purpose: "Interpolate two equal-capacity GPU Array<f32> buffers element by element: out = a + (b - a) * amount. The amount port shadows the 0..1 display control and is clamped so interpolated solids stay between the two frames.",
    inputs: {
        a: Array(f32) required,
        b: Array(f32) required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the two display-time solid frames into `a` and `b` and the frame interpolation coefficient into `amount`. The arrays must have equal capacities; the output follows `a`. This is the GPU-safe replacement for running node.array_math Mix on a producer buffer that may still be in flight.",
    examples: [],
    picker: { label: "Mix Arrays", category: Atom },
    summary: "Blends two GPU lists of numbers element by element for display-time interpolation.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["mix arrays", "array lerp", "solid interpolation", "GPU array blend"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/mix_arrays_body.wgsl"),
    input_access: [Coincident, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "a" },
}

impl Primitive for MixArrays {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let a = input_capacities
            .iter()
            .find(|(name, _)| *name == "a")
            .map(|(_, n)| *n)?;
        // `FromInput { input: "a" }` is the declared fused extent. The
        // runtime checks the equal-capacity contract before dispatch; doing
        // that here would make the freeze compiler's synthetic ascending /
        // descending capacity probes refuse an otherwise valid region.
        Some(a)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = ctx.scalar_or_param("amount", 0.5).clamp(0.0, 1.0);
        let (Some(a), Some(b), Some(out)) = (
            ctx.inputs.array("a"),
            ctx.inputs.array("b"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };

        if a.size != b.size {
            ctx.error(format!(
                "Mix Arrays: input capacities must match (a={} bytes, b={} bytes)",
                a.size, b.size
            ));
            return;
        }
        if out.size != a.size {
            ctx.error(format!(
                "Mix Arrays: output capacity must follow a (a={} bytes, out={} bytes)",
                a.size, out.size
            ));
            return;
        }
        let count = (a.size / std::mem::size_of::<f32>() as u64) as u32;
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms {
            amount,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: a,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: b,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.mix_arrays",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::standalone_for_spec;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mix_arrays_generated_wgsl_validates() {
        let wgsl = standalone_for_spec::<MixArrays>().expect("mix_arrays codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("var<storage, read> buf_a: array<f32>"));
        assert!(wgsl.contains("var<storage, read> buf_b: array<f32>"));
        assert!(wgsl.contains("var<storage, read_write> buf_out: array<f32>"));
        assert!(wgsl.contains("dispatch_count: u32"));
    }

    #[test]
    fn mix_arrays_metadata_and_capacity_contract() {
        assert_eq!(MixArrays::TYPE_ID, "node.mix_arrays");
        assert_eq!(MixArrays::FUSION_KIND, FusionKind::MultiInputCoincident);
        assert_eq!(
            MixArrays::INPUT_ACCESS,
            &[InputAccess::Coincident, InputAccess::Coincident]
        );
        assert_eq!(
            MixArrays::FUSED_OUTPUT_CAPACITY,
            FusedOutputCapacity::FromInput { input: "a" }
        );
        assert_eq!(MixArrays::PARAMS.len(), 1);
        assert_eq!(MixArrays::PARAMS[0].name, "amount");
        assert_eq!(MixArrays::PARAMS[0].range, Some((0.0, 1.0)));

        let node = MixArrays::new();
        assert_eq!(
            node.array_output_capacity("out", &ParamValues::default(), &[("a", 8), ("b", 8)]),
            Some(8)
        );
        assert_eq!(
            node.array_output_capacity("out", &ParamValues::default(), &[("a", 8), ("b", 7)]),
            Some(8)
        );
        assert_eq!(
            node.array_output_capacity("other", &ParamValues::default(), &[("a", 8), ("b", 8)]),
            None
        );
    }

    #[test]
    fn mix_arrays_two_node_graph_reports_one_fusion_region() {
        use crate::testkit::substep_nodes::register_substep_test_nodes;
        use crate::node_graph::{PrimitiveRegistry, fusion_report};
        use manifold_core::effect_graph_def::EffectGraphDef;

        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "a", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 8}}},
                {"id": 1, "nodeId": "b", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 8}}},
                {"id": 2, "nodeId": "c", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 8}}},
                {"id": 3, "nodeId": "first", "typeId": "node.mix_arrays", "params": {"amount": {"type": "Float", "value": 0.25}}},
                {"id": 4, "nodeId": "second", "typeId": "node.mix_arrays", "params": {"amount": {"type": "Float", "value": 0.75}}},
                {"id": 5, "nodeId": "sink", "typeId": "test.value_sink"},
                {"id": 6, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "a"},
                {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "b"},
                {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "a"},
                {"fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "b"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "values"},
                {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"}
            ]
        }))
        .expect("two mix graph definition");

        let report = fusion_report(&def, &registry);
        assert!(
            report.preparation_error.is_none(),
            "fusion preparation failed: {:?}",
            report.preparation_error
        );
        let mixes: Vec<_> = report
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.mix_arrays")
            .collect();
        assert_eq!(
            mixes.len(),
            2,
            "report should contain both mix nodes: {:?}",
            report.nodes
        );
        assert!(
            mixes.iter().all(|node| node.fused),
            "both mix nodes should fuse: {mixes:?}"
        );
        assert_eq!(mixes[0].region_index, mixes[1].region_index);
        let region = &report.regions[mixes[0].region_index.expect("mix region")];
        assert_eq!(region.member_node_ids, vec![3, 4]);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_gpu::{GpuBuffer, GpuDevice};

    fn shared(device: &GpuDevice, data: &[f32]) -> GpuBuffer {
        let buffer = device.create_buffer_shared(std::mem::size_of_val(data).max(16) as u64);
        // SAFETY: no GPU work references this newly created shared buffer.
        unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
        buffer
    }

    fn read(buffer: &GpuBuffer, count: usize) -> Vec<f32> {
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the dispatch completed and `count` elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), count).to_vec() }
    }

    fn expected(a: &[f32], b: &[f32], amount: f32) -> Vec<f32> {
        let t = amount.clamp(0.0, 1.0);
        a.iter().zip(b).map(|(&a, &b)| a + (b - a) * t).collect()
    }

    fn standalone(device: &GpuDevice, a: &[f32], b: &[f32], amount: f32) -> Vec<f32> {
        assert_eq!(a.len(), b.len());
        let pipeline = device.create_compute_pipeline(
            &standalone_for_spec::<MixArrays>().expect("mix_arrays standalone codegen"),
            ENTRY,
            "mix-arrays-standalone",
        );
        let a_buf = shared(device, a);
        let b_buf = shared(device, b);
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(a).max(16) as u64);
        let uniforms = Uniforms {
            amount,
            dispatch_count: a.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let mut enc = device.create_encoder("mix-arrays-standalone");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &a_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &b_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &out_buf,
                    offset: 0,
                },
            ],
            [(a.len() as u32).div_ceil(256), 1, 1],
            "mix-arrays-standalone",
        );
        enc.commit_and_wait_completed();
        read(&out_buf, a.len())
    }

    #[test]
    fn mix_arrays_matches_cpu_formula_and_clamps_amount() {
        let device = crate::test_device();
        let a = [0.0, 1.0, -2.0, 10.0, 0.25, -0.75, 4.0, 8.0];
        let b = [2.0, -1.0, 6.0, -2.0, 1.25, 0.75, -4.0, 0.0];
        for amount in [-0.5, 0.0, 0.25, 0.5, 1.0, 1.5] {
            let got = standalone(&device, &a, &b, amount);
            let want = expected(&a, &b, amount);
            for (i, (&actual, &expected)) in got.iter().zip(&want).enumerate() {
                assert!(
                    (actual - expected).abs() < 1e-6,
                    "amount={amount} index={i}: got {actual}, expected {expected}"
                );
            }
        }
    }

    fn region_node(id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
        RegionNode {
            node_id: NodeInstanceId(id),
            fusion_kind: FusionKind::MultiInputCoincident,
            body: MixArrays::WGSL_BODY.unwrap(),
            params: MixArrays::PARAMS,
            inputs,
            input_access: MixArrays::INPUT_ACCESS.to_vec(),
            node_inputs: MixArrays::INPUTS,
            node_outputs: MixArrays::OUTPUTS,
            node_includes: MixArrays::WGSL_INCLUDES,
            derived_uniforms: MixArrays::DERIVED_UNIFORMS,
            type_id: MixArrays::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }
    }

    #[test]
    fn mix_arrays_mix_arrays_fused_matches_unfused() {
        let device = crate::test_device();
        let a = [0.0, 1.0, -2.0, 10.0, 0.25, -0.75, 4.0, 8.0];
        let b = [2.0, -1.0, 6.0, -2.0, 1.25, 0.75, -4.0, 0.0];
        let c = [-1.0, 3.0, 2.0, 4.0, -0.25, 1.75, 9.0, -8.0];
        let amount_a = 0.25;
        let amount_b = 0.75;

        let region = FusionRegion {
            nodes: vec![
                region_node(0, vec![InputSource::External(0), InputSource::External(1)]),
                region_node(
                    1,
                    vec![
                        InputSource::Node(NodeInstanceId(0)),
                        InputSource::External(2),
                    ],
                ),
            ],
            num_external_inputs: 3,
            outputs: vec![(NodeInstanceId(1), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let fused = generate_fused(&region).expect("mix_arrays chain should fuse");
        let module = naga::front::wgsl::parse_str(&fused.wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&fused.wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&fused.wgsl)));

        let first = standalone(&device, &a, &b, amount_a);
        let unfused = standalone(&device, &first, &c, amount_b);

        let a_buf = shared(&device, &a);
        let b_buf = shared(&device, &b);
        let c_buf = shared(&device, &c);
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(&a) as u64);
        let mut words = vec![amount_a.to_bits(), amount_b.to_bits()];
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }
        let pipeline = device.create_compute_pipeline(&fused.wgsl, ENTRY, "mix-arrays-fused");
        let mut enc = device.create_encoder("mix-arrays-fused");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &a_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &b_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &c_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &out_buf,
                    offset: 0,
                },
            ],
            [(a.len() as u32).div_ceil(256), 1, 1],
            "mix-arrays-fused",
        );
        enc.commit_and_wait_completed();
        let fused_result = read(&out_buf, a.len());
        for (i, (&actual, &expected)) in fused_result.iter().zip(&unfused).enumerate() {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "index {i}: fused differs from unfused"
            );
        }
        assert_eq!(
            fused_result
                .iter()
                .map(|&v| v.to_bits())
                .collect::<Vec<_>>(),
            expected(&expected(&a, &b, amount_a), &c, amount_b)
                .into_iter()
                .map(f32::to_bits)
                .collect::<Vec<_>>(),
            "fused chain differs from CPU reference"
        );
    }
}
