//! `node.flatten_4d` — project an `Array<Vec4Vertex>` to
//! `Array<CurvePoint>` via two-stage perspective (4D → 3D → 2D).
//!
//! Bit-exact WGSL port of `generators::generator_math::project_4d`.
//! Drives Tesseract / Duocylinder decomposition:
//! base verts → Rotate4D → Project4D → (line renderer).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`proj_scale`,
/// `proj_dist`), then the derived `active_count` (declared `derived_uniforms`,
/// carried as f32 — exact for these small vertex counts), then the codegen-
/// injected `dispatch_count` (= the output capacity, the dispatch guard). 4
/// words = 16 bytes. `dispatch_count` is the guard; slots in
/// `[active_count, dispatch_count)` collapse to origin in the body.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Project4DUniforms {
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: Project4D,
    type_id: "node.flatten_4d",
    purpose: "Project an Array<Vec4Vertex> to Array<CurvePoint> via two-stage perspective (4D → 3D collapse with f = proj_dist / (proj_dist - w), then 3D → 2D with s = proj_dist / (proj_dist + p3z)). Bit-exact port of generator_math::project_4d. The 4D-equivalent of node.flatten_3d for Tesseract / Duocylinder decomposition.",
    inputs: {
        in: Array(Vec4Vertex) required,
    },
    outputs: {
        out: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("proj_scale"),
            label: "Projection Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("proj_dist"),
            label: "Projection Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "PROJ_SCALE = 0.25 matches the legacy Wireframe / Tesseract default. The 4D → 3D collapse uses proj_dist as the W-axis camera distance; small values produce strong 4D distortion. Active count = input vertex buffer's capacity; output must be sized at least as large.",
    examples: [],
    picker: { label: "Flatten 4D → 3D", category: Atom },
    summary: "Flattens 4D geometry like a tesseract down toward 3D, the first step in drawing a four-dimensional shape.",
    category: Geometry3D,
    role: Filter,
    aliases: ["project 4d", "flatten", "4d to 3d"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/project_4d_body.wgsl"),
    derived_uniforms: ["active_count"],
}

impl Primitive for Project4D {
    /// Output `out` is sized to match input `in` — one projected
    /// `CurvePoint` per input 4D vertex.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let proj_scale = match ctx.params.get("proj_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.25,
        };
        let proj_dist = match ctx.params.get("proj_dist") {
            Some(ParamValue::Float(f)) => *f,
            _ => 3.0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let point_size = std::mem::size_of::<CurvePoint>() as u64;
        let in_count = (in_buf.size / vertex_size) as u32;
        let out_capacity = (out_buf.size / point_size) as u32;
        let active_count = in_count.min(out_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path, type-changing in/out + derived active_count).
            // project_4d.wgsl is the parity oracle (and the existing executor
            // test compares against generator_math::project_4d).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flatten_4d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flatten_4d",
            )
        });

        let uniforms = Project4DUniforms {
            proj_scale,
            proj_dist,
            active_count: active_count as f32,
            dispatch_count: out_capacity,
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
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [out_capacity.div_ceil(256), 1, 1],
            "node.flatten_4d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn project_4d_declares_vec4_in_and_linepoint_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec4_layout = ArrayType::of_known::<Vec4Vertex>();
        let point_layout = ArrayType::of_known::<CurvePoint>();
        assert_eq!(Project4D::TYPE_ID, "node.flatten_4d");
        assert_eq!(Project4D::INPUTS.len(), 1);
        assert_eq!(Project4D::INPUTS[0].ty, PortType::Array(vec4_layout));
        assert_eq!(Project4D::OUTPUTS.len(), 1);
        assert_eq!(Project4D::OUTPUTS[0].ty, PortType::Array(point_layout));
    }

    #[test]
    fn project_4d_has_scale_and_dist_params() {
        let names: Vec<&str> = Project4D::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["proj_scale", "proj_dist"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Project4D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.flatten_4d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! GPU parity tests against `generators::generator_math::project_4d`.
    //! This module is the test that would have caught the "Tesseract
    //! and Duocylinder spawn in the top right" bug at primitive-level
    //! the first time it ran — the pre-fix WGSL added a `+0.5` shift
    //! that the legacy CPU reference does not. Origin-centered output
    //! is the contract `node.draw_lines` consumes; deviating from
    //! it doubles the screen offset and the wireframe clusters in the
    //! top-right of the output.
    //!
    //! Pattern matches `wireframe_shape::gpu_tests` —
    //! `pre_bind_array`, `execute_frame_with_gpu`, read back via
    //! `mapped_ptr` on the shared MTLBuffer.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::generator_math::project_4d as legacy_project_4d;
    use crate::generators::mesh_common::{CurvePoint, Vec4Vertex};
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::effect_node::{
        EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
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

    use super::Project4D;

    /// Test-only no-op source for `Array<Vec4Vertex>`. Used to satisfy
    /// the chain validator's "every required input is wired" check
    /// when testing `Project4D` in isolation — the caller pre-binds a
    /// shared MTLBuffer to this node's `out` resource and CPU-writes
    /// the vertices before executing the frame. The `run` method is
    /// intentionally a no-op: data already lives in the buffer.
    struct Vec4Source {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl Vec4Source {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.vec4_source"),
                inputs: vec![],
                outputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("out"),
                    ty: PortType::Array(ArrayType::of_known::<Vec4Vertex>()),
                    kind: PortKind::Output,
                    required: false,
                }],
            }
        }
    }

    impl EffectNode for Vec4Source {
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

        /// Output capacity comes from a `max_capacity` Int param —
        /// not actually used in this test fixture; the executor's
        /// pre-allocator is bypassed by `pre_bind_array` so we
        /// don't need a meaningful number here.
        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(0)
        }
    }

    /// Test-only sink: consumes Project4D's `out` Array(CurvePoint)
    /// so the planner allocates the resource (d84ae560 skips outputs
    /// with no downstream consumer).
    struct CurvePointSink {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl CurvePointSink {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.curve_point_sink"),
                inputs: vec![NodePort {
                    name: std::borrow::Cow::Borrowed("in"),
                    ty: PortType::Array(ArrayType::of_known::<CurvePoint>()),
                    kind: PortKind::Input,
                    required: true,
                }],
                outputs: vec![],
            }
        }
    }

    impl EffectNode for CurvePointSink {
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

    fn resource_for(plan: &ExecutionPlan, node: NodeInstanceId, port: &str, is_input: bool) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                let pool = if is_input { &step.inputs } else { &step.outputs };
                for &(name, id) in pool {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no {} port `{port}` on node {node:?}", if is_input { "input" } else { "output" });
    }

    /// Run Project4D standalone with the given 4D input vertices and
    /// projection params; read back the projected CurvePoints from
    /// the shared output buffer. `capacity` is the buffer slot count
    /// (may be larger than `verts.len()` — slots past
    /// `active_count = verts.len()` should collapse to `(0, 0)`).
    fn run_project_4d(
        verts: &[Vec4Vertex],
        capacity: u32,
        proj_scale: f32,
        proj_dist: f32,
    ) -> Vec<CurvePoint> {
        assert!(verts.len() as u32 <= capacity);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Vec4Source::new()));
        let pj = g.add_node(Box::new(Project4D::new()));
        g.connect((src, "out"), (pj, "in")).unwrap();
        g.set_param(pj, "proj_scale", ParamValue::Float(proj_scale)).unwrap();
        g.set_param(pj, "proj_dist", ParamValue::Float(proj_dist)).unwrap();
        let sink = g.add_node(Box::new(CurvePointSink::new()));
        g.connect((pj, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_in = resource_for(&plan, src, "out", false);
        let r_out = resource_for(&plan, pj, "out", false);

        let vert_bytes = (capacity as u64) * std::mem::size_of::<Vec4Vertex>() as u64;
        let point_bytes = (capacity as u64) * std::mem::size_of::<CurvePoint>() as u64;
        let in_buf = device.create_buffer_shared(vert_bytes);
        let out_buf = device.create_buffer_shared(point_bytes);

        // CPU-write the 4D input vertices into the shared input
        // buffer. Slots past `verts.len()` keep their zero-init
        // bytes (Vec4Vertex { position: [0,0,0,0] }) so the test
        // can read them back as the "inactive" sentinel.
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(verts));
        }

        let mut backend = MetalBackend::new(device.arc(), 1, 1, format);
        let _in_slot = backend.pre_bind_array(r_in, in_buf);
        let out_slot = backend.pre_bind_array(r_out, out_buf);

        let mut native_enc = device.create_encoder("project_4d-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_buf = exec
            .backend()
            .array_buffer(out_slot)
            .expect("output buffer retained");
        let ptr = out_buf.mapped_ptr().expect("shared output buffer");
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, point_bytes as usize) };
        bytemuck::cast_slice::<u8, CurvePoint>(bytes).to_vec()
    }

    /// Hero parity test — origin-centered output, bit-equal to the
    /// legacy CPU reference. If anyone reintroduces the `+0.5` shift
    /// in `project_4d.wgsl`, every assertion in this loop fails on
    /// the FIRST vertex (because the legacy reference doesn't add
    /// the offset and the GPU output would).
    #[test]
    fn gpu_output_matches_legacy_project_4d_origin_centered() {
        // Hand-picked 4D vertices across the cube/duocylinder regime.
        // Includes signs, off-axis, and a couple near the W-camera
        // singularity for f-clamp coverage.
        let verts = vec![
            Vec4Vertex { position: [0.25, 0.0, 0.0, 0.0] },
            Vec4Vertex { position: [0.0, 0.25, 0.0, 0.0] },
            Vec4Vertex { position: [0.1, -0.1, 0.1, 0.1] },
            Vec4Vertex { position: [-0.2, 0.2, -0.2, 0.2] },
            Vec4Vertex { position: [0.25, 0.25, 0.25, -0.25] },
            Vec4Vertex { position: [-0.25, -0.25, 0.0, 0.25] },
            Vec4Vertex { position: [0.0, 0.0, 0.5, 0.0] },
            Vec4Vertex { position: [0.3, 0.2, 0.1, 0.4] },
        ];
        let proj_scale = 0.25;
        let proj_dist = 3.0;
        let capacity = 16u32;
        let out = run_project_4d(&verts, capacity, proj_scale, proj_dist);

        let tol = 1e-5_f32;
        for (i, v) in verts.iter().enumerate() {
            let (px, py, _pz) = legacy_project_4d(
                v.position[0], v.position[1], v.position[2], v.position[3], proj_dist,
            );
            // Legacy already bakes PROJ_SCALE = 0.25 into the return,
            // so when proj_scale == 0.25 (the default the WGSL uses)
            // the GPU output should equal the legacy reference
            // directly. Re-derive without baking when proj_scale
            // differs.
            let (want_x, want_y) = if (proj_scale - 0.25).abs() < 1e-6 {
                (px, py)
            } else {
                let rescale = proj_scale / 0.25;
                (px * rescale, py * rescale)
            };
            let [got_x, got_y] = out[i].xy;
            assert!(
                (got_x - want_x).abs() < tol && (got_y - want_y).abs() < tol,
                "vertex {i}: got ({got_x:?}, {got_y:?}) want ({want_x:?}, {want_y:?}) — \
                 (regression: did someone reintroduce the `+0.5` shift in project_4d.wgsl?)",
            );
            // Doubly explicit: origin-centered means small inputs
            // produce small outputs. Anything near (0.5, 0.5) is the
            // top-right-cluster bug.
            assert!(
                got_x.abs() < 1.0 && got_y.abs() < 1.0,
                "vertex {i}: ({got_x}, {got_y}) drifted outside the [-1, 1] curve-space band — \
                 likely the `+0.5` shift returned",
            );
        }

        // Inactive slots must be (0.0, 0.0) — render_lines treats
        // those as degenerate dots that contribute nothing. The
        // pre-fix shader collapsed them to (0.5, 0.5), which would
        // draw a stray dot at the centre of the screen for every
        // unused capacity slot.
        for (i, pt) in out.iter().enumerate().skip(verts.len()) {
            assert_eq!(
                pt.xy,
                [0.0, 0.0],
                "inactive slot {i} must collapse to origin, got {:?}",
                pt.xy,
            );
        }
    }
}

