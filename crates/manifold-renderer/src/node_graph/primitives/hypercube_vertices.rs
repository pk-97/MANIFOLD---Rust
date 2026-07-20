//! `node.hypercube_points` — emit the 16 corner vertices of a 4D
//! hypercube as `Array<Vec4Vertex>`, with a continuous `dimension`
//! control that collapses the higher axes toward zero so the shape
//! morphs **point → line → square → cube → tesseract** as `dimension`
//! ramps from 1 to 4.
//!
//! The 4D-side counterpart of [`super::polytope_vertices`]; pair with
//! [`super::edges_from_hypercube`] for the matching wireframe topology
//! and feed both into `node.rotate_4d → node.flatten_4d →
//! node.draw_lines` (with the `edges` input wired).
//!
//! Vertex `i` has coordinates `(sx, sy, sz, sw) * 0.125 * present(axis)`
//! where the sign pattern is `(sign(i&1), sign(i&2), sign(i&4),
//! sign(i&8))` and `present(axis) = clamp(dimension - axis, 0, 1)` for
//! axis index `x=0, y=1, z=2, w=3`. At `dimension = 4` every axis is
//! fully present and the output is bit-identical to the legacy
//! `generate_tesseract_vertices` bake (`sign * 0.125`). Lower
//! `dimension` lerps a higher axis to zero, collapsing the 16 corners
//! onto a lower-dimensional cube (cube at 3, square at 2, line at 1) —
//! the edge topology stays the full 32-edge bit-flip set, so the
//! collapsed edges render as zero-length (invisible / dots), which is
//! exactly the "ramp the 4th coord from 0" reveal.
//!
//! The `0.125 = 0.25 / 2` scaling normalises the max-magnitude corner
//! (`sqrt(4) = 2`) to magnitude 0.25 — the legacy `PROJ_SCALE`
//! screen-fit factor — so downstream `project_4d.proj_scale` defaults
//! to 1.0 and the outer-card Scale binds to it directly (no graph-side
//! math node). Same convention as `wireframe_shape` / `polytope_vertices`.
//! 4D perspective is non-linear in `w`, so this bake does not reproduce
//! the legacy generator's *projected* pixels bit-exactly — accepted
//! trade-off, identical to the prior `generate_tesseract_vertices`.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const HYPERCUBE_VERTEX_COUNT: u32 = 16;

/// Generated-codegen uniform layout: the `dimension` param (f32) then the
/// codegen-injected `dispatch_count` (= vertex capacity, the guard), padded
/// to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    dimension: f32,
    dispatch_count: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: HypercubeVertices,
    type_id: "node.hypercube_points",
    purpose: "Emit the 16 corner vertices of a 4D hypercube as Array<Vec4Vertex>, with a continuous `dimension` control (1..4) that collapses the higher axes toward zero so the shape morphs point → line → square → cube → tesseract. At dimension=4 it is the full tesseract. Pair with node.hypercube_edges and feed both into node.rotate_4d → node.flatten_4d → node.draw_lines (with the `edges` input wired). The 4D counterpart of node.platonic_solid_points.",
    inputs: {
        // Port-shadows the `dimension` param. Wire an LFO / envelope /
        // macro here to animate the square→cube→tesseract morph live;
        // leave unwired and the param's static value picks the shape.
        dimension: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(Vec4Vertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("dimension"),
            label: "Dimension",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((1.0, 4.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Vertex i = (sign(i&1), sign(i&2), sign(i&4), sign(i&8)) * 0.125 * clamp(dimension - axis, 0, 1) per axis (x=0,y=1,z=2,w=3). The 0.125 = 0.25 / 2 bake normalises the corner magnitude (sqrt(4)=2) to 0.25 (legacy PROJ_SCALE) so project_4d.proj_scale defaults to 1.0. dimension=4 is the full tesseract (bit-exact to the legacy generate_tesseract_vertices bake); lower values collapse a higher axis to zero (cube at 3, square at 2, line at 1). Output is pre-sized to 16. Wire `dimension` from an LFO / macro for the live dimension-morph reveal.",
    examples: [],
    picker: { label: "Hypercube Points (4D)", category: Atom },
    summary: "Builds the corner points of a hypercube. The Dimension knob morphs it from a flat square up through a cube to a full 4D tesseract — wire it to an LFO to animate the reveal.",
    category: Geometry3D,
    role: Source,
    aliases: ["tesseract", "hypercube", "hypercube vertices", "4d cube", "polytope", "dimension morph"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/hypercube_vertices_body.wgsl"),
}

impl Primitive for HypercubeVertices {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "vertices" {
            Some(HYPERCUBE_VERTEX_COUNT)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Port-shadows-param: a wired `dimension` scalar wins over the
        // param, falling back to the param's static value when unwired.
        let dimension = ctx.scalar_or_param("dimension", 4.0).clamp(1.0, 4.0);

        let Some(vert_dst) = ctx.outputs.array("vertices") else {
            log::warn!(
                "node.hypercube_points: no GpuBuffer bound to output port `vertices` — \
                 the chain build did not pre-allocate the Array<Vec4Vertex> output.",
            );
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let vert_capacity = (vert_dst.size / vertex_size) as u32;
        if vert_capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // source path). hypercube_vertices.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.hypercube_points standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.hypercube_points",
            )
        });

        let uniforms = Uniforms {
            dimension,
            dispatch_count: vert_capacity,
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
                    buffer: vert_dst,
                    offset: 0,
                },
            ],
            [vert_capacity.div_ceil(64), 1, 1],
            "node.hypercube_points",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_dimension_input_and_vec4_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let vec4_layout = ArrayType::of_known::<Vec4Vertex>();
        assert_eq!(HypercubeVertices::TYPE_ID, "node.hypercube_points");
        assert_eq!(HypercubeVertices::INPUTS.len(), 1);
        assert_eq!(HypercubeVertices::INPUTS[0].name, "dimension");
        assert!(!HypercubeVertices::INPUTS[0].required);
        assert_eq!(
            HypercubeVertices::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(HypercubeVertices::OUTPUTS.len(), 1);
        assert_eq!(HypercubeVertices::OUTPUTS[0].name, "vertices");
        assert_eq!(
            HypercubeVertices::OUTPUTS[0].ty,
            PortType::Array(vec4_layout)
        );
    }

    #[test]
    fn dimension_param_defaults_to_full_tesseract() {
        let p = HypercubeVertices::PARAMS
            .iter()
            .find(|p| p.name == "dimension")
            .unwrap();
        assert_eq!(p.ty, ParamType::Float);
        assert_eq!(p.default, ParamValue::Float(4.0));
        assert_eq!(p.range, Some((1.0, 4.0)));
    }

    #[test]
    fn output_capacity_is_sixteen() {
        let prim = HypercubeVertices::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &params, &[]),
            Some(HYPERCUBE_VERTEX_COUNT)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None
        );
    }

    #[test]
    fn registers_with_palette() {
        let prim = HypercubeVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.hypercube_points");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! GPU parity for the hypercube corner bake. The reference is the
    //! closed-form `(sign(i&bit)) * 0.125 * clamp(dimension - axis, 0, 1)`
    //! the legacy `generate_tesseract_vertices` produced (which baked
    //! `sign * 0.125` — i.e. exactly the `dimension = 4` case here).
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::mesh_common::Vec4Vertex;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    };
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };

    use super::{HYPERCUBE_VERTEX_COUNT, HypercubeVertices};

    struct VertexSink {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl VertexSink {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.vec4_vertex_sink"),
                inputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Array(ArrayType::of_known::<Vec4Vertex>()),
                    kind: PortKind::Input,
                    required: true,
                }],
                outputs: vec![],
            }
        }
    }

    impl EffectNode for VertexSink {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule { crate::node_graph::depth_rule::DepthRule::Terminal } // test fixture
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

    fn expected_position(i: u32, dimension: f32) -> [f32; 4] {
        let k = 0.125_f32;
        let sign = |bit: u32| if (i & bit) != 0 { 1.0_f32 } else { -1.0_f32 };
        let present = |axis: f32| (dimension - axis).clamp(0.0, 1.0);
        [
            sign(1) * k * present(0.0),
            sign(2) * k * present(1.0),
            sign(4) * k * present(2.0),
            sign(8) * k * present(3.0),
        ]
    }

    fn run_hypercube_vertices(dimension: f32) -> Vec<Vec4Vertex> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let hv = g.add_node(Box::new(HypercubeVertices::new()));
        g.set_param(hv, "dimension", ParamValue::Float(dimension))
            .unwrap();
        let sink = g.add_node(Box::new(VertexSink::new()));
        g.connect((hv, "vertices"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_verts = output_resource(&plan, hv, "vertices");
        let vert_bytes =
            (HYPERCUBE_VERTEX_COUNT as u64) * std::mem::size_of::<Vec4Vertex>() as u64;
        let vert_buf = device.create_buffer_shared(vert_bytes);

        let mut backend = MetalBackend::new(device.arc(), 1, 1, format);
        let vert_slot = backend.pre_bind_array(r_verts, vert_buf);

        let mut native_enc = device.create_encoder("hypercube-vertices-test");
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
        bytemuck::cast_slice::<u8, Vec4Vertex>(v_bytes_slice).to_vec()
    }

    #[test]
    fn gpu_vertices_match_closed_form_at_each_dimension() {
        let tol = 1e-6_f32;
        // 4.0 = full tesseract (legacy bake parity); 3/2/1 = the cube /
        // square / line collapse; 2.5 = a mid-morph (z half-present).
        for dimension in [4.0_f32, 3.0, 2.5, 2.0, 1.0] {
            let verts = run_hypercube_vertices(dimension);
            for i in 0..HYPERCUBE_VERTEX_COUNT {
                let want = expected_position(i, dimension);
                let got = verts[i as usize].position;
                for c in 0..4 {
                    assert!(
                        (got[c] - want[c]).abs() < tol,
                        "dimension={dimension} vertex {i} component {c}: got {} want {}",
                        got[c],
                        want[c],
                    );
                }
            }
        }
    }

    #[test]
    fn dimension_four_is_full_unit_corner() {
        // The full tesseract: every coord is ±0.125, magnitude 0.25.
        let verts = run_hypercube_vertices(4.0);
        for (i, v) in verts.iter().enumerate() {
            let p = v.position;
            for (c, comp) in p.iter().enumerate() {
                assert!(
                    (comp.abs() - 0.125).abs() < 1e-6,
                    "vertex {i} component {c} = {comp} (want ±0.125)",
                );
            }
            let mag = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2] + p[3] * p[3]).sqrt();
            assert!((mag - 0.25).abs() < 1e-5, "vertex {i} magnitude {mag} != 0.25");
        }
    }
}
