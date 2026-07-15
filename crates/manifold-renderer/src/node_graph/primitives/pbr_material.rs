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

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::material::{AlphaMode, Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const ALPHA_MODES: &[&str] = &["Opaque", "Mask", "Blend"];

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
        alpha_cutoff: ScalarF32 optional,
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular + ior.
        specular: ScalarF32 optional,
        specular_tint_r: ScalarF32 optional,
        specular_tint_g: ScalarF32 optional,
        specular_tint_b: ScalarF32 optional,
        ior: ScalarF32 optional,
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_texture_transform,
        // per-map — one folded 2×3 affine
        // `uv' = (m00*u + m01*v + tx, m10*u + m11*v + ty)` per map family
        // (`uv_*` = base color, `nrm_uv_*` = normal, `mr_uv_*` =
        // metallic-roughness, `occ_uv_*` = occlusion, `em_uv_*` =
        // emissive). The glTF importer wires these as literal per-object
        // params; a hand-authored PBR material leaves them at the identity
        // default.
        uv_m00: ScalarF32 optional,
        uv_m01: ScalarF32 optional,
        uv_m10: ScalarF32 optional,
        uv_m11: ScalarF32 optional,
        uv_tx: ScalarF32 optional,
        uv_ty: ScalarF32 optional,
        nrm_uv_m00: ScalarF32 optional,
        nrm_uv_m01: ScalarF32 optional,
        nrm_uv_m10: ScalarF32 optional,
        nrm_uv_m11: ScalarF32 optional,
        nrm_uv_tx: ScalarF32 optional,
        nrm_uv_ty: ScalarF32 optional,
        mr_uv_m00: ScalarF32 optional,
        mr_uv_m01: ScalarF32 optional,
        mr_uv_m10: ScalarF32 optional,
        mr_uv_m11: ScalarF32 optional,
        mr_uv_tx: ScalarF32 optional,
        mr_uv_ty: ScalarF32 optional,
        occ_uv_m00: ScalarF32 optional,
        occ_uv_m01: ScalarF32 optional,
        occ_uv_m10: ScalarF32 optional,
        occ_uv_m11: ScalarF32 optional,
        occ_uv_tx: ScalarF32 optional,
        occ_uv_ty: ScalarF32 optional,
        em_uv_m00: ScalarF32 optional,
        em_uv_m01: ScalarF32 optional,
        em_uv_m10: ScalarF32 optional,
        em_uv_m11: ScalarF32 optional,
        em_uv_tx: ScalarF32 optional,
        em_uv_ty: ScalarF32 optional,
    },
    outputs: {
        out: Material,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color_r"),
            label: "Base Colour R",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Base Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Base Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.82),
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
            default: ParamValue::Float(0.05),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("metallic"),
            label: "Metallic",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("roughness"),
            label: "Roughness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.01, 1.0)),
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
        ParamDef {
            name: Cow::Borrowed("specular"),
            label: "Specular",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_tint_r"),
            label: "Specular Tint R",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_tint_g"),
            label: "Specular Tint G",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("specular_tint_b"),
            label: "Specular Tint B",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("ior"),
            label: "Index of Refraction",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((1.0, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_m00"),
            label: "UV Transform M00",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_m01"),
            label: "UV Transform M01",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_m10"),
            label: "UV Transform M10",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_m11"),
            label: "UV Transform M11",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_tx"),
            label: "UV Transform Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_ty"),
            label: "UV Transform Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_m00"),
            label: "Normal UV M00",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_m01"),
            label: "Normal UV M01",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_m10"),
            label: "Normal UV M10",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_m11"),
            label: "Normal UV M11",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_tx"),
            label: "Normal UV Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("nrm_uv_ty"),
            label: "Normal UV Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_m00"),
            label: "MR UV M00",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_m01"),
            label: "MR UV M01",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_m10"),
            label: "MR UV M10",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_m11"),
            label: "MR UV M11",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_tx"),
            label: "MR UV Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mr_uv_ty"),
            label: "MR UV Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_m00"),
            label: "Occlusion UV M00",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_m01"),
            label: "Occlusion UV M01",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_m10"),
            label: "Occlusion UV M10",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_m11"),
            label: "Occlusion UV M11",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_tx"),
            label: "Occlusion UV Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("occ_uv_ty"),
            label: "Occlusion UV Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_m00"),
            label: "Emissive UV M00",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_m01"),
            label: "Emissive UV M01",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_m10"),
            label: "Emissive UV M10",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_m11"),
            label: "Emissive UV M11",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_tx"),
            label: "Emissive UV Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("em_uv_ty"),
            label: "Emissive UV Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-128.0, 128.0)),
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
    boundary_reason: NonGpu,
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

        let alpha_mode = match ctx.params.get("alpha_mode") {
            Some(ParamValue::Enum(v)) if *v == 1 => AlphaMode::Mask,
            Some(ParamValue::Enum(v)) if *v == 2 => AlphaMode::Blend,
            Some(ParamValue::Float(f)) if f.round() as i32 == 1 => AlphaMode::Mask,
            Some(ParamValue::Float(f)) if f.round() as i32 == 2 => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        let alpha_cutoff = ctx.scalar_or_param("alpha_cutoff", 0.5);

        // GLB_CONFORMANCE_DESIGN.md G-P4/D5.
        let specular = ctx.scalar_or_param("specular", 1.0);
        let specular_tint_r = ctx.scalar_or_param("specular_tint_r", 1.0);
        let specular_tint_g = ctx.scalar_or_param("specular_tint_g", 1.0);
        let specular_tint_b = ctx.scalar_or_param("specular_tint_b", 1.0);
        let ior = ctx.scalar_or_param("ior", 1.5);
        // One folded per-map UV affine per family (G-P4). The closure keeps
        // the 30 reads mechanical; identity defaults are exactly inert.
        let uv_xf = |prefix: &str| -> [f32; 6] {
            [
                ctx.scalar_or_param(&format!("{prefix}m00"), 1.0),
                ctx.scalar_or_param(&format!("{prefix}m01"), 0.0),
                ctx.scalar_or_param(&format!("{prefix}m10"), 0.0),
                ctx.scalar_or_param(&format!("{prefix}m11"), 1.0),
                ctx.scalar_or_param(&format!("{prefix}tx"), 0.0),
                ctx.scalar_or_param(&format!("{prefix}ty"), 0.0),
            ]
        };
        let base_color_uv_transform = uv_xf("uv_");
        let normal_uv_transform = uv_xf("nrm_uv_");
        let mr_uv_transform = uv_xf("mr_uv_");
        let occlusion_uv_transform = uv_xf("occ_uv_");
        let emissive_uv_transform = uv_xf("em_uv_");

        let mut material = Material::pbr(
            [color_r, color_g, color_b, color_a],
            ambient,
            metallic,
            roughness,
            [emission_r, emission_g, emission_b],
            emission_intensity,
        );
        material.alpha_mode = alpha_mode;
        material.alpha_cutoff = alpha_cutoff;
        material.specular_factor = specular;
        material.specular_tint = [specular_tint_r, specular_tint_g, specular_tint_b];
        material.ior = ior;
        material.base_color_uv_transform = base_color_uv_transform;
        material.normal_uv_transform = normal_uv_transform;
        material.mr_uv_transform = mr_uv_transform;
        material.occlusion_uv_transform = occlusion_uv_transform;
        material.emissive_uv_transform = emissive_uv_transform;
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
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(0.85));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(0.85));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(0.85));
        params.insert(std::borrow::Cow::Borrowed("color_a"), ParamValue::Float(1.0));
        params.insert(std::borrow::Cow::Borrowed("ambient"), ParamValue::Float(0.05));
        params.insert(std::borrow::Cow::Borrowed("metallic"), ParamValue::Float(1.0));
        // Roughness zero — the constructor must clamp to >= 0.01.
        params.insert(std::borrow::Cow::Borrowed("roughness"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_r"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_g"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_b"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_intensity"), ParamValue::Float(0.0));

        let mut prim = PbrMaterial::new();
        let inputs_bindings: &[(&'static str, Slot)] = &[];
        let outputs_bindings: &[(&'static str, Slot)] = &[("out", out_slot)];
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let inputs = NodeInputs::new(inputs_bindings, &backend);
        let outputs = NodeOutputs::new(
            outputs_bindings,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
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
