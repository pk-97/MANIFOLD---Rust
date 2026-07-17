//! `node.checkerboard` — alternating black/white squares at a
//! configurable scale. Output is binary {0, 1} broadcast to RGB,
//! A = 1.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CheckerUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    _pad: f32,
}

crate::primitive! {
    name: Checkerboard,
    type_id: "node.checkerboard",
    purpose: "Pure generator. Alternating black/white squares at configurable scale. Output is binary {0, 1} broadcast to RGB (A = 1). Useful as a debug/diagnostic pattern, a mask for node.compose, or a base for grid-aligned effects.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((0.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Scale = squares-per-UV-unit (so scale = 8 → an 8×8 grid). Use as a mask in node.compose to alternate between two upstream sources. Pair with node.lut1d to colorize the {0, 1} values.",
    examples: [],
    picker: { label: "Checkerboard", category: Atom },
    summary: "Lays down an alternating black and white checker grid at any scale. Handy as a test pattern, a mask, or a base for tiled looks.",
    category: Generate,
    role: Source,
    aliases: ["checkerboard", "checker", "grid", "test pattern"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/checkerboard_body.wgsl"),
}

impl Primitive for Checkerboard {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = match ctx.params.get("scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 8.0,
        };
        let offset_x = match ctx.params.get("offset_x") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let offset_y = match ctx.params.get("offset_y") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let w = target.width;
        let h = target.height;
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: SOURCE atom (no input) — the generated kernel binds
            // uniform(0), output(1), matching the set below. checkerboard.wgsl is
            // the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.checkerboard standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.checkerboard",
            )
        });

        let uniforms = CheckerUniforms {
            scale,
            offset_x,
            offset_y,
            _pad: 0.0,
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
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.checkerboard",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn checkerboard_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(Checkerboard::TYPE_ID, "node.checkerboard");
        assert!(Checkerboard::INPUTS.is_empty());
        assert_eq!(Checkerboard::OUTPUTS.len(), 1);
        assert_eq!(Checkerboard::OUTPUTS[0].name, "out");
        assert_eq!(Checkerboard::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn checkerboard_has_expected_params() {
        let names: Vec<&str> = Checkerboard::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["scale", "offset_x", "offset_y"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Checkerboard::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.checkerboard");
    }
}
