//! `node.displace_copies` — move instance positions by scalar weights.

use manifold_gpu::GpuBinding;
use std::borrow::Cow;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Uniforms {
    pub(super) amount: f32,
    pub(super) direction_x: f32,
    pub(super) direction_y: f32,
    pub(super) direction_z: f32,
    pub(super) dispatch_count: u32,
    pub(super) _pad: [u32; 3],
}

crate::primitive! {
    name: DisplaceCopies,
    type_id: "node.displace_copies",
    purpose: "Displace each InstanceTransform's current xyz by amount * weight * direction. Scale, rotation, marker, inactive holes, and every other record field are preserved; zero amount and zero per-copy scale return the exact source record.",
    inputs: {
        instances: Array(InstanceTransform) required,
        weights: Array(f32) required,
        amount: ScalarF32 optional,
        direction_x: ScalarF32 optional,
        direction_y: ScalarF32 optional,
        direction_z: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef { name: Cow::Borrowed("amount"), label: "Amount", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_x"), label: "Direction X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_y"), label: "Direction Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_z"), label: "Direction Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Both array capacities must match; the planner returns no output capacity for unequal inputs so an invalid graph fails explicitly. The displacement uses the current position only and preserves scale plus all rotation/marker data. A zero-scale source is an inactive hole and is copied byte-for-byte, which keeps source liveness intact. Pair with node.copy_positions → node.wave_field_3d for a reusable wave-driven copy field.",
    examples: [],
    picker: { label: "Displace Copies", category: Atom },
    summary: "Moves copies along a chosen direction according to a scalar field while preserving their transform metadata.",
    category: Geometry3D,
    role: Filter,
    aliases: ["displace copies", "copy displacement", "weighted copies", "move copies"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/displace_copies_body.wgsl"),
}

impl Primitive for DisplaceCopies {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        let instances = input_capacities
            .iter()
            .find(|(name, _)| *name == "instances")
            .map(|(_, n)| *n)?;
        let weights = input_capacities
            .iter()
            .find(|(name, _)| *name == "weights")
            .map(|(_, n)| *n)?;
        (instances == weights).then_some(instances)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = ctx.scalar_or_param("amount", 1.0);
        let direction_x = ctx.scalar_or_param("direction_x", 0.0);
        let direction_y = ctx.scalar_or_param("direction_y", 1.0);
        let direction_z = ctx.scalar_or_param("direction_z", 0.0);
        let Some(instances) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(weights) = ctx.inputs.array("weights") else {
            return;
        };
        let Some(out) = ctx.outputs.array("instances") else {
            return;
        };
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;
        let count = ((instances.size / instance_size) as u32)
            .min((weights.size / 4) as u32)
            .min((out.size / instance_size) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms {
            amount,
            direction_x,
            direction_y,
            direction_z,
            dispatch_count: count,
            _pad: [0; 3],
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
                    buffer: instances,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.displace_copies",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn wave_pilot_displace_copies_ports_and_capacity_guard() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let prim = DisplaceCopies::new();
        let layout = ArrayType::of_known::<InstanceTransform>();
        assert_eq!(DisplaceCopies::TYPE_ID, "node.displace_copies");
        for name in ["instances", "weights"] {
            let p = DisplaceCopies::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert!(p.required);
        }
        assert_eq!(DisplaceCopies::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(DisplaceCopies::OUTPUTS[0].ty, PortType::Array(layout));
        for name in ["amount", "direction_x", "direction_y", "direction_z"] {
            let p = DisplaceCopies::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(p.ty, PortType::Scalar(ScalarType::F32));
        }
        let params = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "instances",
                &params,
                &[("instances", 4), ("weights", 4)]
            ),
            Some(4)
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "instances",
                &params,
                &[("instances", 4), ("weights", 3)]
            ),
            None
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DisplaceCopies::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.displace_copies");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    fn dispatch(
        src: &[InstanceTransform],
        weights: &[f32],
        amount: f32,
        direction: [f32; 3],
    ) -> Vec<InstanceTransform> {
        let device = crate::test_device();
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<DisplaceCopies>().unwrap();
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "wave-pilot-displace",
        );
        let input = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        let weight_buf = device.create_buffer_shared(std::mem::size_of_val(weights) as u64);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
            weight_buf.write(0, bytemuck::cast_slice(weights));
        }
        let output = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        let uniforms = Uniforms {
            amount,
            direction_x: direction[0],
            direction_y: direction[1],
            direction_z: direction[2],
            dispatch_count: src.len() as u32,
            _pad: [0; 3],
        };
        let mut enc = device.create_encoder("wave-pilot-displace");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &weight_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [(src.len() as u32).div_ceil(256), 1, 1],
            "wave-pilot-displace",
        );
        enc.commit_and_wait_completed();
        let ptr = output.mapped_ptr().unwrap();
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, src.len()) }.to_vec()
    }

    #[test]
    fn wave_pilot_displace_matches_cpu_and_preserves_zero_scale_marker() {
        let src = vec![
            InstanceTransform {
                pos_scale: [1.0, 2.0, 3.0, 2.0],
                rot_pad: [4.0, 5.0, 6.0, 17.0],
            },
            InstanceTransform {
                pos_scale: [-1.0, 8.0, 2.0, 0.0],
                rot_pad: [9.0, 10.0, 11.0, 23.0],
            },
        ];
        let got = dispatch(&src, &[0.5, 100.0], 2.0, [1.0, -0.5, 0.25]);
        assert_eq!(got[0].pos_scale, [2.0, 1.5, 3.25, 2.0]);
        assert_eq!(got[0].rot_pad, src[0].rot_pad);
        assert_eq!(bytemuck::bytes_of(&got[1]), bytemuck::bytes_of(&src[1]));
        let zero = dispatch(&src, &[3.0, 4.0], 0.0, [9.0, 8.0, 7.0]);
        assert_eq!(
            bytemuck::cast_slice::<InstanceTransform, u8>(&zero),
            bytemuck::cast_slice::<InstanceTransform, u8>(&src)
        );
    }
}
