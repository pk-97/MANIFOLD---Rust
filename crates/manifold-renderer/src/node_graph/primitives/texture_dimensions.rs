//! `node.texture_size` — Texture→Scalar bridge that emits the
//! input texture's `width`, `height`, and `aspect` (width / height) on
//! three scalar output ports.
//!
//! No GPU dispatch. Dimensions are CPU-readable from the bound
//! `GpuTexture`, so the output is fresh this frame with zero latency
//! (unlike `node.luminance` which carries one frame of readback lag).
//!
//! The atom unblocks effect-side decompositions that need to know the
//! render-target's aspect ratio to do screen-space math correctly
//! (DoF's radial CoC, future vignette-class atomizations, aspect-aware
//! procedurals). Effects don't have a `system.generator_input.aspect`
//! scalar source the way generators do — wire `system.source`'s output
//! into this primitive's `in` and the aspect comes out the other side.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: TextureDimensions,
    type_id: "node.texture_size",
    purpose: "Read the input texture's pixel dimensions. Outputs `width`, `height`, and `aspect` (= width / height) as scalars. No GPU dispatch — values are CPU-accessible from the bound texture, so the read is zero-latency. Use to feed aspect-correction into a downstream effect-graph chain (e.g. wire `aspect` into `distance_to_point.scale_x` to make a radial mask circular on a non-square canvas).",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        width: ScalarF32,
        height: ScalarF32,
        aspect: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Pure metadata read — no shader, no dispatch. Outputs are zero-latency this frame. `aspect = width / height`; for a 1920×1080 canvas the value is 16/9 ≈ 1.778. If the input texture isn't bound yet (e.g. first frame during graph rebuild) the outputs fall back to 1920 / 1080 / 1.778 so downstream divisions don't see zero. Generators should still use `system.generator_input.aspect` directly; this atom is for effects, which lack the boundary-node scalar exposures.",
    examples: [],
    picker: { label: "Texture Size", category: Driver },
    summary: "Reads the width, height, and aspect ratio of an image and hands them back as numbers. Wire the aspect into a mask to keep circles round on a wide canvas.",
    category: MathAndConvert,
    role: Control,
    aliases: ["texture size", "texture dimensions", "dimensions", "resolution", "aspect"],
    boundary_reason: NonGpu,
}

impl Primitive for TextureDimensions {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (w, h) = match ctx.inputs.texture_2d("in") {
            Some(tex) => (tex.width as f32, tex.height as f32),
            None => (1920.0, 1080.0),
        };
        let aspect = if h > 0.0 { w / h } else { 1.0 };
        ctx.outputs.set_scalar("width", ParamValue::Float(w));
        ctx.outputs.set_scalar("height", ParamValue::Float(h));
        ctx.outputs.set_scalar("aspect", ParamValue::Float(aspect));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn texture_dimensions_declares_one_texture_input_and_three_scalar_outputs() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(TextureDimensions::TYPE_ID, "node.texture_size");
        let ins = TextureDimensions::INPUTS;
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].name, "in");
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert!(ins[0].required);

        let outs = TextureDimensions::OUTPUTS;
        assert_eq!(outs.len(), 3);
        let names: Vec<&str> = outs.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["width", "height", "aspect"]);
        for port in outs {
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TextureDimensions::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.texture_size");
    }
}
