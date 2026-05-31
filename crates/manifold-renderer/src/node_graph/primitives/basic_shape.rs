//! `node.basic_shape` — curated single-dispatch 2D SDF rasterizer.
//!
//! One instance draws one shape (Square / Diamond / Octagon) into an
//! RGBA16F texture with anti-aliased edges via finite-difference fwidth.
//! Pure render: no clip-trigger state, no cycling, no easing. The shape
//! is picked by a static `shape` enum param — `node.basic_shape`
//! instances live one-per-shape in a preset and feed a `mux_texture` for
//! runtime selection (BasicShapes.json is the canonical consumer).
//!
//! `rotation`, `is_wireframe`, and the geometric params (`aspect`,
//! `scale`, `line`) are port-shadowable so an outer graph can drive
//! them per-frame: cycling math + `node.trigger_ease_to` produce
//! `rotation`; a wireframe-or-solid mux feeds `is_wireframe`.
//!
//! The shape set is closed-by-curation (3 axis-aligned regular polygons
//! at single-dispatch granularity); adding Pentagon / Hexagon / Star is
//! a `case` addition to the WGSL switch + enum-table bump.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const BASIC_SHAPE_SHAPES: &[&str] = &["Square", "Diamond", "Octagon"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BasicShapeUniforms {
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    shape_idx: f32,
    is_wireframe: f32,
    rotation: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: BasicShape,
    type_id: "node.basic_shape",
    purpose: "Single-dispatch 2D SDF shape — Square / Diamond / Octagon — rasterised into an RGBA16F texture with anti-aliased edges. One instance draws one shape; pick which via the static `shape` enum param. `rotation`, `is_wireframe`, and the geometric params (aspect/scale/line) are port-shadows-param so cycling and easing live in the outer graph. Mux multiple `node.basic_shape` instances at the output for runtime shape selection (the BasicShapes.json pattern).",
    inputs: {
        aspect: ScalarF32 optional,
        scale: ScalarF32 optional,
        line: ScalarF32 optional,
        rotation: ScalarF32 optional,
        is_wireframe: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "shape",
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Square
            range: Some((0.0, (BASIC_SHAPE_SHAPES.len() - 1) as f32)),
            enum_values: BASIC_SHAPE_SHAPES,
        },
        ParamDef {
            name: "aspect",
            label: "Aspect Ratio",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.25, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "line",
            label: "Line Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.015),
            range: Some((0.0005, 0.03)),
            enum_values: &[],
        },
        ParamDef {
            name: "rotation",
            label: "Rotation",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: "is_wireframe",
            label: "Wireframe",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Three instances + node.mux_texture is the canonical multi-shape preset (BasicShapes.json). `shape` is static per instance; everything else is port-shadows-param so cycling and rotation easing compose externally. `is_wireframe` reads as a scalar: > 0.5 draws the outline, otherwise solid fill. `line` only affects the wireframe path. `scale` is inverted internally so larger values zoom out (matches legacy BasicShapes behaviour). For mixed solid/wireframe presets, wire `is_wireframe` from a fill-mode mux driven by clip_trigger_count.",
    examples: [],
    picker: { label: "Basic Shape", category: Atom },
    summary: "Draws one of three simple shapes, a square, diamond, or octagon, as a clean anti-aliased fill. Pick the shape, then size and rotate it.",
    category: Generate,
    role: Source,
    aliases: ["basic shape", "square", "diamond", "octagon"],
}

impl Primitive for BasicShape {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let aspect = ctx.scalar_or_param("aspect", 1.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let line = ctx.scalar_or_param("line", 0.015);
        let rotation = ctx.scalar_or_param("rotation", 0.0);
        let is_wireframe = ctx.scalar_or_param("is_wireframe", 0.0);

        let shape_idx = match ctx.params.get("shape") {
            Some(ParamValue::Enum(v)) => (*v).min((BASIC_SHAPE_SHAPES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => {
                (f.round().clamp(0.0, (BASIC_SHAPE_SHAPES.len() - 1) as f32)) as u32
            }
            _ => 0,
        };

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/basic_shape.wgsl"),
                "cs_main",
                "node.basic_shape",
            )
        });

        let uniforms = BasicShapeUniforms {
            aspect_ratio: aspect,
            line_thickness: line,
            uv_scale,
            shape_idx: shape_idx as f32,
            is_wireframe: if is_wireframe > 0.5 { 1.0 } else { 0.0 },
            rotation,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.basic_shape",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn basic_shape_declares_five_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(BasicShape::TYPE_ID, "node.basic_shape");
        let ins = BasicShape::INPUTS;
        assert_eq!(ins.len(), 5);
        for (i, name) in ["aspect", "scale", "line", "rotation", "is_wireframe"]
            .iter()
            .enumerate()
        {
            assert_eq!(ins[i].name, *name);
            assert!(!ins[i].required);
            assert_eq!(ins[i].ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(BasicShape::OUTPUTS.len(), 1);
        assert_eq!(BasicShape::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn basic_shape_three_shape_enum_covers_square_diamond_octagon() {
        assert_eq!(BASIC_SHAPE_SHAPES, &["Square", "Diamond", "Octagon"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BasicShape::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.basic_shape");
    }
}
