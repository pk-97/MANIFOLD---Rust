//! `node.transform_mesh_patches` — fixed-cell rigid transforms for a reference
//! triangle stream. Spatial cells are not topology or adjacency information.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformMeshPatchesUniforms {
    separation: f32,
    rotation: f32,
    orbit: f32,
    spread: f32,
    phase: f32,
    frequency: f32,
    yaw: f32,
    pitch: f32,
    cell_size: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    enabled: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: TransformMeshPatches,
    type_id: "node.transform_mesh_patches",
    purpose: "Apply one shared rigid transform to each reference triangle's fixed spatial cell. Reference centroids plus source offset are normalized by scale and quantized into cell_size cubes; all three corners of a triangle use the same cell center, so faces stay rigid. Current vertices are coincident; reference vertices are BufferGather. UVs, padding, smooth normals, tangent xyz, and tangent.w are preserved.",
    inputs: {
        in: Array(MeshVertex) required,
        reference: Array(MeshVertex) required,
        separation: ScalarF32 optional,
        rotation: ScalarF32 optional,
        orbit: ScalarF32 optional,
        spread: ScalarF32 optional,
        phase: ScalarF32 optional,
        frequency: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
        enabled: ScalarF32 optional,
    },
    outputs: { out: Array(MeshVertex), },
    params: [
        ParamDef { name: Cow::Borrowed("separation"), label: "Separation", ty: ParamType::Float, default: ParamValue::Float(0.15), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rotation"), label: "Rotation", ty: ParamType::Angle, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("orbit"), label: "Orbit", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("spread"), label: "Spread", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("phase"), label: "Phase", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("frequency"), label: "Frequency", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yaw"), label: "Yaw", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pitch"), label: "Pitch", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scale", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use the animated/current mesh as `in` and an immutable source mesh as `reference`. Spatial cells may group disconnected faces; this atom has no adjacency or watertight-fracture guarantee. All controls are instantaneous scalar shadows with no hidden clock or random source. enabled <= 0 or zero motion controls return the current record byte-exactly.",
    examples: [],
    picker: { label: "Transform Mesh Patches", category: Atom },
    summary: "Moves textured mesh patches as rigid cells using a reference triangle stream.",
    category: Geometry3D,
    role: Filter,
    aliases: ["transform mesh patches", "mesh patches", "rigid patches", "fragment transform", "surface peel"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/transform_mesh_patches_body.wgsl"),
    input_access: [Coincident, BufferGather],
}

impl Primitive for TransformMeshPatches {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let current = input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n);
        let reference = input_capacities
            .iter()
            .find(|(p, _)| *p == "reference")
            .map(|(_, n)| *n);
        match (current, reference) {
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let separation = ctx.scalar_or_param("separation", 0.15);
        let rotation = ctx.scalar_or_param("rotation", 1.0);
        let orbit = ctx.scalar_or_param("orbit", 0.0);
        let spread = ctx.scalar_or_param("spread", 0.0);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let cell_size = ctx.scalar_or_param("cell_size", 0.15);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(reference) = ctx.inputs.array("reference") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32)
            .min((reference.size / vertex_size) as u32)
            .min((dst.size / vertex_size) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = TransformMeshPatchesUniforms {
            separation,
            rotation,
            orbit,
            spread,
            phase,
            frequency,
            yaw,
            pitch,
            cell_size,
            scale,
            source_offset_x,
            source_offset_y,
            source_offset_z,
            enabled,
            dispatch_count: count,
            _pad0: 0,
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
                    buffer: reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.transform_mesh_patches",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::EffectNode;

    #[test]
    fn photoscan_modifier_patch_ports_access_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::freeze::classify::InputAccess;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let prim = TransformMeshPatches::new();
        let mesh = ArrayType::of_known::<MeshVertex>();
        for name in ["in", "reference"] {
            let port = TransformMeshPatches::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Array(mesh));
            assert!(port.required);
        }
        for name in [
            "separation",
            "rotation",
            "orbit",
            "spread",
            "phase",
            "frequency",
            "yaw",
            "pitch",
            "cell_size",
            "scale",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
            "enabled",
        ] {
            let port = TransformMeshPatches::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert_eq!(
            TransformMeshPatches::INPUT_ACCESS,
            &[InputAccess::Coincident, InputAccess::BufferGather]
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "out",
                &ParamValues::default(),
                &[("in", 12), ("reference", 12)]
            ),
            Some(12)
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "out",
                &ParamValues::default(),
                &[("in", 12), ("reference", 11)]
            ),
            None
        );
    }

    #[test]
    fn photoscan_modifier_patch_registers() {
        let prim = TransformMeshPatches::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.transform_mesh_patches");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{
        generate_fused, FusionRegion, InputSource, RegionNode, ENTRY,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    fn vertex(position: [f32; 3], normal: [f32; 3], tangent: [f32; 4]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 29.0,
            normal,
            _pad1: -13.0,
            uv: [0.25, 0.75],
            _pad2: [5.0, -9.0],
            tangent,
        }
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn length(v: [f32; 3]) -> f32 {
        dot(v, v).sqrt()
    }
    fn unit(v: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
        let l = length(v);
        if l > 1e-8 {
            [v[0] / l, v[1] / l, v[2] / l]
        } else {
            fallback
        }
    }
    fn rotate_basis(v: [f32; 3], yaw: f32, pitch: f32) -> [f32; 3] {
        let (sp, cp) = pitch.sin_cos();
        let rx = [v[0], cp * v[1] - sp * v[2], sp * v[1] + cp * v[2]];
        let (sy, cy) = yaw.sin_cos();
        [cy * rx[0] + sy * rx[2], rx[1], -sy * rx[0] + cy * rx[2]]
    }
    fn rotate(v: [f32; 3], axis: [f32; 3], angle: f32) -> [f32; 3] {
        let (s, c) = angle.sin_cos();
        let cr = cross(axis, v);
        [
            v[0] * c + cr[0] * s + axis[0] * dot(axis, v) * (1.0 - c),
            v[1] * c + cr[1] * s + axis[1] * dot(axis, v) * (1.0 - c),
            v[2] * c + cr[2] * s + axis[2] * dot(axis, v) * (1.0 - c),
        ]
    }
    fn fallback_axis(axis: [f32; 3]) -> [f32; 3] {
        let basis = if axis[0].abs() > 0.9 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        unit(cross(axis, basis), [0.0, 0.0, 1.0])
    }

    fn cpu_patch(
        v: &MeshVertex,
        reference: &[MeshVertex],
        idx: usize,
        u: TransformMeshPatchesUniforms,
    ) -> MeshVertex {
        if u.enabled <= 0.0
            || (u.separation == 0.0 && u.rotation == 0.0 && u.orbit == 0.0 && u.spread == 0.0)
            || idx / 3 * 3 + 2 >= reference.len()
        {
            return *v;
        }
        let base = idx / 3 * 3;
        let off = [u.source_offset_x, u.source_offset_y, u.source_offset_z];
        let safe_scale = u.scale.abs().max(1e-6);
        let centroid = [
            (reference[base].position[0]
                + reference[base + 1].position[0]
                + reference[base + 2].position[0])
                / 3.0
                + off[0],
            (reference[base].position[1]
                + reference[base + 1].position[1]
                + reference[base + 2].position[1])
                / 3.0
                + off[1],
            (reference[base].position[2]
                + reference[base + 1].position[2]
                + reference[base + 2].position[2])
                / 3.0
                + off[2],
        ];
        let cell = u.cell_size.abs().max(1e-6);
        let cell_norm = [
            (centroid[0] / safe_scale / cell + 0.5).floor() * cell,
            (centroid[1] / safe_scale / cell + 0.5).floor() * cell,
            (centroid[2] / safe_scale / cell + 0.5).floor() * cell,
        ];
        let center = [
            cell_norm[0] * safe_scale,
            cell_norm[1] * safe_scale,
            cell_norm[2] * safe_scale,
        ];
        let axis = unit(
            rotate_basis([0.0, 1.0, 0.0], u.yaw, u.pitch),
            [0.0, 1.0, 0.0],
        );
        let radial = unit(center, [1.0, 0.0, 0.0]);
        let local_axis = unit(cross(radial, axis), fallback_axis(axis));
        let mask = 0.5
            + 0.5 * (std::f32::consts::TAU * (dot(cell_norm, axis) * u.frequency - u.phase)).sin();
        let w = u.enabled * mask;
        let local = [
            v.position[0] + off[0] - center[0],
            v.position[1] + off[1] - center[1],
            v.position[2] + off[2] - center[2],
        ];
        let local_response = {
            let r = rotate(local, local_axis, u.rotation * w);
            [r[0] + center[0], r[1] + center[1], r[2] + center[2]]
        };
        let rotated = rotate(local_response, axis, u.orbit * w);
        let translated = [
            rotated[0] + safe_scale * w * (u.separation * radial[0] + u.spread * axis[0]),
            rotated[1] + safe_scale * w * (u.separation * radial[1] + u.spread * axis[1]),
            rotated[2] + safe_scale * w * (u.separation * radial[2] + u.spread * axis[2]),
        ];
        let mut out = *v;
        out.position = [
            translated[0] - off[0],
            translated[1] - off[1],
            translated[2] - off[2],
        ];
        out.normal = rotate(
            rotate(v.normal, local_axis, u.rotation * w),
            axis,
            u.orbit * w,
        );
        let tangent = rotate(
            rotate(
                [v.tangent[0], v.tangent[1], v.tangent[2]],
                local_axis,
                u.rotation * w,
            ),
            axis,
            u.orbit * w,
        );
        out.tangent = [tangent[0], tangent[1], tangent[2], v.tangent[3]];
        out
    }

    fn dispatch(
        wgsl: &str,
        current: &[MeshVertex],
        reference: &[MeshVertex],
        u: TransformMeshPatchesUniforms,
        label: &str,
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(current) as u64);
        let rbuf = device.create_buffer_shared(std::mem::size_of_val(reference) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(current));
            rbuf.write(0, bytemuck::cast_slice(reference));
        }
        let dst = device.create_buffer_shared(std::mem::size_of_val(current) as u64);
        let mut enc = device.create_encoder(label);
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &rbuf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &dst,
                    offset: 0,
                },
            ],
            [(current.len() as u32).div_ceil(256), 1, 1],
            label,
        );
        enc.commit_and_wait_completed();
        let ptr = dst.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, current.len()) }.to_vec()
    }

    fn standalone_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<TransformMeshPatches>()
            .expect("patch standalone codegen")
    }

    #[test]
    fn photoscan_modifier_patch_cpu_oracle_rigid_reference_and_degenerate() {
        let current = vec![
            vertex([0.4, 0.1, 0.2], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, -1.0]),
            vertex([1.1, 0.1, 0.2], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, -1.0]),
            vertex([0.4, 0.8, 0.2], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, -1.0]),
        ];
        let reference = vec![
            vertex([0.03, 0.02, 0.0], [0.0, 0.0, 1.0], [0.0; 4]),
            vertex([0.06, 0.02, 0.0], [0.0, 0.0, 1.0], [0.0; 4]),
            vertex([0.03, 0.06, 0.0], [0.0, 0.0, 1.0], [0.0; 4]),
        ];
        let u = TransformMeshPatchesUniforms {
            separation: 0.22,
            rotation: 0.41,
            orbit: 0.18,
            spread: 0.07,
            phase: 0.13,
            frequency: 0.8,
            yaw: -0.2,
            pitch: 0.15,
            cell_size: 0.2,
            scale: -0.7,
            source_offset_x: 0.1,
            source_offset_y: -0.05,
            source_offset_z: 0.02,
            enabled: 1.0,
            dispatch_count: 3,
            _pad0: 0,
        };
        let got = dispatch(
            &standalone_wgsl(),
            &current,
            &reference,
            u,
            "photoscan-patch-oracle",
        );
        let expected: Vec<_> = current
            .iter()
            .enumerate()
            .map(|(i, v)| cpu_patch(v, &reference, i, u))
            .collect();
        for i in 0..3 {
            for a in 0..3 {
                assert!(
                    (got[i].position[a] - expected[i].position[a]).abs() < 3e-5,
                    "position {i}:{a}"
                );
            }
            for a in 0..3 {
                assert!(
                    (got[i].normal[a] - expected[i].normal[a]).abs() < 3e-5,
                    "normal {i}:{a}"
                );
            }
            assert_eq!(got[i].uv, current[i].uv);
            assert_eq!(got[i].tangent[3], current[i].tangent[3]);
        }
        let edge = |a: usize, b: usize, verts: &[MeshVertex]| {
            length([
                verts[a].position[0] - verts[b].position[0],
                verts[a].position[1] - verts[b].position[1],
                verts[a].position[2] - verts[b].position[2],
            ])
        };
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            assert!(
                (edge(a, b, &got) - edge(a, b, &current)).abs() < 4e-5,
                "rigid edge {a}-{b}"
            );
        }

        let mut shifted_ref = reference.clone();
        for v in &mut shifted_ref {
            v.position[0] += 1.0;
        }
        let shifted = dispatch(
            &standalone_wgsl(),
            &current,
            &shifted_ref,
            u,
            "photoscan-patch-reference",
        );
        assert!(
            got.iter()
                .zip(&shifted)
                .any(|(a, b)| a.position != b.position),
            "reference centroid must affect patch response"
        );

        let degenerate = vec![reference[0]; 3];
        let deg_out = dispatch(
            &standalone_wgsl(),
            &current,
            &degenerate,
            u,
            "photoscan-patch-degenerate",
        );
        assert!(
            deg_out.iter().all(|v| v
                .position
                .iter()
                .chain(v.normal.iter())
                .all(|x| x.is_finite())),
            "degenerate cells must remain finite"
        );
    }

    #[test]
    fn photoscan_modifier_patch_disabled_identity_and_phasewrap() {
        let src = vec![vertex([0.4, 0.1, 0.2], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]); 3];
        let reference = src.clone();
        let u = TransformMeshPatchesUniforms {
            separation: 0.2,
            rotation: 0.3,
            orbit: 0.1,
            spread: 0.1,
            phase: 0.0,
            frequency: 0.7,
            yaw: 0.0,
            pitch: 0.0,
            cell_size: 0.2,
            scale: 1.0,
            source_offset_x: 0.0,
            source_offset_y: 0.0,
            source_offset_z: 0.0,
            enabled: 0.0,
            dispatch_count: 3,
            _pad0: 0,
        };
        let wgsl = standalone_wgsl();
        let identity = dispatch(&wgsl, &src, &reference, u, "photoscan-patch-identity");
        for i in 0..3 {
            assert_eq!(
                bytemuck::bytes_of(&identity[i]),
                bytemuck::bytes_of(&src[i])
            );
        }
        let mut phase = u;
        phase.enabled = 1.0;
        phase.phase = 0.0;
        let a = dispatch(&wgsl, &src, &reference, phase, "photoscan-patch-phase0");
        phase.phase = 1.0;
        let b = dispatch(&wgsl, &src, &reference, phase, "photoscan-patch-phase1");
        for i in 0..3 {
            for axis in 0..3 {
                assert!((a[i].position[axis] - b[i].position[axis]).abs() < 3e-5);
            }
        }
    }

    #[test]
    fn photoscan_modifier_patch_buffer_gather_is_explicit_fusion_boundary() {
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: TransformMeshPatches::WGSL_BODY.unwrap(),
                params: TransformMeshPatches::PARAMS,
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: vec![InputAccess::Coincident, InputAccess::BufferGather],
                node_inputs: TransformMeshPatches::INPUTS,
                node_outputs: TransformMeshPatches::OUTPUTS,
                node_includes: TransformMeshPatches::WGSL_INCLUDES,
                derived_uniforms: TransformMeshPatches::DERIVED_UNIFORMS,
                type_id: TransformMeshPatches::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba32float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 2,
            outputs: vec![(id(0), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
        };
        assert!(
            generate_fused(&region).is_err(),
            "BufferGather reference lookup must remain an explicit fusion boundary"
        );
    }
}
