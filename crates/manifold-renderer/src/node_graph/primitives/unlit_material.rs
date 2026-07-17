//! `node.unlit_material` — flat-colour material with no lighting math.
//!
//! Emits a [`Material`] of kind [`MaterialKind::Unlit`] on the `out` port.
//! Consumers (the bundled 3D mesh renderers) skip every lighting calculation
//! and write `base_color + emission` directly — useful for UI overlays,
//! debug colours, neon, anything that shouldn't react to lights or shadows.
//!
//! The 3D mesh renderers DO NOT require a `light` input when an Unlit
//! material is wired; the conditional-requirement table lets the validator
//! treat the light input as truly optional for this kind.
//!
//! CPU-only — no GPU dispatch. Industry-standard Blender / TouchDesigner
//! shape (Blender's "Background" / TD's "Constant" MAT).

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::material::{AlphaMode, Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const ALPHA_MODES: &[&str] = &["Opaque", "Mask", "Blend"];

crate::primitive! {
    name: UnlitMaterial,
    type_id: "node.unlit_material",
    purpose: "Flat-colour material — no lighting math, no shadow term. The renderer writes (base_color + emission) directly. Use for UI overlays, debug visualisation, neon, anything that shouldn't react to lights. The bundled 3D mesh renderers do NOT require a `light` input when this material is wired (the conditional-requirement table lets the light input stay truly optional). Outputs one Material on `out` consumed by render_3d_mesh / render_instanced_3d_mesh. Emission is premultiplied with `emission_intensity` at emission — downstream reads the final emissive directly.",
    inputs: {
        color_r: ScalarF32 optional,
        color_g: ScalarF32 optional,
        color_b: ScalarF32 optional,
        color_a: ScalarF32 optional,
        emission_r: ScalarF32 optional,
        emission_g: ScalarF32 optional,
        emission_b: ScalarF32 optional,
        emission_intensity: ScalarF32 optional,
        alpha_cutoff: ScalarF32 optional,
    },
    outputs: {
        out: Material,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color_r"),
            label: "Colour R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_a"),
            label: "Opacity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emission_r"),
            label: "Emission R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emission_g"),
            label: "Emission G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emission_b"),
            label: "Emission B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("emission_intensity"),
            label: "Emission Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("alpha_mode"),
            label: "Alpha Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Opaque
            range: Some((0.0, (ALPHA_MODES.len() - 1) as f32)),
            enum_values: ALPHA_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("alpha_cutoff"),
            label: "Alpha Cutoff",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `out` into a 3D mesh renderer's `material` input. The renderer's `light` input stays unwired when this material is in use (Unlit is the only kind where light is truly optional). `color_a < 1.0` is informational — opaque-only rendering for v1; transparency lands as a follow-up. To get the legacy 'flat lit' look from the pre-Material-system render_3d_mesh, use `node.phong_material` with `ambient = 1.0` instead.",
    examples: [],
    picker: { label: "Unlit Material", category: Atom },
    summary: "A flat-colour material with no lighting, so the surface shows its base colour straight. The simplest material, good for solid or glowing looks.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["unlit", "flat", "constant", "material", "Emission"],
    boundary_reason: NonGpu,
}

impl Primitive for UnlitMaterial {
    fn emitted_material_kind(&self) -> Option<MaterialKind> {
        Some(MaterialKind::Unlit)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color_r = ctx.scalar_or_param("color_r", 1.0);
        let color_g = ctx.scalar_or_param("color_g", 1.0);
        let color_b = ctx.scalar_or_param("color_b", 1.0);
        let color_a = ctx.scalar_or_param("color_a", 1.0);
        let emission_r = ctx.scalar_or_param("emission_r", 0.0);
        let emission_g = ctx.scalar_or_param("emission_g", 0.0);
        let emission_b = ctx.scalar_or_param("emission_b", 0.0);
        let emission_intensity = ctx.scalar_or_param("emission_intensity", 0.0);

        let alpha_mode = match ctx.params.get("alpha_mode") {
            Some(ParamValue::Enum(v)) if *v == 1 => AlphaMode::Mask,
            Some(ParamValue::Enum(v)) if *v == 2 => AlphaMode::Blend,
            Some(ParamValue::Float(f)) if f.round() as i32 == 1 => AlphaMode::Mask,
            Some(ParamValue::Float(f)) if f.round() as i32 == 2 => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        let alpha_cutoff = ctx.scalar_or_param("alpha_cutoff", 0.5);

        let mut material = Material::unlit(
            [color_r, color_g, color_b, color_a],
            [emission_r, emission_g, emission_b],
            emission_intensity,
        );
        material.alpha_mode = alpha_mode;
        material.alpha_cutoff = alpha_cutoff;
        ctx.outputs.set_material("out", material);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::material::MaterialKind;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn unlit_material_declares_port_shadow_scalars_and_material_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(UnlitMaterial::TYPE_ID, "node.unlit_material");
        for input in UnlitMaterial::INPUTS {
            assert!(
                !input.required,
                "{} should be optional (port-shadow)",
                input.name
            );
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(UnlitMaterial::OUTPUTS.len(), 1);
        assert_eq!(UnlitMaterial::OUTPUTS[0].name, "out");
        assert_eq!(UnlitMaterial::OUTPUTS[0].ty, PortType::Material);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = UnlitMaterial::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.unlit_material");
    }

    #[test]
    fn run_emits_unlit_material_with_premultiplied_emission() {
        use crate::node_graph::MockBackend;
        use crate::node_graph::backend::Backend;
        use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::execution_plan::ResourceId;
        use crate::node_graph::ports::PortType;
        use manifold_core::{Beats, Seconds};

        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Material, None, (0, 0));
        let mut params = ParamValues::default();
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(0.5));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(0.6));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(0.7));
        params.insert(std::borrow::Cow::Borrowed("color_a"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("emission_r"), ParamValue::Float(0.5));
        params.insert(std::borrow::Cow::Borrowed("emission_g"), ParamValue::Float(0.4));
        params.insert(std::borrow::Cow::Borrowed("emission_b"), ParamValue::Float(0.3));
        params.insert(std::borrow::Cow::Borrowed("emission_intensity"), ParamValue::Float(2.0));

        let mut prim = UnlitMaterial::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend, &[]);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let time = crate::node_graph::effect_node::FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in material_scratch.drain(..) {
            backend.set_material(slot, value);
        }

        let mat = backend.material(out_slot).expect("material should be set");
        assert_eq!(mat.kind, MaterialKind::Unlit);
        assert_eq!(mat.base_color, [0.5, 0.6, 0.7, 1.0]);
        // Emission premultiplied with intensity=2.0.
        assert!((mat.emission[0] - 1.0).abs() < 1e-5);
        assert!((mat.emission[1] - 0.8).abs() < 1e-5);
        assert!((mat.emission[2] - 0.6).abs() < 1e-5);
        assert!(!mat.requires_light());
    }
}
