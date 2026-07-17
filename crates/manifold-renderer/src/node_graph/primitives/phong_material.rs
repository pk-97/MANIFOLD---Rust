//! `node.phong_material` — Lambert diffuse + Blinn-Phong specular material.
//!
//! Emits a [`Material`] of kind [`MaterialKind::Phong`] on the `out` port.
//! Consumers (the bundled 3D mesh renderers) compute
//! `diffuse = max(N·L, 0)` and `specular = pow(max(N·H, 0), specular_power)`
//! per fragment and combine them with the wired [`Light`](crate::node_graph::light::Light).
//!
//! Required wires on the renderer when this material is in use:
//! - `light` — direction + colour (premultiplied with intensity).
//!
//! CPU-only — no GPU dispatch. The cheap baseline material; pick this when
//! you need lit surfaces but don't need full PBR's IBL + microfacet cost.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::material::{AlphaMode, Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const ALPHA_MODES: &[&str] = &["Opaque", "Mask", "Blend"];

crate::primitive! {
    name: PhongMaterial,
    type_id: "node.phong_material",
    purpose: "Lambert diffuse + Blinn-Phong specular material. Cheap baseline for lit 3D surfaces. The bundled 3D mesh renderers compute per-fragment N·L diffuse and (N·H)^power specular, combine with the wired light's colour, and mix the ambient floor. Outputs one Material on `out`. Requires a `light` input wired to the renderer (Phong needs an illumination source; the conditional-requirement table enforces this at preset-load).",
    inputs: {
        color_r: ScalarF32 optional,
        color_g: ScalarF32 optional,
        color_b: ScalarF32 optional,
        color_a: ScalarF32 optional,
        ambient: ScalarF32 optional,
        specular_color_r: ScalarF32 optional,
        specular_color_g: ScalarF32 optional,
        specular_color_b: ScalarF32 optional,
        specular_power: ScalarF32 optional,
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
            default: ParamValue::Float(0.85),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.88),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.92),
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
            name: Cow::Borrowed("ambient"),
            label: "Ambient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_color_r"),
            label: "Specular R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_color_g"),
            label: "Specular G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_color_b"),
            label: "Specular B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_power"),
            label: "Specular Power",
            ty: ParamType::Float,
            default: ParamValue::Float(32.0),
            range: Some((1.0, 256.0)),
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
    depth_rule: Terminal,
    composition_notes: "Wire `out` into a 3D mesh renderer's `material` input AND wire a `node.light` into the renderer's `light` input. `specular_power` is the Phong exponent — 1 is very soft (almost diffuse), 32 is the default highlight, 256 is pinpoint. `ambient` mixes via `lit = lambert * (1 - ambient) + ambient`; bump to 0.3+ for half-lit surfaces or zero for hard-lit shadow contrast. Optional renderer-side `normal_map` texture perturbs the per-fragment normal.",
    examples: [],
    picker: { label: "Phong Material", category: Atom },
    summary: "A basic shiny material with soft diffuse shading and a sharp highlight. The cheap go-to for lit 3D surfaces.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["phong", "glossy", "specular", "material"],
    boundary_reason: NonGpu,
}

impl Primitive for PhongMaterial {
    fn emitted_material_kind(&self) -> Option<MaterialKind> {
        Some(MaterialKind::Phong)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color_r = ctx.scalar_or_param("color_r", 0.85);
        let color_g = ctx.scalar_or_param("color_g", 0.88);
        let color_b = ctx.scalar_or_param("color_b", 0.92);
        let color_a = ctx.scalar_or_param("color_a", 1.0);
        let ambient = ctx.scalar_or_param("ambient", 0.15);
        let spec_r = ctx.scalar_or_param("specular_color_r", 1.0);
        let spec_g = ctx.scalar_or_param("specular_color_g", 1.0);
        let spec_b = ctx.scalar_or_param("specular_color_b", 1.0);
        let spec_power = ctx.scalar_or_param("specular_power", 32.0);
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

        let mut material = Material::phong(
            [color_r, color_g, color_b, color_a],
            ambient,
            [spec_r, spec_g, spec_b],
            spec_power,
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
    fn phong_material_declares_port_shadow_scalars_and_material_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(PhongMaterial::TYPE_ID, "node.phong_material");
        for input in PhongMaterial::INPUTS {
            assert!(
                !input.required,
                "{} should be optional (port-shadow)",
                input.name
            );
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(PhongMaterial::OUTPUTS.len(), 1);
        assert_eq!(PhongMaterial::OUTPUTS[0].name, "out");
        assert_eq!(PhongMaterial::OUTPUTS[0].ty, PortType::Material);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PhongMaterial::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.phong_material");
    }

    #[test]
    fn run_emits_phong_material_with_specular_fields() {
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
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(0.8));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(0.85));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(0.9));
        params.insert(std::borrow::Cow::Borrowed("color_a"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("ambient"), ParamValue::Float(0.2));
        params.insert(std::borrow::Cow::Borrowed("specular_color_r"), ParamValue::Float(0.9));
        params.insert(std::borrow::Cow::Borrowed("specular_color_g"), ParamValue::Float(0.8));
        params.insert(std::borrow::Cow::Borrowed("specular_color_b"), ParamValue::Float(0.7));
        params.insert(std::borrow::Cow::Borrowed("specular_power"), ParamValue::Float(64.0));
        params.insert(std::borrow::Cow::Borrowed("emission_r"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_g"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_b"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_intensity"), ParamValue::Float(0.0));

        let mut prim = PhongMaterial::new();
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
        assert_eq!(mat.kind, MaterialKind::Phong);
        assert_eq!(mat.base_color, [0.8, 0.85, 0.9, 1.0]);
        assert_eq!(mat.ambient, 0.2);
        assert_eq!(mat.specular_color, [0.9, 0.8, 0.7, 1.0]);
        assert_eq!(mat.specular_power, 64.0);
        assert!(mat.requires_light());
        assert!(!mat.requires_envmap());
    }
}
