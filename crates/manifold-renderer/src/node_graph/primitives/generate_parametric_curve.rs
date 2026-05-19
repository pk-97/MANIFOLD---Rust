//! `node.generate_parametric_curve` — emit an `Array<LinePoint>`
//! sampled from a parametric curve.
//!
//! Phase C of `BUFFER_PORT_PLAN`. The producer side of the line
//! family — covers Lissajous, hypocycloid, rose, circle as a
//! single primitive with a `curve_type` Enum param. Output is in
//! screen space [0, 1] centered at (0.5, 0.5).

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::LinePoint;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const CURVE_TYPES: &[&str] = &["Lissajous", "Hypocycloid", "Rose", "Circle"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurveUniforms {
    active_count: u32,
    capacity: u32,
    curve_type: u32,
    _pad0: u32,
    freq_x: f32,
    freq_y: f32,
    phase: f32,
    scale: f32,
}

crate::primitive! {
    name: GenerateParametricCurve,
    type_id: "node.generate_parametric_curve",
    purpose: "Sample a parametric curve (Lissajous, hypocycloid, rose, or circle) into an Array<LinePoint>. Output is screen-space [0, 1] centered. Pair with node.render_lines to draw bright vector strokes; or feed the points into any consumer that wants ordered 2D positions (particle seeders, mesh deformers).",
    inputs: {},
    outputs: {
        points: Array(LinePoint),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(4096),
            range: Some((16.0, 65_536.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Int(256),
            range: Some((4.0, 65_536.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "curve_type",
            label: "Curve",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: CURVE_TYPES,
        },
        ParamDef {
            name: "freq_x",
            label: "Frequency X",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.1, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "freq_y",
            label: "Frequency Y",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 20.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "phase",
            label: "Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-6.28318, 6.28318)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "active_count is the number of samples along the curve [0, 2π]. Higher = smoother. freq_x/freq_y interact per curve_type: Lissajous uses both as harmonic ratios; Hypocycloid and Rose only use freq_x as the integer petal count.",
    examples: [],
    picker: { label: "Generate Parametric Curve", category: Atom },
}

impl Primitive for GenerateParametricCurve {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(4) as u32,
            _ => 256,
        };
        let curve_type = match ctx.params.get("curve_type") {
            Some(ParamValue::Enum(n)) => (*n).max(0) as u32,
            _ => 0,
        };
        let freq_x = match ctx.params.get("freq_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 3.0,
        };
        let freq_y = match ctx.params.get("freq_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 2.0,
        };
        let phase = match ctx.params.get("phase") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.8,
        };

        let Some(out_buf) = ctx.outputs.array("points") else {
            return;
        };
        let item_size = std::mem::size_of::<LinePoint>() as u64;
        let capacity = (out_buf.size / item_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_parametric_curve.wgsl"),
                "cs_main",
                "node.generate_parametric_curve",
            )
        });

        let uniforms = CurveUniforms {
            active_count,
            capacity,
            curve_type,
            _pad0: 0,
            freq_x,
            freq_y,
            phase,
            scale,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.generate_parametric_curve",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn generate_parametric_curve_declares_zero_inputs_and_linepoint_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<LinePoint>() as u32,
            item_align: std::mem::align_of::<LinePoint>() as u32,
        };
        assert_eq!(
            GenerateParametricCurve::TYPE_ID,
            "node.generate_parametric_curve"
        );
        assert!(GenerateParametricCurve::INPUTS.is_empty());
        assert_eq!(GenerateParametricCurve::OUTPUTS.len(), 1);
        assert_eq!(GenerateParametricCurve::OUTPUTS[0].name, "points");
        assert_eq!(
            GenerateParametricCurve::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn curve_enum_has_four_options() {
        let p = GenerateParametricCurve::PARAMS
            .iter()
            .find(|p| p.name == "curve_type")
            .unwrap();
        assert_eq!(p.ty, ParamType::Enum);
        assert_eq!(p.enum_values.len(), 4);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateParametricCurve::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_parametric_curve");
    }
}
