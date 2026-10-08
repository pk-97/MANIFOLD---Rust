use crate::testkit::liquid_surface::{Harness, read};
use crate::{exec::effect_node::NodeInstanceId, exec::effect_node::ParamValues, primitive::Primitive, primitive::PrimitiveSpec, parameters::ParamValue, ports::KnownItem};
use crate::freeze::{classify::CapacityExpr, codegen::FusionRegion, codegen::InputSource, codegen::RegionNode, codegen::generate_fused};
use manifold_gpu::{GpuBinding, GpuBuffer};
pub fn member<P: PrimitiveSpec>(id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
    RegionNode {
        node_id: NodeInstanceId(id),
        fusion_kind: P::FUSION_KIND,
        body: P::WGSL_BODY.unwrap(),
        params: P::PARAMS,
        inputs,
        input_access: P::INPUT_ACCESS.to_vec(),
        node_inputs: P::INPUTS,
        node_outputs: P::OUTPUTS,
        node_includes: P::WGSL_INCLUDES,
        derived_uniforms: P::DERIVED_UNIFORMS,
        type_id: P::TYPE_ID.to_string(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    }
}
pub fn fused<T: bytemuck::Pod + KnownItem>(
    h: &mut Harness,
    nodes: Vec<RegionNode<'_>>,
    external: &[&GpuBuffer],
    count: usize,
    values: &[(&str, f32)],
) -> Vec<T> {
    let last = nodes.last().unwrap().node_id;
    let region = FusionRegion {
        nodes,
        num_external_inputs: external.len(),
        outputs: vec![(last, "out".to_owned())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: vec![],
        sampled_externals: vec![],
        camera_externals: 0,
        output_capacity: Some(CapacityExpr::Slot(0)),
    };
    let generated = generate_fused(&region).unwrap();
    let mut words: Vec<u32> = generated
        .param_order
        .iter()
        .map(|(node, name)| {
            let param = region
                .nodes
                .iter()
                .find(|n| n.node_id == *node)
                .unwrap()
                .params
                .iter()
                .find(|p| p.name == *name)
                .unwrap();
            let value = values
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| match param.default {
                    ParamValue::Float(v) => v,
                    _ => panic!("unexpected uniform {name}"),
                });
            match param.ty {
                crate::parameters::ParamType::Int => value as i32 as u32,
                _ => value.to_bits(),
            }
        })
        .collect();
    words.resize(words.len().next_multiple_of(4), 0);
    let output = h.array::<T>(&[], count);
    let pipeline = h.device.create_compute_pipeline(
        &generated.wgsl,
        crate::freeze::codegen::ENTRY,
        "whitewater-reference-fused",
    );
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(&words),
    }];
    for (i, b) in external.iter().enumerate() {
        bindings.push(GpuBinding::Buffer {
            binding: i as u32 + 1,
            buffer: b,
            offset: 0,
        });
    }
    bindings.push(GpuBinding::Buffer {
        binding: external.len() as u32 + 1,
        buffer: &output.1,
        offset: 0,
    });
    let mut enc = h.device.create_encoder("whitewater-reference-fused");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [(count as u32).div_ceil(256), 1, 1],
        "whitewater-reference-fused",
    );
    enc.commit_and_wait_completed();
    read(&output.1, count)
}
pub fn run<P: Primitive, T: bytemuck::Pod + crate::ports::KnownItem>(
    harness: &mut Harness,
    prim: &mut P,
    inputs: &[(&'static str, crate::bindings::Slot)],
    len: usize,
    step_params: &ParamValues,
) -> Vec<T> {
    let out = harness.array::<T>(&[], len);
    let (_, errors) = harness.run(prim, inputs, &[("out", out.0)], step_params);
    assert!(errors.is_empty(), "{errors:?}");
    read(&out.1, len)
}