//! `node.platonic_solid_mesh` — CPU-authored closed triangle meshes for the
//! five Platonic solids, uploaded through a compact inline source payload.
//!
//! This is an IO bridge: the reusable topology and flat normals live in
//! `generators::platonic_geometry`; this node only selects the cached source,
//! applies the port-shadowed radius on GPU, and writes the fixed-capacity
//! `Array<MeshVertex>` output.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use crate::generators::mesh_common::{MeshVertex, PLATONIC_SHAPES};
use crate::generators::platonic_geometry::{platonic_mesh_upload_bytes, PLATONIC_MESH_CAPACITY};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: PlatonicMesh,
    type_id: "node.platonic_solid_mesh",
    purpose: "Emit a closed, outward-wound flat-normal triangle mesh for one of the five Platonic solids as Array(MeshVertex). Shape selects the reusable Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron topology; radius scales its circumradius-one source. Pair with node.render_3d_mesh or any mesh transform chain.",
    inputs: {
        shape: ScalarF32 optional,
        radius: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("shape"),
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: PLATONIC_SHAPES,
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is fixed at PLATONIC_MESH_CAPACITY (108 triangle-list vertices, the dodecahedron's 36 triangles). Smaller shapes are zero-padded with degenerate triangles. The CPU source is circumradius one and carries one flat outward normal per triangle; radius is applied during the upload dispatch. Shape and radius are port-shadowed, so scalar wires can animate them without changing the mesh contract.",
    examples: [],
    picker: { label: "Platonic Solid Mesh", category: Atom },
    summary: "Builds a reusable closed triangle mesh for any of the five Platonic solids.",
    category: Geometry3D,
    role: Source,
    aliases: ["platonic mesh", "platonic solid mesh", "solid mesh", "polyhedron"],
    boundary_reason: IoBridge,
    extra_fields: {
        upload_pipeline: Option<GpuComputePipeline> = None,
    },
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadUniforms {
    count: u32,
    capacity: u32,
    radius: f32,
    _pad: u32,
}

impl Primitive for PlatonicMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "vertices").then_some(PLATONIC_MESH_CAPACITY as u32)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let shape = crate::node_graph::primitives::polytope_vertices::read_shape(ctx);
        let radius = ctx.scalar_or_param("radius", 1.0);
        let Some(dst) = ctx.outputs.array("vertices") else {
            log::warn!("node.platonic_solid_mesh: no GpuBuffer bound to output port `vertices`");
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (dst.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.upload_pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/platonic_mesh_upload.wgsl"),
                "cs_main",
                "node.platonic_solid_mesh",
            )
        });
        let uniforms = UploadUniforms {
            count: platonic_mesh_upload_count(shape),
            capacity,
            radius,
            _pad: 0,
        };
        let source = platonic_mesh_upload_bytes(shape);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                // The compact 108 × 32 byte source is copied by Metal at
                // encode time, so later frames cannot overwrite in-flight
                // GPU work through a mapped destination buffer.
                GpuBinding::Bytes {
                    binding: 1,
                    data: source,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.platonic_solid_mesh",
        );
    }
}

fn platonic_mesh_upload_count(shape: u32) -> u32 {
    match shape {
        0 => 12,
        1 => 36,
        2 => 24,
        3 => 60,
        _ => PLATONIC_MESH_CAPACITY as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::EffectNode;

    #[test]
    fn declares_shape_radius_and_mesh_output() {
        let mesh = ArrayType::of_known::<MeshVertex>();
        assert_eq!(PlatonicMesh::TYPE_ID, "node.platonic_solid_mesh");
        assert_eq!(PlatonicMesh::INPUTS.len(), 2);
        assert_eq!(PlatonicMesh::INPUTS[0].name, "shape");
        assert_eq!(PlatonicMesh::INPUTS[1].name, "radius");
        assert!(PlatonicMesh::INPUTS
            .iter()
            .all(|port| { !port.required && port.ty == PortType::Scalar(ScalarType::F32) }));
        assert_eq!(PlatonicMesh::OUTPUTS.len(), 1);
        assert_eq!(PlatonicMesh::OUTPUTS[0].name, "vertices");
        assert_eq!(PlatonicMesh::OUTPUTS[0].ty, PortType::Array(mesh));
        assert_eq!(
            PlatonicMesh::OUTPUTS[0].kind,
            crate::node_graph::ports::PortKind::Output
        );
    }

    #[test]
    fn shape_and_radius_params_match_public_contract() {
        assert_eq!(PlatonicMesh::PARAMS.len(), 2);
        assert_eq!(PlatonicMesh::PARAMS[0].ty, ParamType::Enum);
        assert_eq!(PlatonicMesh::PARAMS[0].enum_values, PLATONIC_SHAPES);
        assert_eq!(PlatonicMesh::PARAMS[1].ty, ParamType::Float);
        assert_eq!(PlatonicMesh::PARAMS[1].default, ParamValue::Float(1.0));
    }

    #[test]
    fn output_capacity_is_fixed_to_dodecahedron_triangle_list() {
        let primitive = PlatonicMesh::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&primitive, "vertices", &params, &[]),
            Some(PLATONIC_MESH_CAPACITY as u32)
        );
        assert_eq!(
            Primitive::array_output_capacity(&primitive, "other", &params, &[]),
            None
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let node: &dyn EffectNode = &PlatonicMesh::new();
        assert_eq!(node.type_id().as_str(), "node.platonic_solid_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::generators::platonic_geometry::platonic_mesh;

    #[test]
    fn upload_matches_cpu_mesh_and_zero_padding() {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/platonic_mesh_upload.wgsl"),
            "cs_main",
            "node.platonic_solid_mesh.gpu-proof",
        );
        for shape in 0..5u32 {
            let out = device.create_buffer_shared(
                (PLATONIC_MESH_CAPACITY * std::mem::size_of::<MeshVertex>()) as u64,
            );
            let uniforms = UploadUniforms {
                count: platonic_mesh_upload_count(shape),
                capacity: PLATONIC_MESH_CAPACITY as u32,
                radius: 1.75,
                _pad: 0,
            };
            let mut encoder = device.create_encoder("platonic-mesh-gpu-proof");
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Bytes {
                        binding: 1,
                        data: platonic_mesh_upload_bytes(shape),
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &out,
                        offset: 0,
                    },
                ],
                [PLATONIC_MESH_CAPACITY.div_ceil(64) as u32, 1, 1],
                "node.platonic_solid_mesh.gpu-proof",
            );
            encoder.commit_and_wait_completed();
            let ptr = out.mapped_ptr().expect("shared output buffer");
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    ptr as *const u8,
                    PLATONIC_MESH_CAPACITY * std::mem::size_of::<MeshVertex>(),
                )
            };
            let actual = bytemuck::cast_slice::<u8, MeshVertex>(bytes);
            let cpu = platonic_mesh(shape);
            for (index, (got, expected)) in actual.iter().zip(cpu.iter()).enumerate() {
                for component in 0..3 {
                    assert!(
                        (got.position[component] - expected.position[component] * 1.75).abs()
                            < 2.0e-5,
                        "shape={shape} vertex={index} position={component} got={} expected={}",
                        got.position[component],
                        expected.position[component] * 1.75
                    );
                    assert!(
                        (got.normal[component] - expected.normal[component]).abs() < 2.0e-5,
                        "shape={shape} vertex={index} normal={component}"
                    );
                }
                assert_eq!(got.uv, [0.0, 0.0]);
                assert_eq!(got.tangent, [0.0; 4]);
            }
            for (index, got) in actual.iter().enumerate().skip(cpu.len()) {
                assert_eq!(got.position, [0.0; 3], "shape={shape} padding={index}");
                assert_eq!(got.normal, [0.0; 3], "shape={shape} padding normal={index}");
                assert_eq!(got.uv, [0.0, 0.0]);
                assert_eq!(got.tangent, [0.0; 4]);
            }
        }
    }
}
