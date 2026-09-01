//! `node.plane_mesh` — emit a flat, camera-facing quad as 6
//! triangle-list `MeshVertex` entries (2 triangles) in the XY plane,
//! facing +Z, normal (0, 0, 1), spanning
//! `[-width/2, width/2] × [-height/2, height/2]`.
//!
//! The plane is the sheet a live layer composite gets skinned onto:
//! wire a `node.layer_source` into the consuming `node.scene_object`'s
//! `base_color_map` and the object wears whatever that layer is playing.
//! UVs use the cube +Z face's convention — `uv = vec2(n.x, 1.0 - n.y)`
//! with `n` the 0..1-normalized position — so the composite reads
//! upright when the plane is viewed from +Z.
//!
//! Single-sided: backface-culled from behind. Flip via a transform
//! rotation (a 180° Y-rotation presents the mirrored back) if the far
//! side is needed.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Number of triangle vertices in the flat plane (2 triangles × 3 vertices).
pub const PLANE_VERTEX_COUNT: u32 = 6;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`max_capacity` Int → i32 [allocation-only, the shader ignores it but it
/// occupies a uniform word], `width` f32, `height` f32) then the
/// codegen-injected `dispatch_count` (= output capacity, the guard) — 4
/// words, exactly one 16-byte uniform, no pad.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlaneUniforms {
    max_capacity: i32,
    width: f32,
    height: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: GeneratePlaneMesh,
    type_id: "node.plane_mesh",
    purpose: "Emit a flat camera-facing quad as 6 triangle-list MeshVertex entries in the XY plane, facing +Z. The sheet that displays a live layer composite in 3D: pair node.layer_source (wired into the consuming node.scene_object's base_color_map) + node.unlit_material to skin the plane with whatever another layer is playing. width/height are port-shadowed so the plane aspect can be driven from the canvas aspect or an LFO.",
    inputs: {
        width: ScalarF32 optional,
        height: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(6.0),
            range: Some((6.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The plane's UVs follow the cube +Z face convention — uv = vec2(n.x, 1.0 - n.y), n the 0..1-normalized position — so a layer composite skinned onto it reads upright when viewed from +Z. Wire node.layer_source's `out` into the consuming node.scene_object's `base_color_map` (that is the layer-skin pairing; an empty `layer` param renders transparent black until a source is picked). Single-sided: backface-culled from behind — flip via a transform rotation if you need the back. width/height are port-shadowed scalar inputs; wire the canvas aspect to width (aspect × height) to keep the skin undistorted. max_capacity is the chain-build pre-allocation ceiling — defaults to 6 (exactly one plane); larger values pad the buffer with degenerate zero-vertex entries.",
    examples: [],
    picker: { label: "Plane Mesh", category: Atom },
    summary: "Builds a flat rectangular sheet of mesh ready to skin with another layer's output. The surface for placing live video in a 3D scene.",
    category: Geometry3D,
    role: Source,
    aliases: ["plane", "quad", "sheet", "flat mesh", "Plane SOP"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/plane_mesh_body.wgsl"),
}

impl Primitive for GeneratePlaneMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_capacity = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => PLANE_VERTEX_COUNT as i32,
        };
        // Port-shadows-param (section 6.2 authoring rule): the wire wins when
        // present, the param is the fallback.
        let width = ctx.scalar_or_param("width", 1.0);
        let height = ctx.scalar_or_param("height", 1.0);

        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (dst.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // source path).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.plane_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.plane_mesh",
            )
        });

        let uniforms = PlaneUniforms {
            max_capacity,
            width,
            height,
            dispatch_count: capacity,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.plane_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_plane_mesh_declares_width_height_inputs_and_mesh_array_output() {
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(GeneratePlaneMesh::TYPE_ID, "node.plane_mesh");
        assert_eq!(GeneratePlaneMesh::INPUTS.len(), 2);
        assert_eq!(GeneratePlaneMesh::INPUTS[0].name, "width");
        assert!(!GeneratePlaneMesh::INPUTS[0].required);
        assert_eq!(GeneratePlaneMesh::INPUTS[0].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GeneratePlaneMesh::INPUTS[1].name, "height");
        assert!(!GeneratePlaneMesh::INPUTS[1].required);
        assert_eq!(GeneratePlaneMesh::INPUTS[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(GeneratePlaneMesh::OUTPUTS.len(), 1);
        assert_eq!(GeneratePlaneMesh::OUTPUTS[0].name, "vertices");
        assert_eq!(
            GeneratePlaneMesh::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn generate_plane_mesh_default_capacity_is_6() {
        let names: Vec<&str> = GeneratePlaneMesh::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["max_capacity", "width", "height"]);
        let cap = GeneratePlaneMesh::PARAMS
            .iter()
            .find(|p| p.name == "max_capacity")
            .unwrap();
        match cap.default {
            ParamValue::Float(n) => assert_eq!(n as u32, PLANE_VERTEX_COUNT),
            _ => panic!("expected Float (Int presentation hint)"),
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GeneratePlaneMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.plane_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level proof for `node.plane_mesh`. Source atoms are
    //! standalone single-source only (the freeze region-grower never folds a
    //! Source into a multi-node fused region — same contract the cube
    //! documents), so there is no fused-vs-unfused neighbor fence to draw —
    //! the proof is the generated standalone kernel's value output against
    //! CPU-computed expected vertices. All six positions, all six normals,
    //! and the four corner UVs are asserted element-by-element: the UV
    //! orientation (`uv = (n.x, 1 - n.y)`, upright from +Z) is the contract.
    use super::*;

    fn dispatch_plane(wgsl: &str, capacity: u32, uniform: &[u8]) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "plane-mesh-oracle");
        let out_buf = device.create_buffer_shared(capacity as u64 * 64);
        let mut enc = device.create_encoder("plane-mesh-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &out_buf, offset: 0 },
            ],
            [capacity.div_ceil(256), 1, 1],
            "plane-mesh-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice =
            unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, capacity as usize) };
        slice.to_vec()
    }

    /// CPU-side mirror of the body's tables: corner positions in ±1 unit
    /// span, corner UVs per the cube +Z convention `uv = (n.x, 1 - n.y)`
    /// (n = the 0..1-normalized position).
    fn expected_vertex(idx: u32, width: f32, height: f32) -> MeshVertex {
        const CORNER_POS: [[f32; 3]; 6] = [
            [-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0],
            [-1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0],
        ];
        const CORNER_UV: [[f32; 2]; 6] = [
            [0.0, 1.0], [1.0, 1.0], [1.0, 0.0],
            [0.0, 1.0], [1.0, 0.0], [0.0, 0.0],
        ];
        let c = CORNER_POS[idx as usize];
        let uv = CORNER_UV[idx as usize];
        MeshVertex {
            position: [c[0] * width * 0.5, c[1] * height * 0.5, 0.0],
            _pad0: 0.0,
            normal: [0.0, 0.0, 1.0],
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
        }
    }

    #[test]
    fn generated_plane_mesh_matches_cpu_expected_positions_normals_and_uvs() {
        const CAPACITY: u32 = 8; // 6 live + 2 padding slots
        let width = 2.0f32;
        let height = 3.0f32;

        // Generated uniform layout: max_capacity(i32), width(f32), height(f32),
        // dispatch_count(u32) — the same byte order `run()` packs.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CAPACITY as i32).to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&CAPACITY.to_le_bytes());

        let wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<GeneratePlaneMesh>()
                .expect("plane_mesh buffer codegen");

        let verts = dispatch_plane(&wgsl, CAPACITY, &bytes);
        assert_eq!(verts.len(), CAPACITY as usize);

        for (i, vert) in verts.iter().take(6).enumerate() {
            let e = expected_vertex(i as u32, width, height);
            for c in 0..3 {
                assert!(
                    (vert.position[c] - e.position[c]).abs() < 1e-6,
                    "slot {i} position[{c}]: got {} expected {}",
                    vert.position[c],
                    e.position[c]
                );
                assert!(
                    (vert.normal[c] - e.normal[c]).abs() < 1e-6,
                    "slot {i} normal[{c}]: got {} expected {}",
                    vert.normal[c],
                    e.normal[c]
                );
            }
            for c in 0..2 {
                assert!(
                    (vert.uv[c] - e.uv[c]).abs() < 1e-6,
                    "slot {i} uv[{c}]: got {} expected {}",
                    vert.uv[c],
                    e.uv[c]
                );
            }
        }

        // The two padding slots write the degenerate vertex form.
        for (i, vert) in verts.iter().enumerate().skip(6) {
            assert_eq!(vert.position, [0.0, 0.0, 0.0], "slot {i} padding position");
            assert_eq!(vert.normal, [0.0, 1.0, 0.0], "slot {i} padding normal");
            assert_eq!(vert.uv, [0.0, 0.0], "slot {i} padding uv");
        }
    }
}