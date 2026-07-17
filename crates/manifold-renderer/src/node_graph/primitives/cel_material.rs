//! `node.cel_material` — cel-shaded material (Lambert N·L quantized into bands).
//!
//! Emits a [`Material`] of kind [`MaterialKind::Cel`] on the `out` port.
//! Consumers (the bundled 3D mesh renderers) compute Lambert N·L per
//! fragment and then snap the result into one of `cel_bands` discrete
//! levels between `band_low` (shadow side) and `band_high` (lit side).
//! The result is the stylised look from the legacy DigitalPlants shader.
//!
//! Required wires on the renderer when this material is in use:
//! - `light` — direction + colour for the N·L computation.
//!
//! Optional renderer-side texture: `normal_map`.
//!
//! CPU-only — no GPU dispatch.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::material::{AlphaMode, Material, MaterialKind};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const ALPHA_MODES: &[&str] = &["Opaque", "Mask", "Blend"];

crate::primitive! {
    name: CelMaterial,
    type_id: "node.cel_material",
    purpose: "Cel-shaded material — Lambert N·L quantized into `cel_bands` discrete bands. Stylised look; the DigitalPlants aesthetic. The bundled 3D mesh renderers compute per-fragment N·L with the wired light, snap into one of N bands between `band_low` (shadow side) and `band_high` (lit side), and multiply by base_color. Outputs one Material on `out`. Requires a `light` input wired to the renderer.",
    inputs: {
        color_r: ScalarF32 optional,
        color_g: ScalarF32 optional,
        color_b: ScalarF32 optional,
        color_a: ScalarF32 optional,
        band_low: ScalarF32 optional,
        band_high: ScalarF32 optional,
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
            default: ParamValue::Float(0.36),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_g"),
            label: "Colour G",
            ty: ParamType::Float,
            default: ParamValue::Float(0.56),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Colour B",
            ty: ParamType::Float,
            default: ParamValue::Float(0.24),
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
            name: Cow::Borrowed("cel_bands"),
            label: "Cel Bands",
            ty: ParamType::Int,
            default: ParamValue::Float(4.0),
            range: Some((2.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("band_low"),
            label: "Band Low",
            ty: ParamType::Float,
            default: ParamValue::Float(0.08),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("band_high"),
            label: "Band High",
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
    depth_rule: Terminal,
    composition_notes: "Wire `out` into a 3D mesh renderer's `material` input AND wire a `node.light` into the renderer's `light` input. `cel_bands = 2` collapses to a silhouette/lit-side binary; `cel_bands = 16` approaches smooth shading. `band_low` is the colour-multiplier on the shadow side (typical 0.08 ≈ 8% — matches legacy DigitalPlants), `band_high` is the lit side (1.0 = full brightness). The construction step clamps `cel_bands` into `[2, 16]` to keep downstream shader assumptions stable.",
    examples: [],
    picker: { label: "Cel Material", category: Atom },
    summary: "A toon material that snaps the lighting into a few flat bands for a cartoon or cel-shaded look.",
    category: MaterialsAndLighting,
    role: Source,
    aliases: ["cel", "toon", "cartoon", "material", "Toon BSDF"],
    boundary_reason: NonGpu,
}

impl Primitive for CelMaterial {
    fn emitted_material_kind(&self) -> Option<MaterialKind> {
        Some(MaterialKind::Cel)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color_r = ctx.scalar_or_param("color_r", 0.36);
        let color_g = ctx.scalar_or_param("color_g", 0.56);
        let color_b = ctx.scalar_or_param("color_b", 0.24);
        let color_a = ctx.scalar_or_param("color_a", 1.0);
        let band_low = ctx.scalar_or_param("band_low", 0.08);
        let band_high = ctx.scalar_or_param("band_high", 1.0);
        let emission_r = ctx.scalar_or_param("emission_r", 0.0);
        let emission_g = ctx.scalar_or_param("emission_g", 0.0);
        let emission_b = ctx.scalar_or_param("emission_b", 0.0);
        let emission_intensity = ctx.scalar_or_param("emission_intensity", 0.0);

        let cel_bands = match ctx.params.get("cel_bands") {
            Some(ParamValue::Float(f)) => f.round().clamp(2.0, 16.0) as u32,
            Some(ParamValue::Enum(v)) => (*v).clamp(2, 16),
            _ => 4,
        };

        let alpha_mode = match ctx.params.get("alpha_mode") {
            Some(ParamValue::Enum(v)) if *v == 1 => AlphaMode::Mask,
            Some(ParamValue::Enum(v)) if *v == 2 => AlphaMode::Blend,
            Some(ParamValue::Float(f)) if f.round() as i32 == 1 => AlphaMode::Mask,
            Some(ParamValue::Float(f)) if f.round() as i32 == 2 => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        let alpha_cutoff = ctx.scalar_or_param("alpha_cutoff", 0.5);

        let mut material = Material::cel(
            [color_r, color_g, color_b, color_a],
            cel_bands,
            band_low,
            band_high,
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
    fn cel_material_declares_port_shadow_scalars_and_material_output() {
        use crate::node_graph::ports::{PortType, ScalarType};

        assert_eq!(CelMaterial::TYPE_ID, "node.cel_material");
        for input in CelMaterial::INPUTS {
            assert!(
                !input.required,
                "{} should be optional (port-shadow)",
                input.name
            );
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(CelMaterial::OUTPUTS.len(), 1);
        assert_eq!(CelMaterial::OUTPUTS[0].name, "out");
        assert_eq!(CelMaterial::OUTPUTS[0].ty, PortType::Material);
    }

    #[test]
    fn cel_bands_is_int_param() {
        let bands = CelMaterial::PARAMS
            .iter()
            .find(|p| p.name == "cel_bands")
            .expect("cel_bands param");
        assert_eq!(bands.ty, ParamType::Int);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = CelMaterial::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.cel_material");
    }

    #[test]
    fn run_emits_cel_material_with_clamped_band_count() {
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
        params.insert(std::borrow::Cow::Borrowed("color_r"), ParamValue::Float(0.4));
        params.insert(std::borrow::Cow::Borrowed("color_g"), ParamValue::Float(0.6));
        params.insert(std::borrow::Cow::Borrowed("color_b"), ParamValue::Float(0.3));
        params.insert(std::borrow::Cow::Borrowed("color_a"), ParamValue::Float(1.0));
        // Above the design's [2,16] cap — the constructor must clamp.
        params.insert(std::borrow::Cow::Borrowed("cel_bands"), ParamValue::Float(99.0));
        params.insert(std::borrow::Cow::Borrowed("band_low"), ParamValue::Float(0.1));
        params.insert(std::borrow::Cow::Borrowed("band_high"), ParamValue::Float(0.95));
        params.insert(std::borrow::Cow::Borrowed("emission_r"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_g"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_b"), ParamValue::Float(0.0));
        params.insert(std::borrow::Cow::Borrowed("emission_intensity"), ParamValue::Float(0.0));

        let mut prim = CelMaterial::new();
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
        assert_eq!(mat.kind, MaterialKind::Cel);
        assert_eq!(mat.cel_bands, 16);
        assert_eq!(mat.band_low, 0.1);
        assert_eq!(mat.band_high, 0.95);
        assert!(mat.requires_light());
        assert!(!mat.requires_envmap());
    }
}
