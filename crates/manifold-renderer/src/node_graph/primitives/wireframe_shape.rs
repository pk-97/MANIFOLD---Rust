//! `node.wireframe_shape` — emit one of five wireframe shape
//! vertex + edge sets as `Array<MeshVertex>` + `Array<EdgePair>`.
//!
//! Five shapes packed behind one `shape` enum: Tetrahedron, Cube,
//! Octahedron, Icosahedron, Dodecahedron. Positions are normalised to
//! the unit sphere; normals point radially outward. Edges carry the
//! shape's full wireframe topology — feed both outputs through
//! `node.rotate_3d` + `node.project_3d` + `node.render_lines` (with
//! the `edges` input wired) to draw the wireframe.
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
    edge_capacity: u32,
    _pad: u32,
}

crate::primitive! {
    name: WireframeShape,
    type_id: "node.wireframe_shape",
    purpose: "Emit one of five wireframe shapes (Tetrahedron / Cube / Octahedron / Icosahedron / Dodecahedron) as a paired Array<MeshVertex> + Array<EdgePair>. Vertices are normalised to the unit sphere; edges carry the full wireframe topology. Pipe both outputs through node.rotate_3d → node.project_3d → node.render_lines (with the `edges` input wired) to draw the wireframe. Clip-trigger mode cycles the shape on each retrigger via the shared ClipTriggerCycle uniqueness invariant.",
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
            edge_capacity,
            _pad: 0,
        };

        // Dispatch enough threads to fill the larger of the two
        // buffers. Each thread writes its slot in both buffers (or
        // pads / sentinels the slot if out of range for the current
        // shape).
        let total = vert_capacity.max(edge_capacity);

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
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: edge_dst,
                    offset: 0,
                },
            ],
            [total.div_ceil(64), 1, 1],
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
