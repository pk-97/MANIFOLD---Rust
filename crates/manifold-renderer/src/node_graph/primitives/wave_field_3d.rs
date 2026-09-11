//! `node.wave_field_3d` — a point-sampled travelling sine field.

use manifold_gpu::GpuBinding;
use std::borrow::Cow;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Uniforms {
    pub(super) frequency: f32,
    pub(super) phase: f32,
    pub(super) direction_x: f32,
    pub(super) direction_y: f32,
    pub(super) direction_z: f32,
    pub(super) dispatch_count: u32,
    pub(super) _pad: [u32; 2],
}

crate::primitive! {
    name: WaveField3d,
    type_id: "node.wave_field_3d",
    purpose: "Evaluate a travelling mathematical sine field at each 3D position. out = sin(TAU * (dot(position.xyz, direction) * frequency - fract(phase))). Direction coefficients are intentionally not normalized: a zero vector is a uniform temporal pulse, and frequency zero is valid.",
    inputs: {
        positions: Array(Vec4Vertex) required,
        frequency: ScalarF32 optional,
        phase: ScalarF32 optional,
        direction_x: ScalarF32 optional,
        direction_y: ScalarF32 optional,
        direction_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("frequency"), label: "Frequency", ty: ParamType::Float, default: ParamValue::Float(0.2), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("phase"), label: "Phase", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_x"), label: "Direction X", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_y"), label: "Direction Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_z"), label: "Direction Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-100.0, 100.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows `positions`. The phase is reduced modulo one inside the formula, so phase 0 and phase 1 are identical while phase remains the only time input. Direction coefficients are not normalized by design; use a zero direction for a uniform pulse and frequency zero for a spatially uniform field.",
    examples: [],
    picker: { label: "Wave Field 3D", category: Atom },
    summary: "Samples a moving sine wave at every 3D point, producing weights for copy displacement or other maps.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["wave field 3d", "sine field", "travelling wave", "wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/wave_field_3d_body.wgsl"),
}

impl Primitive for WaveField3d {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(name, _)| *name == "positions")
            .map(|(_, capacity)| *capacity)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let frequency = ctx.scalar_or_param("frequency", 0.2);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let direction_x = ctx.scalar_or_param("direction_x", 1.0);
        let direction_y = ctx.scalar_or_param("direction_y", 0.0);
        let direction_z = ctx.scalar_or_param("direction_z", 0.0);
        let Some(positions) = ctx.inputs.array("positions") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let count = ((positions.size / std::mem::size_of::<Vec4Vertex>() as u64) as u32)
            .min((out.size / 4) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms {
            frequency,
            phase,
            direction_x,
            direction_y,
            direction_z,
            dispatch_count: count,
            _pad: [0; 2],
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
                    buffer: positions,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.wave_field_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn wave_pilot_wave_field_ports_defaults_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let prim = WaveField3d::new();
        assert_eq!(WaveField3d::TYPE_ID, "node.wave_field_3d");
        assert_eq!(
            WaveField3d::INPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<Vec4Vertex>())
        );
        assert!(WaveField3d::INPUTS[0].required);
        for name in [
            "frequency",
            "phase",
            "direction_x",
            "direction_y",
            "direction_z",
        ] {
            let port = WaveField3d::INPUTS.iter().find(|p| p.name == name).unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "out",
                &ParamValues::default(),
                &[("positions", 23)]
            ),
            Some(23)
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = WaveField3d::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wave_field_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    fn dispatch(src: &[Vec4Vertex], frequency: f32, phase: f32, direction: [f32; 3]) -> Vec<f32> {
        let device = crate::test_device();
        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<WaveField3d>().unwrap();
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "wave-pilot-field",
        );
        let input = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let output = device.create_buffer_shared(4 * src.len() as u64);
        let uniforms = Uniforms {
            frequency,
            phase,
            direction_x: direction[0],
            direction_y: direction[1],
            direction_z: direction[2],
            dispatch_count: src.len() as u32,
            _pad: [0; 2],
        };
        let mut enc = device.create_encoder("wave-pilot-field");
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
                    buffer: &output,
                    offset: 0,
                },
            ],
            [(src.len() as u32).div_ceil(256), 1, 1],
            "wave-pilot-field",
        );
        enc.commit_and_wait_completed();
        let ptr = output.mapped_ptr().unwrap();
        unsafe { std::slice::from_raw_parts(ptr as *const f32, src.len()) }.to_vec()
    }

    #[test]
    fn wave_pilot_wave_field_matches_cpu_and_phase_wrap() {
        let src = vec![
            Vec4Vertex {
                position: [0.25, 0.0, 0.0, 1.0],
            },
            Vec4Vertex {
                position: [1.5, -0.5, 2.0, 1.0],
            },
        ];
        let got0 = dispatch(&src, 0.2, 0.0, [1.0, 0.0, 0.0]);
        let got1 = dispatch(&src, 0.2, 1.0, [1.0, 0.0, 0.0]);
        for (i, p) in src.iter().enumerate() {
            let expected =
                (std::f32::consts::TAU * ((p.position[0] * 0.2) - (0.0f32 - 0.0f32))).sin();
            assert!(
                (got0[i] - expected).abs() < 1e-5,
                "sample {i}: got {} expected {expected}",
                got0[i]
            );
            assert!((got1[i] - got0[i]).abs() < 1e-5, "phase 0 and 1 must match");
        }
        let uniform = dispatch(&src, 0.0, 0.25, [0.0, 0.0, 0.0]);
        assert!(
            uniform.iter().all(|v| (*v + 1.0).abs() < 1e-5),
            "zero direction/frequency still evaluates the temporal pulse"
        );
    }
}
