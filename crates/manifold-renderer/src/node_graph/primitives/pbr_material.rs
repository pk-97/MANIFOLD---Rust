//! `node.pbr_material` — Cook-Torrance microfacet PBR + IBL reflection material.
//!
//! Emits a [`Material`] of kind [`MaterialKind::Pbr`] on the `out` port. The
//! workhorse material for realistic surfaces. Consumers (the bundled 3D mesh
//! renderers) evaluate the D_GGX × G_Smith × F_Schlick microfacet specular
//! BRDF per fragment, mix with the IBL reflection sampled from the wired
//! envmap, and combine with diffuse + emission.
//!
//! Required wires on the renderer when this material is in use:
//! - `light` — direct-light direction + colour.
//! - `envmap` (Texture2D) — equirectangular HDR for IBL reflection. NOT
//!   optional: PBR without IBL is degenerate (matte-grey for non-metals,
//!   pure direct-light for metals).
//!
//! Optional renderer-side textures: `normal_map`, `base_color_map`,
//! `roughness_map`, `metallic_map`.
//!
//! CPU-only — no GPU dispatch.

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::material::{Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: PbrMaterial,
    type_id: "node.pbr_material",
    purpose: "Cook-Torrance microfacet PBR (D_GGX × G_Smith × F_Schlick) + IBL reflection material. The workhorse for realistic 3D surfaces. The bundled 3D mesh renderers evaluate the BRDF per fragment, blend with envmap-sampled IBL, and combine with diffuse + emission. `metallic` blends F0 from dielectric (≈4%) to metal (= base_color); `roughness` controls the microfacet spread (sharp 0.01 to fully rough 1.0). Outputs one Material on `out`. Requires BOTH a `light` input AND an `envmap` Texture2D wired to the renderer (the conditional-requirement table enforces this at preset-load).",
    inputs: {
        color_r: ScalarF32 optional,
        color_g: ScalarF32 optional,
        color_b: ScalarF32 optional,
        color_a: ScalarF32 optional,
        ambient: ScalarF32 optional,
        metallic: ScalarF32 optional,
        roughness: ScalarF32 optional,
        emission_r: ScalarF32 optional,
        emission_g: ScalarF32 optional,
        emission_b: ScalarF32 optional,
        emission_intensity: ScalarF32 optional,
    },
    outputs: {
        out: Material,
    },
    params: [
        ParamDef {
            name: "color_r",
            label: "Base Colour R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_g",
            label: "Base Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_b",
            label: "Base Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.82),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "color_a",
            label: "Opacity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ambient",
            label: "Ambient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.05),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "metallic",
            label: "Metallic",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "roughness",
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.01, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "emission_r",
            label: "Emission R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "emission_g",
            label: "Emission G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "emission_b",
            label: "Emission B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "emission_intensity",
            label: "Emission Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `out` into a 3D mesh renderer's `material` input. The renderer ALSO requires a wired `light` AND an `envmap` Texture2D (typically `node.bake_environment`). `metallic = 0` = dielectric (plastic, wood, fabric), `metallic = 1` = pure metal (chrome, gold). `roughness` is clamped to a 0.01 floor at construction (zero is a numerical landmine in GGX). Optional textures: `normal_map`, `base_color_map`, `roughness_map`, `metallic_map`. The PBR shader writes in linear space; the renderer's tone-map runs internally so no downstream `node.reinhard_tone_map` is needed.",
    examples: [],
    picker: { label: "PBR Material", category: Atom },
    summary: "A physically based material with roughness, metalness, and environment reflections. The realistic workhorse for 3D surfaces.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["pbr", "realistic", "metallic roughness", "material", "Principled BSDF"],
}

impl Primitive for PbrMaterial {
    fn emitted_material_kind(&self) -> Option<MaterialKind> {
        Some(MaterialKind::Pbr)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color_r = ctx.scalar_or_param("color_r", 0.8);
        let color_g = ctx.scalar_or_param("color_g", 0.8);
        let color_b = ctx.scalar_or_param("color_b", 0.82);
        let color_a = ctx.scalar_or_param("color_a", 1.0);
        let ambient = ctx.scalar_or_param("ambient", 0.05);
        let metallic = ctx.scalar_or_param("metallic", 0.0).clamp(0.0, 1.0);
        let roughness = ctx.scalar_or_param("roughness", 0.5);
        let emission_r = ctx.scalar_or_param("emission_r", 0.0);
        let emission_g = ctx.scalar_or_param("emission_g", 0.0);
        let emission_b = ctx.scalar_or_param("emission_b", 0.0);
        let emission_intensity = ctx.scalar_or_param("emission_intensity", 0.0);

        let material = Material::pbr(
            [color_r, color_g, color_b, color_a],
            ambient,
            metallic,
            roughness,
            [emission_r, emission_g, emission_b],
            emission_intensity,
        );
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
    fn pbr_material_declares_port_shadow_scalars_and_material_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(PbrMaterial::TYPE_ID, "node.pbr_material");
        for input in PbrMaterial::INPUTS {
            assert!(
                !input.required,
                "{} should be optional (port-shadow)",
                input.name
            );
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(PbrMaterial::OUTPUTS.len(), 1);
        assert_eq!(PbrMaterial::OUTPUTS[0].name, "out");
        assert_eq!(PbrMaterial::OUTPUTS[0].ty, PortType::Material);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PbrMaterial::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.pbr_material");
    }

    #[test]
    fn run_emits_pbr_material_and_clamps_roughness_floor() {
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
        params.insert("color_r", ParamValue::Float(0.85));
        params.insert("color_g", ParamValue::Float(0.85));
        params.insert("color_b", ParamValue::Float(0.85));
        params.insert("color_a", ParamValue::Float(1.0));
        params.insert("ambient", ParamValue::Float(0.05));
        params.insert("metallic", ParamValue::Float(1.0));
        // Roughness zero — the constructor must clamp to >= 0.01.
        params.insert("roughness", ParamValue::Float(0.0));
        params.insert("emission_r", ParamValue::Float(0.0));
        params.insert("emission_g", ParamValue::Float(0.0));
        params.insert("emission_b", ParamValue::Float(0.0));
        params.insert("emission_intensity", ParamValue::Float(0.0));

        let mut prim = PbrMaterial::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
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
        assert_eq!(mat.kind, MaterialKind::Pbr);
        assert_eq!(mat.metallic, 1.0);
        assert!(mat.roughness >= 0.01, "roughness must clamp to floor");
        assert!(mat.requires_light());
        assert!(mat.requires_envmap());
    }
}
