//! Stack-only binding/uniform packing for the whitewater control atoms.
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::{exec::effect_node::EffectNodeContext, parameters::ParamValue, primitive::Primitive};
use arrayvec::ArrayVec;
use manifold_gpu::{GpuBinding, GpuComputePipeline};

pub(super) fn run<P: Primitive>(
    ctx: &mut EffectNodeContext<'_, '_>,
    pipeline: &mut Option<GpuComputePipeline>,
    ports: &[&str],
    input_stride: u64,
    output_stride: u64,
) {
    let mut words = [0u32; 32];
    for (word, param) in words.iter_mut().zip(P::PARAMS) {
        let default = match param.default {
            ParamValue::Float(v) => v,
            _ => panic!("whitewater control uniform must be float"),
        };
        *word = ctx.scalar_or_param(&param.name, default).to_bits();
    }
    let pipeline = standalone_pipeline::<P>(pipeline, ctx.gpu_encoder().device);
    let Some(first) = ctx.inputs.array(ports[0]) else {
        return;
    };
    let Some(out) = ctx.outputs.array("out") else {
        return;
    };
    let count = first.size / input_stride;
    if out.size / output_stride < count {
        ctx.error(format!("{}: output is shorter than its source", P::TYPE_ID));
        return;
    }
    for port in ["energy", "wavecrest", "turbulence"] {
        if P::TYPE_ID == "node.turbulence_emission_count"
            && ctx.inputs.array(port).is_none_or(|b| b.size / 4 < count)
        {
            ctx.error(format!("{}: {port} is shorter than particles", P::TYPE_ID));
            return;
        }
    }
    if P::TYPE_ID == "node.turbulence_emission_count" {
        let nodes = crate::whitewater::grid_nodes(ctx);
        if crate::whitewater::grid_cells(nodes).is_none()
            || ctx
                .inputs
                .array("influence")
                .is_none_or(|b| b.size / 4 < crate::whitewater::cell_total(nodes))
        {
            ctx.error("Turbulence Emission Count: influence does not cover the lattice".to_owned());
            return;
        }
    }
    if P::TYPE_ID == "node.whitewater_influence"
        && (ctx.inputs.array("solid").is_none_or(|b| b.size / 4 < count)
            || ctx
                .inputs
                .array("source")
                .is_none_or(|b| b.size / 16 < count))
    {
        ctx.error(
            "Whitewater Influence: source and solid must cover every influence node".to_owned(),
        );
        return;
    }
    words[P::PARAMS.len()] = count as u32;
    let bytes = bytemuck::cast_slice(&words[..(P::PARAMS.len() + 1).next_multiple_of(4)]);
    let mut bindings: ArrayVec<GpuBinding<'_>, 16> = ArrayVec::new();
    bindings.push(GpuBinding::Bytes {
        binding: 0,
        data: bytes,
    });
    for (i, port) in ports.iter().enumerate() {
        let Some(buffer) = ctx.inputs.array(port) else {
            return;
        };
        bindings.push(GpuBinding::Buffer {
            binding: i as u32 + 1,
            buffer,
            offset: 0,
        });
    }
    bindings.push(GpuBinding::Buffer {
        binding: ports.len() as u32 + 1,
        buffer: out,
        offset: 0,
    });
    ctx.gpu_encoder().native_enc.dispatch_compute(
        pipeline,
        &bindings,
        [(count as u32).div_ceil(256), 1, 1],
        P::TYPE_ID,
    );
}
