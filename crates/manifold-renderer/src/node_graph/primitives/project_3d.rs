//! `node.project_3d` — project an `Array<MeshVertex>` (3D positions)
//! to an `Array<CurvePoint>` (2D pre-aspect curve space) via either
//! orthographic or perspective projection.
//!
//! **Output is centred at the origin**, matching the convention of
//! every other `Array<CurvePoint>` producer. `node.render_lines`
//! applies the center offset + aspect correction itself; no
//! producer should pre-shift to (0.5, 0.5).
//!
//! Orthographic mode matches WireframeZoo's XY-scale projection
//! bit-for-bit. Perspective mode uses the same projection style as
//! the 4D→2D stage in generator_math::project_4d (s = proj_dist /
//! (proj_dist + z)).

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{CurvePoint, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const PROJECT_3D_MODES: &[&str] = &["Orthographic", "Perspective"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Project3DUniforms {
    active_count: u32,
    capacity: u32,
    mode: u32,
    _pad0: u32,
    proj_scale: f32,
    proj_dist: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Project3D,
    type_id: "node.project_3d",
    purpose: "Project an Array<MeshVertex> (3D positions) to an Array<CurvePoint> (2D pre-aspect curve space) with either orthographic or perspective projection. Output is centred at the origin — node.render_lines applies the center offset itself, so the convention matches every other Array<CurvePoint> producer (generate_lissajous, etc.). For WireframeZoo-shaped decompositions: polytope_vertices → Rotate3D → Project3D → render_lines.",
    inputs: {
        in: Array(MeshVertex) required,
        // Port-shadows-param: control-rate wires take precedence over
        // the inline `proj_scale` / `proj_dist` param values. Lets
        // outer-card sliders drive the zoom factor via math nodes
        // (e.g. `outer_scale × wireframe_zoom_factor → proj_scale`).
        proj_scale: ScalarF32 optional,
        proj_dist: ScalarF32 optional,
    },
    outputs: {
        out: Array(CurvePoint),
    },
    params: [
        ParamDef {
            name: "mode",
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: PROJECT_3D_MODES,
        },
        ParamDef {
            name: "proj_scale",
            label: "Projection Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "proj_dist",
            label: "Projection Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Orthographic mode matches WireframeZoo's bit-exact behaviour (PROJ_SCALE = 0.25 by default; scales xy directly, ignores z). Perspective mode applies s = proj_dist / (proj_dist + z) scaling — useful when the upstream geometry has meaningful depth variation. Active count = input buffer's vertex count; output buffer should be at least the same size.",
    examples: [],
    picker: { label: "Project 3D", category: Atom },
}

impl Primitive for Project3D {
    /// Output `out` is sized to match input `in` — one projected
    /// `CurvePoint` per input vertex.
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
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };
        let proj_scale = ctx.scalar_or_param("proj_scale", 0.25);
        let proj_dist = ctx.scalar_or_param("proj_dist", 3.0);

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let point_size = std::mem::size_of::<CurvePoint>() as u64;
        let in_count = (in_buf.size / vertex_size) as u32;
        let out_capacity = (out_buf.size / point_size) as u32;
        let active_count = in_count.min(out_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/project_3d.wgsl"),
                "cs_main",
                "node.project_3d",
            )
        });

        let uniforms = Project3DUniforms {
            active_count,
            capacity: out_capacity,
            mode,
            _pad0: 0,
            proj_scale,
            proj_dist,
            _pad1: 0.0,
            _pad2: 0.0,
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
            [out_capacity.div_ceil(64), 1, 1],
            "node.project_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn project_3d_declares_mesh_in_and_linepoint_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let point_layout = ArrayType::of_known::<CurvePoint>();
        assert_eq!(Project3D::TYPE_ID, "node.project_3d");
        assert_eq!(Project3D::INPUTS.len(), 3);
        assert_eq!(Project3D::INPUTS[0].name, "in");
        assert!(Project3D::INPUTS[0].required);
        assert_eq!(Project3D::INPUTS[0].ty, PortType::Array(mesh_layout));
        for (i, name) in ["proj_scale", "proj_dist"].iter().enumerate() {
            assert_eq!(Project3D::INPUTS[i + 1].name, *name);
            assert!(!Project3D::INPUTS[i + 1].required);
            assert_eq!(
                Project3D::INPUTS[i + 1].ty,
                PortType::Scalar(crate::node_graph::ports::ScalarType::F32)
            );
        }
        assert_eq!(Project3D::OUTPUTS.len(), 1);
        assert_eq!(Project3D::OUTPUTS[0].ty, PortType::Array(point_layout));
    }

    #[test]
    fn project_3d_has_mode_scale_dist_params() {
        let names: Vec<&str> = Project3D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["mode", "proj_scale", "proj_dist"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Project3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.project_3d");
    }
}
