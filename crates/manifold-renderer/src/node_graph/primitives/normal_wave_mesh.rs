//! `node.normal_wave_mesh` — coherent normal displacement of a current
//! triangle stream with triangle-aware smooth frame transport.
//!
//! The wave is evaluated from each current vertex's scene-relative position
//! and input normal.  The three displaced corners of the current triangle are
//! then used to derive a local planar deformation map.  Input normals use the
//! inverse-transpose map and input tangents use the forward map followed by
//! re-orthogonalisation, so this keeps smooth scan shading without a hidden
//! facet-normal pass.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NormalWaveUniforms {
    amplitude: f32,
    frequency: f32,
    phase: f32,
    yaw: f32,
    pitch: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    enabled: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: NormalWaveMesh,
    type_id: "node.normal_wave_mesh",
    purpose: "Displace the current Array<MeshVertex> along its smooth input normals by a coherent directional sine wave. The phase uses scene-relative current positions and an explicit Phase; amplitude is relative to the wired scene radius. The current triangle's three displaced corners transport smooth normals with an inverse-transpose frame map and tangents with a forward map, preserving UVs and tangent handedness. Disabled or zero amplitude returns the current record exactly; degenerate triangles preserve their input frame.",
    inputs: {
        in: Array(MeshVertex) required,
        amplitude: ScalarF32 optional,
        frequency: ScalarF32 optional,
        phase: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
        enabled: ScalarF32 optional,
    },
    outputs: { out: Array(MeshVertex), },
    params: [
        ParamDef { name: Cow::Borrowed("amplitude"), label: "Amplitude", ty: ParamType::Float, default: ParamValue::Float(0.12), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("frequency"), label: "Frequency", ty: ParamType::Float, default: ParamValue::Float(1.5), range: Some((0.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("phase"), label: "Phase", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yaw"), label: "Yaw", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pitch"), label: "Pitch", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use this for a continuous textured surface wave. It reads only the current mesh, so it composes after preceding mesh modifiers. `scale` should be the scene radius and source offsets should be the attachment's world-to-local correction. Phase is a direct control with no internal clock. The triangle gather is required to transport the smooth input frame from the actual displaced corner basis; it is a standalone boundary like node.transform_mesh_patches and node.facet_normals. It deliberately does not call node.facet_normals, which would replace smooth scan normals with flat ones.",
    examples: ["SurfaceWaves"],
    picker: { label: "Normal Wave Mesh", category: Atom },
    summary: "Travels a smooth directional wave across the current textured mesh while carrying its lighting frame.",
    category: Geometry3D,
    role: Filter,
    aliases: ["normal wave", "surface wave", "surface waves", "wave mesh", "smooth mesh wave"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/normal_wave_mesh_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for NormalWaveMesh {
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
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amplitude = ctx.scalar_or_param("amplitude", 0.12);
        let frequency = ctx.scalar_or_param("frequency", 1.5);
        let phase = ctx.scalar_or_param("phase", 0.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);
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
        let uniforms = NormalWaveUniforms {
            amplitude,
            frequency,
            phase,
            yaw,
            pitch,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.normal_wave_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn normal_wave_declares_current_only_mesh_and_scalar_shadows() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh = ArrayType::of_known::<MeshVertex>();
        let prim = NormalWaveMesh::new();
        assert_eq!(NormalWaveMesh::TYPE_ID, "node.normal_wave_mesh");
        let input = NormalWaveMesh::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .unwrap();
        assert!(input.required);
        assert_eq!(input.ty, PortType::Array(mesh));
        for name in [
            "amplitude",
            "frequency",
            "phase",
            "yaw",
            "pitch",
            "scale",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
            "enabled",
        ] {
            let port = NormalWaveMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(NormalWaveMesh::OUTPUTS[0].ty, PortType::Array(mesh));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &ParamValues::default(), &[("in", 19)]),
            Some(19)
        );
    }

    #[test]
    fn normal_wave_registers_as_palette_atom() {
        let prim = NormalWaveMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.normal_wave_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    fn vertex(position: [f32; 3], normal: [f32; 3], tangent: [f32; 4]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 13.0,
            normal,
            _pad1: -11.0,
            uv: [0.25, 0.75],
            _pad2: [5.0, -3.0],
            tangent,
        }
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn dispatch(
        wgsl: &str,
        src_vertices: &[MeshVertex],
        u: NormalWaveUniforms,
        label: &str,
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(src_vertices) as u64);
        let dst = device.create_buffer_shared(std::mem::size_of_val(src_vertices) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(src_vertices));
        }
        let mut encoder = device.create_encoder(label);
        encoder.dispatch_compute(
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
                    buffer: &dst,
                    offset: 0,
                },
            ],
            [(src_vertices.len() as u32).div_ceil(256), 1, 1],
            label,
        );
        encoder.commit_and_wait_completed();
        let ptr = dst.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src_vertices.len()) }.to_vec()
    }

    fn uniforms(amplitude: f32, enabled: f32) -> NormalWaveUniforms {
        NormalWaveUniforms {
            amplitude,
            frequency: 1.4,
            phase: 0.17,
            yaw: 0.2,
            pitch: -0.1,
            scale: 1.0,
            source_offset_x: 0.1,
            source_offset_y: -0.05,
            source_offset_z: 0.02,
            enabled,
            dispatch_count: 3,
            _pad0: 0,
        }
    }

    #[test]
    fn structured_modifier_normal_wave_standalone_proves_identity_and_smooth_frame() {
        let src = vec![
            vertex([-0.4, -0.2, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, -1.0]),
            vertex([0.5, -0.1, 0.0], [0.0, 0.2, 0.98], [1.0, 0.0, 0.0, -1.0]),
            vertex([-0.1, 0.6, 0.0], [0.15, 0.0, 0.99], [1.0, 0.0, 0.0, -1.0]),
        ];
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<NormalWaveMesh>()
            .expect("normal wave standalone codegen");
        let identity = dispatch(&wgsl, &src, uniforms(0.0, 1.0), "normal-wave-zero");
        for (got, expected) in identity.iter().zip(&src) {
            assert_eq!(
                (got.position, got.normal, got.uv, got.tangent),
                (
                    expected.position,
                    expected.normal,
                    expected.uv,
                    expected.tangent
                ),
                "amplitude zero is exact"
            );
        }
        let disabled = dispatch(&wgsl, &src, uniforms(0.4, 0.0), "normal-wave-disabled");
        for (got, expected) in disabled.iter().zip(&src) {
            assert_eq!(
                (got.position, got.normal, got.uv, got.tangent),
                (
                    expected.position,
                    expected.normal,
                    expected.uv,
                    expected.tangent
                ),
                "disabled is exact"
            );
        }
        let displaced = dispatch(&wgsl, &src, uniforms(0.24, 1.0), "normal-wave-displaced");
        assert!(
            displaced
                .iter()
                .zip(&src)
                .any(|(a, b)| a.position != b.position),
            "nonzero amplitude changes the current surface"
        );
        assert!(
            displaced.iter().all(|v| v
                .position
                .iter()
                .chain(v.normal.iter())
                .all(|x| x.is_finite())),
            "normal wave frame transport remains finite"
        );
        for (got, expected) in displaced.iter().zip(&src) {
            assert_eq!(got.uv, expected.uv);
            assert_eq!(got.tangent[3], expected.tangent[3]);
        }

        // Independent oblique-triangle oracle: with the source normal equal
        // to the source face normal, the transported normal must remain
        // orthogonal to both deformed edges. This catches an inverse versus
        // inverse-transpose mistake in the frame map.
        let oblique = vec![
            vertex([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]),
            vertex([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]),
            vertex([0.2, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]),
        ];
        let mut oblique_u = uniforms(0.22, 1.0);
        oblique_u.frequency = 1.1;
        oblique_u.yaw = std::f32::consts::FRAC_PI_2;
        let oblique_out = dispatch(&wgsl, &oblique, oblique_u, "normal-wave-oblique");
        let edge_a = [
            oblique_out[1].position[0] - oblique_out[0].position[0],
            oblique_out[1].position[1] - oblique_out[0].position[1],
            oblique_out[1].position[2] - oblique_out[0].position[2],
        ];
        let edge_b = [
            oblique_out[2].position[0] - oblique_out[0].position[0],
            oblique_out[2].position[1] - oblique_out[0].position[1],
            oblique_out[2].position[2] - oblique_out[0].position[2],
        ];
        assert!(dot(oblique_out[0].normal, edge_a).abs() < 2e-4);
        assert!(dot(oblique_out[0].normal, edge_b).abs() < 2e-4);
    }

    #[test]
    fn structured_modifier_normal_wave_triangle_gather_is_fusion_boundary() {
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: NormalWaveMesh::WGSL_BODY.unwrap(),
                params: NormalWaveMesh::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![InputAccess::BufferGather],
                node_inputs: NormalWaveMesh::INPUTS,
                node_outputs: NormalWaveMesh::OUTPUTS,
                node_includes: NormalWaveMesh::WGSL_INCLUDES,
                derived_uniforms: NormalWaveMesh::DERIVED_UNIFORMS,
                type_id: NormalWaveMesh::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba32float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 1,
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
            "triangle gather must remain a standalone boundary"
        );
    }
}
