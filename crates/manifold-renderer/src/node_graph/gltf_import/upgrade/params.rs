//! Small, side-effect-free helpers used by the legacy material upgrader.

use std::collections::{BTreeMap, HashSet};

use manifold_core::effect_graph_def::{EffectGraphNode, SerializedParamValue};

use crate::node_graph::gltf_load::GltfMaterialInfo;

use super::super::materials::write_material_params;
use super::{MaterialBindingUpdate, MaterialGraphUpgrade};
use manifold_core::effect_graph_def::BindingTarget;

pub(super) fn generated_material_defaults(
    existing: &EffectGraphNode,
    material: &GltfMaterialInfo,
) -> BTreeMap<String, SerializedParamValue> {
    let mut defaults = existing.clone();
    write_material_params(&mut defaults, material);
    defaults.params.insert(
        "color_a".to_string(),
        SerializedParamValue::Float {
            value: material.base_color_factor[3],
        },
    );
    defaults.params.insert(
        "roughness".to_string(),
        SerializedParamValue::Float {
            value: if material.mr_texture_is_gloss_alpha {
                1.0
            } else {
                material.roughness.max(0.01)
            },
        },
    );
    defaults.params.insert(
        "ambient".to_string(),
        SerializedParamValue::Float { value: 0.0 },
    );
    defaults.params.insert(
        "emission_intensity".to_string(),
        SerializedParamValue::Float {
            value: if material.emissive.iter().any(|channel| *channel > 0.0) {
                material.emissive_strength
            } else {
                0.0
            },
        },
    );
    defaults.params.insert(
        "alpha_mode".to_string(),
        SerializedParamValue::Enum {
            value: if material.was_blend || material.transmission_factor > 0.0 {
                2
            } else if material.alpha_mask {
                1
            } else {
                0
            },
        },
    );
    defaults.params.insert(
        "baked_look".to_string(),
        SerializedParamValue::Bool { value: false },
    );
    defaults.params
}

pub(super) fn retain_unlit_params(params: &mut BTreeMap<String, SerializedParamValue>) {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::UnlitMaterial;
    params.retain(|name, _| {
        UnlitMaterial::PARAMS
            .iter()
            .any(|param| param.name == name.as_str())
    });
}

pub(super) fn float_param(node: &EffectGraphNode, name: &str) -> Option<f32> {
    match node.params.get(name) {
        Some(SerializedParamValue::Float { value }) => Some(*value),
        Some(SerializedParamValue::Int { value }) => Some(*value as f32),
        Some(SerializedParamValue::Enum { value }) => Some(*value as f32),
        _ => None,
    }
}

pub(super) fn enum_param(node: &EffectGraphNode, name: &str) -> Option<u32> {
    match node.params.get(name) {
        Some(SerializedParamValue::Enum { value }) => Some(*value),
        Some(SerializedParamValue::Int { value }) if *value >= 0 => Some(*value as u32),
        _ => None,
    }
}

pub(super) fn approximately(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5_f32.max(a.abs().max(b.abs()) * 1e-5)
}

pub(super) fn repair_material_node(
    node: &mut EffectGraphNode,
    material: &GltfMaterialInfo,
    incoming: &HashSet<String>,
    metadata: &mut manifold_core::effect_graph_def::PresetMetadata,
    result: &mut MaterialGraphUpgrade,
) -> bool {
    if node.type_id != "node.pbr_material" && node.type_id != "node.unlit_material" {
        result.notices.push(format!(
            "material upgrade deferred for {}: unsupported material node {}",
            node.node_id, node.type_id
        ));
        return false;
    }
    let mut defaults = generated_material_defaults(node, material);
    if node.type_id == "node.unlit_material" {
        retain_unlit_params(&mut defaults);
        return fill_missing(node, defaults);
    }
    // Compare authored fields before filling new defaults: a missing old tint
    // means neutral, not an authored edit to the newly imported colour.
    let mut changed = false;
    if let Some(old_mean) = material.legacy_specular_factor {
        let old_tint = [
            float_param(node, "specular_tint_r"),
            float_param(node, "specular_tint_g"),
            float_param(node, "specular_tint_b"),
        ];
        let scalar_unchanged = !incoming.contains("specular")
            && float_param(node, "specular").is_some_and(|value| approximately(value, old_mean));
        let tint_unchanged = !incoming.contains("specular_tint_r")
            && !incoming.contains("specular_tint_g")
            && !incoming.contains("specular_tint_b")
            && old_tint
                .iter()
                .all(|value| value.is_none_or(|v| approximately(v, 1.0)));
        if scalar_unchanged && tint_unchanged {
            changed |= replace_float(node, "specular", 1.0, old_mean, metadata, result);
            for (name, value) in [
                ("specular_tint_r", material.specular_color_factor[0]),
                ("specular_tint_g", material.specular_color_factor[1]),
                ("specular_tint_b", material.specular_color_factor[2]),
            ] {
                changed |= replace_float(node, name, value, 1.0, metadata, result);
            }
        } else if scalar_unchanged || tint_unchanged {
            result.notices.push(format!(
                "partial legacy spec-gloss recovery for {}: authored specular edits preserved",
                node.node_id
            ));
        }
    }
    if material.mr_texture_is_gloss_alpha && !incoming.contains("roughness") {
        let old = material.roughness.max(0.01);
        if float_param(node, "roughness").is_some_and(|value| approximately(value, old)) {
            changed |= replace_float(node, "roughness", 1.0, old, metadata, result);
        }
    }
    changed | fill_missing(node, defaults)
}

fn fill_missing(
    node: &mut EffectGraphNode,
    defaults: BTreeMap<String, SerializedParamValue>,
) -> bool {
    let mut changed = false;
    for (name, value) in defaults {
        if let std::collections::btree_map::Entry::Vacant(entry) = node.params.entry(name) {
            entry.insert(value);
            changed = true;
        }
    }
    changed
}
fn replace_float(
    node: &mut EffectGraphNode,
    name: &str,
    new_value: f32,
    old_value: f32,
    metadata: &mut manifold_core::effect_graph_def::PresetMetadata,
    result: &mut MaterialGraphUpgrade,
) -> bool {
    let value = float_param(node, name).unwrap_or(old_value);
    if !approximately(value, old_value) || approximately(value, new_value) {
        return false;
    }
    node.params.insert(
        name.to_string(),
        SerializedParamValue::Float { value: new_value },
    );
    let mut counts = BTreeMap::new();
    for binding in &metadata.bindings {
        *counts.entry(binding.id.clone()).or_insert(0usize) += 1;
    }
    for binding in metadata.bindings.iter_mut() {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        if node_id != &node.node_id
            || param != name
            || binding.user_added
            || counts.get(&binding.id) != Some(&1)
            || !approximately(binding.scale, 1.0)
            || !approximately(binding.offset, 0.0)
            || !approximately(binding.default_value, old_value)
        {
            continue;
        }
        let id = binding.id.clone();
        binding.default_value = new_value;
        for spec in metadata
            .params
            .iter_mut()
            .filter(|spec| spec.id == binding.id)
        {
            if approximately(spec.default_value, old_value) {
                spec.default_value = new_value;
            }
        }
        result.binding_updates.push(MaterialBindingUpdate {
            id,
            old_value,
            new_value,
        });
    }
    true
}
