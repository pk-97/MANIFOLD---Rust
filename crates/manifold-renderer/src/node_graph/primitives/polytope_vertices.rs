//! `node.platonic_solid_points` — emit the vertex set of one of the five
//! Platonic solids as `Array<MeshVertex>`. Curated-enum atom: a single
//! GPU compute dispatch with closed-form per-shape coordinates baked
//! into WGSL.
//!
//! The set is mathematically closed at five — Euclid proved in ~300 BC
//! that exactly five regular convex polyhedra exist in 3D. Compiled-in
//! variants are correct because the family can never grow. Non-Platonic
//! shape sources (loaded meshes, geodesic spheres, parametric prisms)
//! ship as sibling primitives sharing the same `Array<MeshVertex>` wire
//! type — the downstream `rotate_3d → project_3d → render_lines` chain
//! is reusable across all of them.
//!
//! Pair with [`super::polytope_edges`] for the matching wireframe
//! topology. Both atoms read the same shape selector — wire a single
//! scalar to both so the vertices and edges agree per frame.
//!
//! Positions are normalised to magnitude `POLYTOPE_DEFAULT_RADIUS` in
//! WGSL (0.25 — the legacy `PROJ_SCALE` from the original line
//! generators). The constant lives inside the primitive so downstream
//! `project_3d.proj_scale` defaults to 1.0 (user-facing "Scale = 1.0 =
//! default zoom") instead of needing a graph-side math node.
//!
//! Vertex counts: Tetra=4, Cube=8, Octa=6, Icosa=12, Dodeca=20. Slots
//! `i >= nverts` are zero-padded so downstream rotate/project never
//! reads garbage; the paired `polytope_edges` only references valid
//! indices for the active shape.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{MeshVertex, PLATONIC_MAX_VERTS, PLATONIC_SHAPES};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `shape` Enum param (u32) then the
/// codegen-injected `dispatch_count` (= vertex capacity, the guard), padded to
/// 16 bytes. `nverts` is derived in-shader from `shape` (no separate count).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PolytopeUniforms {
    shape: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Resolve the `shape` selector with port-shadows-param semantics for
/// the curated `ParamValue::Enum` storage. `scalar_or_param` only
/// matches the `Float` variant and silently falls through on `Enum`,
/// which would always emit the default-0 shape (Tetrahedron) regardless
/// of the JSON-set value. Match both storage shapes explicitly here.
pub(crate) fn read_shape(ctx: &EffectNodeContext<'_, '_>) -> u32 {
    let wired = ctx.inputs.scalar("shape").and_then(|v| match v {
        ParamValue::Float(f) => Some(f.round().max(0.0) as u32),
        ParamValue::Enum(n) => Some(n),
        _ => None,
    });
    let raw = wired.unwrap_or_else(|| match ctx.params.get("shape") {
        Some(ParamValue::Enum(n)) => *n,
        Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
        _ => 0,
    });
    raw.min(PLATONIC_SHAPES.len() as u32 - 1)
}

crate::primitive! {
    name: PolytopeVertices,
    type_id: "node.platonic_solid_points",
    purpose: "Emit the vertex set of one of the five Platonic solids (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as Array<MeshVertex>. Curated-enum atom — one GPU dispatch with closed-form per-shape coordinates baked into WGSL, normalised to magnitude 0.25 (the legacy screen-friendly default). Pair with node.platonic_solid_edges (driving both from the same shape scalar) and feed both into node.rotate_3d → node.flatten_3d → node.draw_lines for a 3D wireframe.",
    inputs: {
        // Port-shadows the `shape` enum param. Wire a scalar here to
        // drive the shape from a mux / clip_trigger_cycle / external
        // selector; leave unwired and the param's static value picks
        // the shape.
        shape: ScalarF32 optional,
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
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is fixed at PLATONIC_MAX_VERTS (20 — the dodecahedron count); slots past the active shape's vertex count are zero-padded so a downstream rotate/project chain never reads garbage. Indices written are stable per shape and match the paired `node.platonic_solid_edges` topology — wire the same `shape` scalar to both atoms so vertices and edges agree.",
    examples: [],
    picker: { label: "Platonic Solid Points", category: Atom },
    summary: "Builds the corner points of one of the five Platonic solids, from a tetrahedron to a dodecahedron. The vertex set for wireframe geometry.",
    category: Geometry3D,
    role: Source,
    aliases: ["platonic solid", "polytope vertices", "polytope", "vertices", "points"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/polytope_vertices_body.wgsl"),
}

impl Primitive for PolytopeVertices {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "vertices" {
            Some(PLATONIC_MAX_VERTS)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param: wired `shape` scalar wins over the enum
        // param. `scalar_or_param` only matches `Float` storage — the
        // `shape` param is stored as `ParamValue::Enum(n)`, so the
        // helper falls through. Read both storage variants explicitly.
        let shape = read_shape(ctx);

        let Some(vert_dst) = ctx.outputs.array("vertices") else {
            log::warn!(
                "node.platonic_solid_points: no GpuBuffer bound to output port `vertices` — \
                 the chain build did not pre-allocate the Array<MeshVertex> output.",
            );
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let vert_capacity = (vert_dst.size / vertex_size) as u32;
        if vert_capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer source
            // path; per-shape vertex tables inlined). polytope_vertices.wgsl is
            // the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.platonic_solid_points standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.platonic_solid_points",
            )
        });

        let uniforms = PolytopeUniforms {
            shape,
            dispatch_count: vert_capacity,
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
                    buffer: vert_dst,
                    offset: 0,
                },
            ],
            [vert_capacity.div_ceil(256), 1, 1],
            "node.platonic_solid_points",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_shape_input_and_mesh_vertex_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let vert_layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(PolytopeVertices::TYPE_ID, "node.platonic_solid_points");
        assert_eq!(PolytopeVertices::INPUTS.len(), 1);
        assert_eq!(PolytopeVertices::INPUTS[0].name, "shape");
        assert!(!PolytopeVertices::INPUTS[0].required);
        assert_eq!(
            PolytopeVertices::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );

        assert_eq!(PolytopeVertices::OUTPUTS.len(), 1);
        assert_eq!(PolytopeVertices::OUTPUTS[0].name, "vertices");
        assert_eq!(
            PolytopeVertices::OUTPUTS[0].ty,
            PortType::Array(vert_layout)
        );
    }

    #[test]
    fn shape_enum_lists_five_platonic_solids() {
        let shape = PolytopeVertices::PARAMS
            .iter()
            .find(|p| p.name == "shape")
            .unwrap();
        assert_eq!(shape.ty, ParamType::Enum);
        assert_eq!(shape.enum_values.len(), 5);
        assert_eq!(shape.enum_values, PLATONIC_SHAPES);
    }

    #[test]
    fn output_capacity_is_platonic_max_verts() {
        let prim = PolytopeVertices::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &params, &[]),
            Some(PLATONIC_MAX_VERTS)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = PolytopeVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.platonic_solid_points");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! GPU parity tests against the legacy `WireframeZooGenerator`'s
    //! CPU pipeline — same shape as the previous wireframe_shape
    //! gpu_tests, now scoped to vertex emission only (edges live in
    //! the paired polytope_edges atom).
    //!
    //! What we verify: the WGSL output matches the legacy
    //! `normalise_shape() * PROJ_SCALE` reference element-wise for
    //! every shape × every vertex.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::mesh_common::{MeshVertex, PLATONIC_MAX_VERTS};
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType,
    };
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };

    use super::PolytopeVertices;

    /// Test-only sink: consumes the `vertices` Array(MeshVertex) so
    /// d84ae560's planner keeps the resource alive.
    struct VertexSink {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl VertexSink {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.vertex_sink"),
                inputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                    kind: PortKind::Input,
                    required: true,
                }],
                outputs: vec![],
            }
        }
    }

    impl EffectNode for VertexSink {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
        fn is_liveness_root(&self) -> bool {
            true
        }
    }

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    const PHI: f32 = 1.618034;
    const INV_PHI: f32 = 0.618034;

    fn legacy_raw_vertices(shape: u32) -> &'static [[f32; 3]] {
        const TETRA: &[[f32; 3]] = &[
            [1.0, 1.0, 1.0],
            [1.0, -1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
        ];
        const CUBE: &[[f32; 3]] = &[
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        const OCTA: &[[f32; 3]] = &[
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        const ICOSA: &[[f32; 3]] = &[
            [-1.0, PHI, 0.0],
            [1.0, PHI, 0.0],
            [-1.0, -PHI, 0.0],
            [1.0, -PHI, 0.0],
            [0.0, -1.0, PHI],
            [0.0, 1.0, PHI],
            [0.0, -1.0, -PHI],
            [0.0, 1.0, -PHI],
            [PHI, 0.0, -1.0],
            [PHI, 0.0, 1.0],
            [-PHI, 0.0, -1.0],
            [-PHI, 0.0, 1.0],
        ];
        const DODECA: &[[f32; 3]] = &[
            [1.0, 1.0, 1.0],
            [1.0, 1.0, -1.0],
            [1.0, -1.0, 1.0],
            [1.0, -1.0, -1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, -1.0, -1.0],
            [0.0, PHI, INV_PHI],
            [0.0, PHI, -INV_PHI],
            [0.0, -PHI, INV_PHI],
            [0.0, -PHI, -INV_PHI],
            [INV_PHI, 0.0, PHI],
            [INV_PHI, 0.0, -PHI],
            [-INV_PHI, 0.0, PHI],
            [-INV_PHI, 0.0, -PHI],
            [PHI, INV_PHI, 0.0],
            [PHI, -INV_PHI, 0.0],
            [-PHI, INV_PHI, 0.0],
            [-PHI, -INV_PHI, 0.0],
        ];
        match shape {
            0 => TETRA,
            1 => CUBE,
            2 => OCTA,
            3 => ICOSA,
            _ => DODECA,
        }
    }

    fn legacy_expected_position(shape: u32, i: usize) -> [f32; 3] {
        let raw = legacy_raw_vertices(shape)[i];
        let max_dist = legacy_raw_vertices(shape)
            .iter()
            .map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt())
            .fold(0.0_f32, f32::max);
        let k = 0.25 / max_dist;
        [raw[0] * k, raw[1] * k, raw[2] * k]
    }

    fn run_polytope_vertices(shape: u32) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let pv = g.add_node(Box::new(PolytopeVertices::new()));
        g.set_param(pv, "shape", ParamValue::Enum(shape)).unwrap();
        let sink = g.add_node(Box::new(VertexSink::new()));
        g.connect((pv, "vertices"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_verts = output_resource(&plan, pv, "vertices");

        let vert_bytes = (PLATONIC_MAX_VERTS as u64) * std::mem::size_of::<MeshVertex>() as u64;
        let vert_buf = device.create_buffer_shared(vert_bytes);

        let mut backend = MetalBackend::new(device.arc(), 1, 1, format);
        let vert_slot = backend.pre_bind_array(r_verts, vert_buf);

        let mut native_enc = device.create_encoder("polytope-vertices-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let verts_buf = exec
            .backend()
            .array_buffer(vert_slot)
            .expect("vertices buffer retained");
        let v_ptr = verts_buf.mapped_ptr().expect("shared vertices buffer");
        let v_bytes_slice =
            unsafe { std::slice::from_raw_parts(v_ptr as *const u8, vert_bytes as usize) };
        bytemuck::cast_slice::<u8, MeshVertex>(v_bytes_slice).to_vec()
    }

    #[test]
    fn gpu_vertex_buffer_matches_legacy_normalize_shape_times_proj_scale() {
        let tol = 1e-4_f32;
        let shapes = [(0u32, 4usize), (1, 8), (2, 6), (3, 12), (4, 20)];
        for (shape, count) in shapes {
            let verts = run_polytope_vertices(shape);
            for (i, vert) in verts.iter().enumerate().take(count) {
                let want = legacy_expected_position(shape, i);
                let got = vert.position;
                for c in 0..3 {
                    assert!(
                        (got[c] - want[c]).abs() < tol,
                        "shape={shape} vertex {i} component {c}: got {} want {}",
                        got[c],
                        want[c],
                    );
                }
            }
            // Magnitude check on every active vertex: the bake-in is
            // 0.25, so the post-normalise distance from origin is
            // exactly 0.25.
            for (i, vert) in verts.iter().enumerate().take(count) {
                let p = vert.position;
                let mag = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                assert!(
                    (mag - 0.25).abs() < tol,
                    "shape={shape} vertex {i} magnitude {mag} != 0.25 ± {tol}",
                );
            }
        }
    }
}
