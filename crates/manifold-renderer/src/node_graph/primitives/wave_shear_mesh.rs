//! `node.wave_shear_mesh` — an instantaneous analytic shear wave over a
//! triangle-list `Array<MeshVertex>` with Jacobian-aware frame transport.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHEAR_AXES: &[&str] = &["Basis X", "Basis Z"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaveShearUniforms {
    amplitude: f32,
    frequency: f32,
    phase: f32,
    yaw: f32,
    pitch: f32,
    scale: f32,
    origin_x: f32,
    origin_y: f32,
    origin_z: f32,
    enabled: f32,
    phase_offset: f32,
    axis: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: WaveShearMesh,
    type_id: "node.wave_shear_mesh",
    purpose: "Apply a pure analytic shear wave to an Array<MeshVertex>. R_y(yaw)*R_x(pitch) rotates the sampling Basis Y and displacement Basis X or Basis Z. safe_scale=max(abs(scale),1e-6), q=(position-origin)/safe_scale, f=TAU*(dot(q,sample_basis_y)*frequency-phase-phase_offset), position += safe_scale*amplitude*enabled*sin(f)*displacement_basis. Normals use inverse-transpose Jacobian transport and tangents use forward Jacobian transport followed by orthogonalization.",
    inputs: {
        in: Array(MeshVertex) required,
        amplitude: ScalarF32 optional,
        frequency: ScalarF32 optional,
        phase: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        scale: ScalarF32 optional,
        origin_x: ScalarF32 optional,
        origin_y: ScalarF32 optional,
        origin_z: ScalarF32 optional,
        enabled: ScalarF32 optional,
        phase_offset: ScalarF32 optional,
    },
    outputs: { out: Array(MeshVertex), },
    params: [
        ParamDef { name: Cow::Borrowed("amplitude"), label: "Amplitude", ty: ParamType::Float, default: ParamValue::Float(0.2), range: Some((-10.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("frequency"), label: "Frequency", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-100.0, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("phase"), label: "Phase", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yaw"), label: "Yaw", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pitch"), label: "Pitch", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scale", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-1000.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("origin_x"), label: "Origin X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("origin_y"), label: "Origin Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("origin_z"), label: "Origin Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("phase_offset"), label: "Phase Offset", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("axis"), label: "Displacement Basis", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, (SHEAR_AXES.len() - 1) as f32)), enum_values: SHEAR_AXES },
    ],
    depth_rule: Terminal,
    composition_notes: "Pure live mapping with no hidden clock. phase and phase_offset are the only phase terms. enabled <= 0 or amplitude == 0 returns the current MeshVertex exactly, including smooth normals, tangent sentinel/handedness, UV, and padding. Output capacity follows in.",
    examples: [],
    picker: { label: "Wave Shear Mesh", category: Atom },
    summary: "Shears a textured mesh with a travelling wave while transporting normals and tangents analytically.",
    category: Geometry3D,
    role: Filter,
    aliases: ["wave shear", "wave shear mesh", "shear wave", "mesh wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/wave_shear_mesh_body.wgsl"),
}

impl Primitive for WaveShearMesh {
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
            .find(|(name, _)| *name == "in")
            .map(|(_, capacity)| *capacity)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amplitude = ctx.scalar_or_param("amplitude", 0.2);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let origin_x = ctx.scalar_or_param("origin_x", 0.0);
        let origin_y = ctx.scalar_or_param("origin_y", 0.0);
        let origin_z = ctx.scalar_or_param("origin_z", 0.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);
        let phase_offset = ctx.scalar_or_param("phase_offset", 0.0);
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            _ => 0,
        };
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / vertex_size) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = WaveShearUniforms {
            amplitude,
            frequency,
            phase,
            yaw,
            pitch,
            scale,
            origin_x,
            origin_y,
            origin_z,
            enabled,
            phase_offset,
            axis,
            dispatch_count: count,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.wave_shear_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::EffectNode;

    #[test]
    fn photoscan_modifier_wave_shear_ports_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let prim = WaveShearMesh::new();
        let mesh = ArrayType::of_known::<MeshVertex>();
        let input = WaveShearMesh::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .unwrap();
        assert_eq!(input.ty, PortType::Array(mesh));
        assert!(input.required);
        for name in [
            "amplitude",
            "frequency",
            "phase",
            "yaw",
            "pitch",
            "scale",
            "origin_x",
            "origin_y",
            "origin_z",
            "enabled",
            "phase_offset",
        ] {
            let port = WaveShearMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert!(!WaveShearMesh::INPUTS.iter().any(|p| p.name == "axis"));
        assert_eq!(WaveShearMesh::OUTPUTS[0].ty, PortType::Array(mesh));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &ParamValues::default(), &[("in", 19)]),
            Some(19)
        );
    }

    #[test]
    fn photoscan_modifier_wave_shear_registers() {
        let prim = WaveShearMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wave_shear_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{
        generate_fused, FusionRegion, InputSource, RegionNode, ENTRY,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    fn vertex(position: [f32; 3], normal: [f32; 3], tangent: [f32; 4]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 17.25,
            normal,
            _pad1: -3.5,
            uv: [0.125, 0.875],
            _pad2: [11.0, -7.0],
            tangent,
        }
    }

    fn rotate_basis(v: [f32; 3], yaw: f32, pitch: f32) -> [f32; 3] {
        let (sp, cp) = pitch.sin_cos();
        let rx = [v[0], cp * v[1] - sp * v[2], sp * v[1] + cp * v[2]];
        let (sy, cy) = yaw.sin_cos();
        [cy * rx[0] + sy * rx[2], rx[1], -sy * rx[0] + cy * rx[2]]
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn length(v: [f32; 3]) -> f32 {
        dot(v, v).sqrt()
    }
    fn normalize(v: [f32; 3]) -> [f32; 3] {
        let l = length(v);
        if l > 1e-8 {
            [v[0] / l, v[1] / l, v[2] / l]
        } else {
            [0.0; 3]
        }
    }

    fn cpu_wave(v: &MeshVertex, u: WaveShearUniforms) -> MeshVertex {
        if u.enabled <= 0.0 || u.amplitude == 0.0 {
            return *v;
        }
        let sample = rotate_basis([0.0, 1.0, 0.0], u.yaw, u.pitch);
        let displacement = rotate_basis(
            if u.axis == 0 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 0.0, 1.0]
            },
            u.yaw,
            u.pitch,
        );
        let safe_scale = u.scale.abs().max(1e-6);
        let q = [
            (v.position[0] - u.origin_x) / safe_scale,
            (v.position[1] - u.origin_y) / safe_scale,
            (v.position[2] - u.origin_z) / safe_scale,
        ];
        let f = std::f32::consts::TAU * (dot(q, sample) * u.frequency - u.phase - u.phase_offset);
        let mut out = *v;
        let s = f.sin();
        out.position = [
            v.position[0] + safe_scale * u.amplitude * u.enabled * s * displacement[0],
            v.position[1] + safe_scale * u.amplitude * u.enabled * s * displacement[1],
            v.position[2] + safe_scale * u.amplitude * u.enabled * s * displacement[2],
        ];
        let base = normalize(v.normal);
        let k = u.amplitude * u.enabled * std::f32::consts::TAU * u.frequency * f.cos();
        let raw = [
            base[0] - sample[0] * k * dot(displacement, base),
            base[1] - sample[1] * k * dot(displacement, base),
            base[2] - sample[2] * k * dot(displacement, base),
        ];
        out.normal = if length(raw) > 1e-8 {
            normalize(raw)
        } else {
            base
        };
        if length([v.tangent[0], v.tangent[1], v.tangent[2]]) > 1e-8 {
            let t = [v.tangent[0], v.tangent[1], v.tangent[2]];
            let forward = [
                t[0] + displacement[0] * k * dot(sample, t),
                t[1] + displacement[1] * k * dot(sample, t),
                t[2] + displacement[2] * k * dot(sample, t),
            ];
            let tangent = [
                forward[0] - out.normal[0] * dot(out.normal, forward),
                forward[1] - out.normal[1] * dot(out.normal, forward),
                forward[2] - out.normal[2] * dot(out.normal, forward),
            ];
            if length(tangent) > 1e-8 {
                let n = normalize(tangent);
                out.tangent = [n[0], n[1], n[2], v.tangent[3]];
            }
        }
        out
    }

    fn dispatch_bytes(
        wgsl: &str,
        src: &[MeshVertex],
        uniform_bytes: &[u8],
        label: &str,
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let input = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let output = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        let mut enc = device.create_encoder(label);
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
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
            label,
        );
        enc.commit_and_wait_completed();
        let ptr = output.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    fn dispatch(
        wgsl: &str,
        src: &[MeshVertex],
        u: WaveShearUniforms,
        label: &str,
    ) -> Vec<MeshVertex> {
        dispatch_bytes(wgsl, src, bytemuck::bytes_of(&u), label)
    }

    fn fused_wgsl() -> String {
        let id = NodeInstanceId;
        let node = |node_id, input| RegionNode {
            node_id: id(node_id),
            fusion_kind: FusionKind::Pointwise,
            body: WaveShearMesh::WGSL_BODY.unwrap(),
            params: WaveShearMesh::PARAMS,
            inputs: vec![input],
            input_access: WaveShearMesh::INPUT_ACCESS.to_vec(),
            node_inputs: WaveShearMesh::INPUTS,
            node_outputs: WaveShearMesh::OUTPUTS,
            node_includes: WaveShearMesh::WGSL_INCLUDES,
            derived_uniforms: WaveShearMesh::DERIVED_UNIFORMS,
            type_id: WaveShearMesh::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba32float",
            stencil_fetch: false,
            quantize_f16: false,
        };
        let region = FusionRegion {
            nodes: vec![
                node(0, InputSource::External(0)),
                node(1, InputSource::Node(id(0))),
            ],
            num_external_inputs: 1,
            outputs: vec![(id(1), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
        };
        let generated = generate_fused(&region).expect("wave shear is a fusable buffer atom");
        assert!(
            naga::front::wgsl::parse_str(&generated.wgsl).is_ok(),
            "generated fused WGSL must parse"
        );
        generated.wgsl
    }

    #[test]
    fn photoscan_modifier_wave_shear_cpu_oracle_finite_difference_and_phasewrap() {
        let src = vec![vertex(
            [0.35, -0.2, 0.7],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, -1.0],
        )];
        let u = WaveShearUniforms {
            amplitude: 0.27,
            frequency: 0.43,
            phase: 0.19,
            yaw: 0.31,
            pitch: -0.22,
            scale: -0.8,
            origin_x: 0.1,
            origin_y: -0.3,
            origin_z: 0.4,
            enabled: 1.0,
            phase_offset: 0.07,
            axis: 1,
            dispatch_count: 1,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let got = dispatch(
            &crate::node_graph::freeze::codegen::standalone_for_spec::<WaveShearMesh>().unwrap(),
            &src,
            u,
            "photoscan-wave-shear-oracle",
        );
        let expected = cpu_wave(&src[0], u);
        for axis in 0..3 {
            assert!((got[0].position[axis] - expected.position[axis]).abs() < 2e-5);
        }
        for axis in 0..3 {
            assert!(
                (got[0].normal[axis] - expected.normal[axis]).abs() < 2e-5,
                "normal axis {axis}"
            );
        }
        for axis in 0..4 {
            assert!(
                (got[0].tangent[axis] - expected.tangent[axis]).abs() < 2e-5,
                "tangent axis {axis}"
            );
        }
        assert_eq!(got[0].uv, src[0].uv);

        // Zero scale and a degenerate input normal must still produce finite
        // output through the safe-scale and safe-normal branches.
        let mut zero_scale = src[0];
        zero_scale.normal = [0.0; 3];
        zero_scale.tangent = [0.0; 4];
        let mut zero_scale_u = u;
        zero_scale_u.scale = 0.0;
        let zero_out = dispatch(
            &crate::node_graph::freeze::codegen::standalone_for_spec::<WaveShearMesh>().unwrap(),
            &[zero_scale],
            zero_scale_u,
            "photoscan-wave-shear-zero-scale",
        );
        assert!(zero_out[0]
            .position
            .iter()
            .chain(zero_out[0].normal.iter())
            .all(|x| x.is_finite()));

        // The normal is also the finite-difference normal of the deformed
        // surface: this catches a position/Jacobian mismatch independently of
        // the CPU implementation above.
        let eps = 1e-4;
        let mut px = src[0];
        px.position[0] += eps;
        let mut py = src[0];
        py.position[1] += eps;
        let ax = cpu_wave(&px, u).position;
        let ay = cpu_wave(&py, u).position;
        let p = expected.position;
        let dx = [
            (ax[0] - p[0]) / eps,
            (ax[1] - p[1]) / eps,
            (ax[2] - p[2]) / eps,
        ];
        let dy = [
            (ay[0] - p[0]) / eps,
            (ay[1] - p[1]) / eps,
            (ay[2] - p[2]) / eps,
        ];
        let finite = normalize([
            dx[1] * dy[2] - dx[2] * dy[1],
            dx[2] * dy[0] - dx[0] * dy[2],
            dx[0] * dy[1] - dx[1] * dy[0],
        ]);
        assert!(
            dot(finite, got[0].normal).abs() > 0.9999,
            "finite difference normal disagrees"
        );

        let mut wrapped = u;
        wrapped.phase += 1.0;
        let wrapped_out = dispatch(
            &crate::node_graph::freeze::codegen::standalone_for_spec::<WaveShearMesh>().unwrap(),
            &src,
            wrapped,
            "photoscan-wave-shear-phase",
        );
        for axis in 0..3 {
            assert!((wrapped_out[0].position[axis] - got[0].position[axis]).abs() < 3e-5);
        }
    }

    #[test]
    fn photoscan_modifier_wave_shear_identity_bytes_and_generated_fused_match() {
        let src = vec![
            vertex(
                [0.5, -0.3, 1.2],
                [0.267, 0.535, 0.802],
                [0.4, 0.2, 0.8, -1.0],
            ),
            vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.0; 4]),
        ];
        let identity = WaveShearUniforms {
            amplitude: 0.8,
            frequency: 1.0,
            phase: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            scale: 0.0,
            origin_x: 0.0,
            origin_y: 0.0,
            origin_z: 0.0,
            enabled: 0.0,
            phase_offset: 0.0,
            axis: 0,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let standalone =
            crate::node_graph::freeze::codegen::standalone_for_spec::<WaveShearMesh>().unwrap();
        let identity_out = dispatch(&standalone, &src, identity, "photoscan-wave-shear-identity");
        for i in 0..src.len() {
            assert_eq!(
                bytemuck::bytes_of(&identity_out[i]),
                bytemuck::bytes_of(&src[i]),
                "identity must preserve vertex {i} byte-for-byte"
            );
        }

        let u0 = WaveShearUniforms {
            amplitude: 0.31,
            frequency: 0.67,
            phase: 0.13,
            yaw: -0.2,
            pitch: 0.17,
            scale: 0.9,
            origin_x: -0.1,
            origin_y: 0.2,
            origin_z: 0.0,
            enabled: 1.0,
            phase_offset: -0.04,
            axis: 0,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let u1 = WaveShearUniforms {
            amplitude: -0.17,
            frequency: 0.41,
            phase: -0.08,
            yaw: 0.11,
            pitch: -0.09,
            scale: 0.65,
            origin_x: 0.04,
            origin_y: -0.1,
            origin_z: 0.12,
            enabled: 1.0,
            phase_offset: 0.25,
            axis: 1,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let expected: Vec<_> = src.iter().map(|v| cpu_wave(&cpu_wave(v, u0), u1)).collect();
        let first = dispatch(&standalone, &src, u0, "photoscan-wave-shear-standalone-a");
        let standalone_out = dispatch(&standalone, &first, u1, "photoscan-wave-shear-standalone-b");
        let mut words = [0_u32; 24];
        for (slot, value) in [
            u0.amplitude,
            u0.frequency,
            u0.phase,
            u0.yaw,
            u0.pitch,
            u0.scale,
            u0.origin_x,
            u0.origin_y,
            u0.origin_z,
            u0.enabled,
            u0.phase_offset,
            0.0,
            u1.amplitude,
            u1.frequency,
            u1.phase,
            u1.yaw,
            u1.pitch,
            u1.scale,
            u1.origin_x,
            u1.origin_y,
            u1.origin_z,
            u1.enabled,
            u1.phase_offset,
            0.0,
        ]
        .iter()
        .enumerate()
        {
            words[slot] = value.to_bits();
        }
        // Enum uniforms are WGSL u32 fields; use their integer discriminants
        // rather than the IEEE bits of a float with the same displayed value.
        words[11] = u0.axis;
        words[23] = u1.axis;
        let fused_out = dispatch_bytes(
            &fused_wgsl(),
            &src,
            bytemuck::cast_slice(&words),
            "photoscan-wave-shear-fused",
        );
        for i in 0..src.len() {
            for axis in 0..3 {
                assert!(
                    (standalone_out[i].position[axis] - expected[i].position[axis]).abs() < 2e-5
                );
            }
            for axis in 0..3 {
                assert!(
                    (fused_out[i].position[axis] - standalone_out[i].position[axis]).abs() < 2e-5,
                    "fused position axis {axis} vertex {i}"
                );
            }
            for axis in 0..3 {
                assert!(
                    (fused_out[i].normal[axis] - standalone_out[i].normal[axis]).abs() < 2e-5,
                    "fused normal axis {axis} vertex {i}"
                );
            }
            assert_eq!(fused_out[i].uv, src[i].uv);
            assert_eq!(fused_out[i].tangent[3], src[i].tangent[3]);
        }
    }
}
