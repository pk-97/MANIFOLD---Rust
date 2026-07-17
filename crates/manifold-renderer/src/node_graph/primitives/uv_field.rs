//! `node.uv_field` — write per-pixel UV coordinates as R/G channels.
//!
//! R = u, G = v, B = 0, A = 1.
//!
//! Foundation primitive of the procedural texture math family.
//! Compose with `node.distance_to_point`, `node.polar_field`,
//! `node.sin_texture`, etc. to author novel procedural textures
//! without writing WGSL.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: UvField,
    type_id: "node.uv_field",
    purpose: "Pure generator. Writes per-pixel UV coordinates as a texture: R = u (0..1 left-to-right), G = v (0..1 top-to-bottom), B = 0, A = 1. Foundation primitive of the procedural texture math family — compose with math/noise/distance/polar primitives to author novel procedural textures.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [],
    // depth_rule: zero-input UV-coordinate producer, same reasoning as centered_uv/grid_uv_field
    depth_rule: Terminal,
    composition_notes: "Pairs with node.sin_texture / cos_texture / fract_texture / scale_offset_texture for per-axis sinusoids and stripes. Pairs with node.distance_to_point for radial fields. Output texel center sampling is (i+0.5)/dims (standard convention).",
    examples: [],
    picker: { label: "UV Field", category: Atom },
    summary: "Outputs the position of each pixel as a coordinate, red for left-to-right and green for top-to-bottom. The starting grid for most warps and patterns.",
    category: FieldsAndCoordinates,
    role: Source,
    aliases: ["uv", "coordinates", "position", "uv map"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/uv_field_body.wgsl"),
}

impl Primitive for UvField {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
            // Single-source: paramless SOURCE atom — the generated kernel binds
            // only its output at binding 0 (no uniform, no input, no sampler),
            // matching the binding below. uv_field.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.uv_field standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.uv_field",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[GpuBinding::Texture {
                binding: 0,
                texture: target,
            }],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.uv_field",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn uv_field_declares_zero_inputs_and_one_texture_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(UvField::TYPE_ID, "node.uv_field");
        assert!(UvField::INPUTS.is_empty());
        assert_eq!(UvField::OUTPUTS.len(), 1);
        assert_eq!(UvField::OUTPUTS[0].name, "out");
        assert_eq!(UvField::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn uv_field_has_no_params() {
        assert!(UvField::PARAMS.is_empty());
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = UvField::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.uv_field");
    }
}
