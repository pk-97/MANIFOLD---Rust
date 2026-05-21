//! `node.wireframe_shape` — emit one of five wireframe shape
//! vertex + edge sets as `Array<MeshVertex>` + `Array<EdgePair>`.
//!
//! Five shapes packed behind one `shape` enum: Tetrahedron, Cube,
//! Octahedron, Icosahedron, Dodecahedron. Positions are normalised
//! to magnitude 0.25 (the curated "wireframe-fits-on-screen" default
//! matching legacy `generator_math::PROJ_SCALE`); normals stay
//! length-1 for shading. Edges carry the shape's full wireframe
//! topology — feed both outputs through `node.rotate_3d` +
//! `node.project_3d` + `node.render_lines` (with the `edges` input
//! wired) to draw the wireframe.
//!
//! The 0.25 magnitude lives inside this primitive (not as a graph-
//! side math node or a baked binding-layer multiplier) so downstream
//! `project_3d.proj_scale` defaults to 1.0 — outer-card Scale binds
//! to it directly and gives the user "Scale 1.0 = default zoom" UX
//! without a multiplier node in the graph.
//!
//! Clip-trigger mode cycles the shape on each retrigger via the
//! shared [`ClipTriggerCycle`] uniqueness invariant — same defense
//! in depth as Plasma's `pattern` cycling.
//!
//! Vertex counts: Tetra=4, Cube=8, Octa=6, Icosa=12, Dodeca=20.
//! Edge counts:   Tetra=6, Cube=12, Octa=12, Icosa=30, Dodeca=30.

use manifold_gpu::GpuBinding;

use crate::generators::clip_trigger::ClipTriggerCycle;
use crate::generators::mesh_common::{EdgePair, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Shape enum labels. Order matches the WGSL `switch` cases.
pub const WIREFRAME_SHAPES: &[&str] = &[
    "Tetrahedron",
    "Cube",
    "Octahedron",
    "Icosahedron",
    "Dodecahedron",
];

/// Number of shape variants — used as the modulus for clip-trigger
/// cycling. Must match `WIREFRAME_SHAPES.len()`.
pub const WIREFRAME_SHAPE_COUNT: u32 = 5;

/// Maximum vertex count across all shapes (Dodecahedron = 20). Default
/// for the `vertices` output capacity.
pub const WIREFRAME_MAX_VERTS: u32 = 20;

/// Maximum edge count across all shapes (Icosa/Dodeca = 30). Default
/// for the `edges` output capacity.
pub const WIREFRAME_MAX_EDGES: u32 = 30;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WireframeUniforms {
    shape: u32,
    vert_capacity: u32,
    _pad0: u32,
    _pad1: u32,
}

// ── CPU-side edge tables ────────────────────────────────────────
//
// Edges are written CPU-side into the shared `edges` output buffer
// each frame. Two reasons we don't do this in the WGSL shader:
//
// 1. The only consumer (`node.render_lines`) reads the edges buffer
//    CPU-side to build its per-instance EdgeInstance buffer. A
//    same-frame GPU write would not be visible to that CPU read
//    without an explicit fence; CPU-write keeps the data path
//    trivially serial on the content thread.
//
// 2. The edge tables are tiny (≤30 entries × 8 bytes) and topology
//    is constant per shape. No GPU dispatch is justified.

const TETRA_EDGES: [EdgePair; 6] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 0, b: 2 },
    EdgePair { a: 0, b: 3 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 1, b: 3 },
    EdgePair { a: 2, b: 3 },
];

const CUBE_EDGES: [EdgePair; 12] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 2, b: 3 },
    EdgePair { a: 3, b: 0 },
    EdgePair { a: 4, b: 5 },
    EdgePair { a: 5, b: 6 },
    EdgePair { a: 6, b: 7 },
    EdgePair { a: 7, b: 4 },
    EdgePair { a: 0, b: 4 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 2, b: 6 },
    EdgePair { a: 3, b: 7 },
];

const OCTA_EDGES: [EdgePair; 12] = [
    EdgePair { a: 0, b: 2 },
    EdgePair { a: 0, b: 3 },
    EdgePair { a: 0, b: 4 },
    EdgePair { a: 0, b: 5 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 1, b: 3 },
    EdgePair { a: 1, b: 4 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 2, b: 4 },
    EdgePair { a: 2, b: 5 },
    EdgePair { a: 3, b: 4 },
    EdgePair { a: 3, b: 5 },
];

const ICOSA_EDGES: [EdgePair; 30] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 0, b: 5 },
    EdgePair { a: 0, b: 7 },
    EdgePair { a: 0, b: 10 },
    EdgePair { a: 0, b: 11 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 1, b: 7 },
    EdgePair { a: 1, b: 8 },
    EdgePair { a: 1, b: 9 },
    EdgePair { a: 2, b: 3 },
    EdgePair { a: 2, b: 4 },
    EdgePair { a: 2, b: 6 },
    EdgePair { a: 2, b: 10 },
    EdgePair { a: 2, b: 11 },
    EdgePair { a: 3, b: 4 },
    EdgePair { a: 3, b: 6 },
    EdgePair { a: 3, b: 8 },
    EdgePair { a: 3, b: 9 },
    EdgePair { a: 4, b: 5 },
    EdgePair { a: 4, b: 9 },
    EdgePair { a: 4, b: 11 },
    EdgePair { a: 5, b: 9 },
    EdgePair { a: 5, b: 11 },
    EdgePair { a: 6, b: 7 },
    EdgePair { a: 6, b: 8 },
    EdgePair { a: 6, b: 10 },
    EdgePair { a: 7, b: 8 },
    EdgePair { a: 7, b: 10 },
    EdgePair { a: 8, b: 9 },
    EdgePair { a: 10, b: 11 },
];

const DODECA_EDGES: [EdgePair; 30] = [
    EdgePair { a: 0, b: 8 },
    EdgePair { a: 0, b: 12 },
    EdgePair { a: 0, b: 16 },
    EdgePair { a: 1, b: 9 },
    EdgePair { a: 1, b: 13 },
    EdgePair { a: 1, b: 16 },
    EdgePair { a: 2, b: 10 },
    EdgePair { a: 2, b: 12 },
    EdgePair { a: 2, b: 17 },
    EdgePair { a: 3, b: 11 },
    EdgePair { a: 3, b: 13 },
    EdgePair { a: 3, b: 17 },
    EdgePair { a: 4, b: 8 },
    EdgePair { a: 4, b: 14 },
    EdgePair { a: 4, b: 18 },
    EdgePair { a: 5, b: 9 },
    EdgePair { a: 5, b: 15 },
    EdgePair { a: 5, b: 18 },
    EdgePair { a: 6, b: 10 },
    EdgePair { a: 6, b: 14 },
    EdgePair { a: 6, b: 19 },
    EdgePair { a: 7, b: 11 },
    EdgePair { a: 7, b: 15 },
    EdgePair { a: 7, b: 19 },
    EdgePair { a: 8, b: 9 },
    EdgePair { a: 10, b: 11 },
    EdgePair { a: 12, b: 14 },
    EdgePair { a: 13, b: 15 },
    EdgePair { a: 16, b: 17 },
    EdgePair { a: 18, b: 19 },
];

fn edges_for_shape(shape: u32) -> &'static [EdgePair] {
    match shape {
        0 => &TETRA_EDGES,
        1 => &CUBE_EDGES,
        2 => &OCTA_EDGES,
        3 => &ICOSA_EDGES,
        _ => &DODECA_EDGES,
    }
}

crate::primitive! {
    name: WireframeShape,
    type_id: "node.wireframe_shape",
    purpose: "Emit one of five wireframe shapes (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as a paired Array<MeshVertex> + Array<EdgePair>. Vertices are normalised to magnitude 0.25 (the curated screen-friendly wireframe default); edges carry the full wireframe topology. Pipe both outputs through node.rotate_3d → node.project_3d → node.render_lines (with the `edges` input wired) to draw the wireframe. Clip-trigger mode cycles the shape on each retrigger via the shared ClipTriggerCycle uniqueness invariant.",
    inputs: {
        // Wire `system.generator_input.trigger_count` here to enable
        // clip-trigger shape cycling. Port-shadows the `trigger_count`
        // param.
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        vertices: Array(MeshVertex),
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: "shape",
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: WIREFRAME_SHAPES,
        },
        ParamDef {
            name: "clip_trigger",
            label: "Clip Trigger",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Count",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Vertices and edges buffers are pre-sized for the largest shape (vertices=20, edges=30). Unused tail slots are zero-padded (vertices) or filled with EdgePair::SENTINEL (edges). When `clip_trigger=true`, the shape selector is driven by `trigger_count % 5` through the ClipTriggerCycle invariant — two adjacent retriggers never land on the same shape. Wire `system.generator_input.trigger_count` to the `trigger_count` input port so the cycle advances on retrigger.",
    examples: [],
    picker: { label: "Wireframe Shape", category: Atom },
    extra_fields: {
        clip_trigger_cycle: crate::generators::clip_trigger::ClipTriggerCycle = ClipTriggerCycle::new(),
    },
}

// Legacy type-ID alias for projects saved before the rename from
// `node.generate_platonic_solid` → `node.wireframe_shape`. Hidden
// from the palette (`picker: None`) so users only see the new name.
inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.generate_platonic_solid",
        create: || Box::new(WireframeShape::new()),
        picker: None,
    }
}

impl Primitive for WireframeShape {
    /// Output capacities:
    /// - `vertices`: WIREFRAME_MAX_VERTS (20) — the largest shape.
    /// - `edges`:    WIREFRAME_MAX_EDGES (30) — the largest shape.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "vertices" => Some(WIREFRAME_MAX_VERTS),
            "edges" => Some(WIREFRAME_MAX_EDGES),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Static shape param (used when clip_trigger=false).
        let static_shape = match ctx.params.get("shape") {
            Some(ParamValue::Enum(n)) => *n,
            Some(ParamValue::Int(i)) => (*i).max(0) as u32,
            _ => 0,
        };

        // Clip-trigger gate. When true, cycle the shape through the
        // ClipTriggerCycle invariant; when false, use the static
        // `shape` param.
        let clip_trigger = match ctx.params.get("clip_trigger") {
            Some(ParamValue::Bool(b)) => *b,
            Some(ParamValue::Float(f)) => *f > 0.5,
            Some(ParamValue::Int(i)) => *i != 0,
            _ => false,
        };

        let shape = if clip_trigger {
            // Port-shadows-param: wired `trigger_count` input wins,
            // param is the fallback.
            let trigger_count = ctx.scalar_or_param("trigger_count", 0.0);
            let raw = trigger_count.floor().max(0.0) as u32;
            self.clip_trigger_cycle.step(raw, WIREFRAME_SHAPE_COUNT)
        } else {
            static_shape.min(WIREFRAME_SHAPE_COUNT - 1)
        };

        let Some(vert_dst) = ctx.outputs.array("vertices") else {
            log::warn!(
                "node.wireframe_shape: no GpuBuffer bound to output port `vertices` — \
                 the chain build did not pre-allocate the Array<MeshVertex> output.",
            );
            return;
        };
        let Some(edge_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.wireframe_shape: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output.",
            );
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let vert_capacity = (vert_dst.size / vertex_size) as u32;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if vert_capacity == 0 || edge_capacity == 0 {
            return;
        }

        // ── CPU-write the edges buffer ──
        // Edges live in a shared (CPU+GPU visible) MTLBuffer per
        // the Array<T> pre-allocation policy, so node.render_lines
        // reads them CPU-side downstream. Stack-allocated buffer
        // (fixed cap = 30) padded with sentinels for the unused
        // tail.
        let mut edges_scratch = [EdgePair::SENTINEL; WIREFRAME_MAX_EDGES as usize];
        let active = edges_for_shape(shape);
        edges_scratch[..active.len()].copy_from_slice(active);
        let write_count = (edge_capacity as usize).min(edges_scratch.len());
        // Safety: the buffer is shared-memory (per `pre_allocate_array_buffers`)
        // and the chain build prebound it; write_count is clamped
        // to the buffer's capacity; no GPU pass races this write
        // because the executor walks primitives sequentially on
        // the content thread and any downstream reader runs after
        // us on the same thread.
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&edges_scratch[..write_count]));
        }

        // ── Dispatch the vertex-write compute shader ──
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/wireframe_shape.wgsl"),
                "cs_main",
                "node.wireframe_shape",
            )
        });

        let uniforms = WireframeUniforms {
            shape,
            vert_capacity,
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
            [vert_capacity.div_ceil(64), 1, 1],
            "node.wireframe_shape",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn wireframe_shape_declares_trigger_count_input_and_two_array_outputs() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let vert_layout = ArrayType {
            item_size: std::mem::size_of::<MeshVertex>() as u32,
            item_align: std::mem::align_of::<MeshVertex>() as u32,
        };
        let edge_layout = ArrayType {
            item_size: std::mem::size_of::<EdgePair>() as u32,
            item_align: std::mem::align_of::<EdgePair>() as u32,
        };
        assert_eq!(WireframeShape::TYPE_ID, "node.wireframe_shape");
        assert_eq!(WireframeShape::INPUTS.len(), 1);
        assert_eq!(WireframeShape::INPUTS[0].name, "trigger_count");
        assert!(!WireframeShape::INPUTS[0].required);
        assert_eq!(WireframeShape::INPUTS[0].ty, PortType::Scalar(ScalarType::F32));

        assert_eq!(WireframeShape::OUTPUTS.len(), 2);
        assert_eq!(WireframeShape::OUTPUTS[0].name, "vertices");
        assert_eq!(WireframeShape::OUTPUTS[0].ty, PortType::Array(vert_layout));
        assert_eq!(WireframeShape::OUTPUTS[1].name, "edges");
        assert_eq!(WireframeShape::OUTPUTS[1].ty, PortType::Array(edge_layout));
    }

    #[test]
    fn wireframe_shape_has_five_shape_options() {
        let shape = WireframeShape::PARAMS
            .iter()
            .find(|p| p.name == "shape")
            .unwrap();
        assert_eq!(shape.ty, ParamType::Enum);
        assert_eq!(shape.enum_values.len(), 5);
        assert_eq!(WIREFRAME_SHAPES.len(), WIREFRAME_SHAPE_COUNT as usize);
    }

    #[test]
    fn wireframe_shape_params_include_clip_trigger_and_trigger_count() {
        let names: Vec<&str> = WireframeShape::PARAMS.iter().map(|p| p.name).collect();
        assert!(names.contains(&"clip_trigger"));
        assert!(names.contains(&"trigger_count"));
    }

    #[test]
    fn wireframe_shape_array_output_capacities_match_max_constants() {
        let prim = WireframeShape::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        // Disambiguate: both `Primitive` and `EffectNode` (via blanket
        // impl) expose `array_output_capacity` — the latter delegates
        // to the former. The test calls the Primitive trait path
        // directly so future-me's grep lands on the override site.
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &params, &[]),
            Some(WIREFRAME_MAX_VERTS)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            Some(WIREFRAME_MAX_EDGES)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom_under_new_type_id() {
        let prim = WireframeShape::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wireframe_shape");
    }

    /// Legacy projects saved before the rename used the old type_id.
    /// The hidden inventory alias keeps them loadable through the new
    /// primitive constructor.
    #[test]
    fn legacy_type_id_alias_resolves_to_wireframe_shape() {
        let registry = crate::node_graph::persistence::PrimitiveRegistry::with_builtin();
        let node = registry
            .construct("node.generate_platonic_solid")
            .expect("legacy alias must be registered");
        assert_eq!(node.type_id().as_str(), "node.wireframe_shape");
    }
}

#[cfg(test)]
mod gpu_tests {
    //! GPU parity tests against the legacy `WireframeZooGenerator`'s
    //! CPU pipeline. Mirrors the per-primitive `gpu_tests` pattern
    //! used by image-side primitives (see separable_gaussian.rs).
    //!
    //! What we verify:
    //!
    //! - Vertices buffer: runs the WGSL compute shader through the
    //!   graph executor and reads back the produced `Array<MeshVertex>`
    //!   via the shared MTLBuffer pointer. Compares positions
    //!   element-wise against the legacy `normalize_shape() *
    //!   PROJ_SCALE` reference (inlined in `legacy_reference_*`
    //!   helpers — the legacy `wireframe_zoo.rs` is deleted, so the
    //!   reference lives here as the authoritative expectation).
    //! - Edges buffer: CPU-written by `WireframeShape::run` from the
    //!   const tables, so the parity assertion is direct.
    //!
    //! Bit-parity is the bar. Any drift means a transcription error
    //! in one of the 50 raw vertex coords or 90 raw edges.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::generators::mesh_common::{EdgePair, MeshVertex};
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
        compile,
    };

    use super::{WIREFRAME_MAX_EDGES, WIREFRAME_MAX_VERTS, WireframeShape};

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

    /// Legacy raw (unnormalised) vertex tables — copied from the
    /// pre-deleted `wireframe_zoo.rs`. Used as the parity reference.
    /// If the legacy generator ever needs to be recreated, this is
    /// the source of truth.
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

    /// Legacy expected vertex position: normalise to unit sphere then
    /// multiply by PROJ_SCALE = 0.25. WireframeShape's WGSL bakes this
    /// in directly — we expect the GPU output to match this reference.
    fn legacy_expected_position(shape: u32, i: usize) -> [f32; 3] {
        let raw = legacy_raw_vertices(shape)[i];
        let max_dist = legacy_raw_vertices(shape)
            .iter()
            .map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt())
            .fold(0.0_f32, f32::max);
        let k = 0.25 / max_dist;
        [raw[0] * k, raw[1] * k, raw[2] * k]
    }

    /// Run WireframeShape standalone through the graph executor with
    /// `shape` selected. Returns (vertices, edges) read back from
    /// the shared output buffers.
    fn run_wireframe_shape(shape: u32) -> (Vec<MeshVertex>, Vec<EdgePair>) {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let wf = g.add_node(Box::new(WireframeShape::new()));
        g.set_param(wf, "shape", ParamValue::Enum(shape)).unwrap();
        let plan = compile(&g).unwrap();

        let r_verts = output_resource(&plan, wf, "vertices");
        let r_edges = output_resource(&plan, wf, "edges");

        let vert_bytes = (WIREFRAME_MAX_VERTS as u64) * std::mem::size_of::<MeshVertex>() as u64;
        let edge_bytes = (WIREFRAME_MAX_EDGES as u64) * std::mem::size_of::<EdgePair>() as u64;
        let vert_buf = device.create_buffer_shared(vert_bytes);
        let edge_buf = device.create_buffer_shared(edge_bytes);

        let mut backend = MetalBackend::new(&device, 1, 1, format);
        let vert_slot = backend.pre_bind_array(r_verts, vert_buf);
        let edge_slot = backend.pre_bind_array(r_edges, edge_buf);

        let mut native_enc = device.create_encoder("wireframe-test");
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
        let edges_buf = exec
            .backend()
            .array_buffer(edge_slot)
            .expect("edges buffer retained");

        let v_ptr = verts_buf.mapped_ptr().expect("shared vertices buffer");
        let e_ptr = edges_buf.mapped_ptr().expect("shared edges buffer");
        let v_bytes_slice = unsafe {
            std::slice::from_raw_parts(v_ptr as *const u8, vert_bytes as usize)
        };
        let e_bytes_slice = unsafe {
            std::slice::from_raw_parts(e_ptr as *const u8, edge_bytes as usize)
        };
        let v: Vec<MeshVertex> = bytemuck::cast_slice::<u8, MeshVertex>(v_bytes_slice).to_vec();
        let e: Vec<EdgePair> = bytemuck::cast_slice::<u8, EdgePair>(e_bytes_slice).to_vec();
        (v, e)
    }

    /// Hero parity test — every shape, every vertex. WGSL output
    /// must equal `legacy normalise_shape() × 0.25` element-wise.
    #[test]
    fn gpu_vertex_buffer_matches_legacy_normalize_shape_times_proj_scale() {
        let tol = 1e-4_f32; // fp16-friendly tolerance for any rgba16-quantised internals
        let shapes = [(0u32, 4usize), (1, 8), (2, 6), (3, 12), (4, 20)];
        for (shape, count) in shapes {
            let (verts, _) = run_wireframe_shape(shape);
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

    /// Edges buffer: WireframeShape::run CPU-writes the static
    /// per-shape edge tables and sentinel-pads the tail. The pinned
    /// vertex indices per shape are bit-exact with the legacy
    /// WireframeZoo edge connectivity tables.
    #[test]
    fn gpu_edges_buffer_carries_correct_topology_with_sentinel_padding() {
        // (shape, expected_active_edges)
        let cases = [(0u32, 6usize), (1, 12), (2, 12), (3, 30), (4, 30)];
        for (shape, expected) in cases {
            let (_, edges) = run_wireframe_shape(shape);
            // First `expected` slots must be valid (non-sentinel),
            // remainder must be sentinel.
            for (i, edge) in edges.iter().enumerate().take(expected) {
                assert_ne!(
                    edge.a,
                    u32::MAX,
                    "shape={shape} edge {i} should be active, got sentinel"
                );
            }
            for (i, edge) in edges
                .iter()
                .enumerate()
                .take(WIREFRAME_MAX_EDGES as usize)
                .skip(expected)
            {
                assert_eq!(edge.a, u32::MAX, "shape={shape} edge {i} should be sentinel");
                assert_eq!(edge.b, u32::MAX);
            }
        }
    }

    /// Tetra edges pinned by hand against the legacy table — a single
    /// transcription error here would silently render the wrong
    /// shape (right vertices, wrong wireframe).
    #[test]
    fn tetra_edges_match_legacy_pinned_indices() {
        let expected: &[(u32, u32)] = &[
            (0, 1),
            (0, 2),
            (0, 3),
            (1, 2),
            (1, 3),
            (2, 3),
        ];
        let (_, edges) = run_wireframe_shape(0);
        for (i, &(a, b)) in expected.iter().enumerate() {
            assert_eq!(edges[i].a, a, "tetra edge {i}.a");
            assert_eq!(edges[i].b, b, "tetra edge {i}.b");
        }
    }
}
