//! Recover newly supported maps through the importer's existing map catalog.
use super::super::materials::{ObjectAssembly, wire_map_families};
use super::params;
use crate::node_graph::gltf_load::GltfMaterialInfo;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
    StringBindingDef,
};
use std::collections::HashMap;

pub(super) fn repair_gloss_source(
    nodes: &mut [EffectGraphNode],
    wires: &[EffectGraphWire],
    scene_id: u32,
    material: &GltfMaterialInfo,
) -> bool {
    if !material.mr_texture_is_gloss_alpha {
        return false;
    }
    let Some(source_id) = super::unique_wire_source(wires, scene_id, "mr_map") else {
        return false;
    };
    let Some(node) = nodes.iter_mut().find(|node| node.id == source_id) else {
        return false;
    };
    if node.type_id != "node.gltf_texture_source"
        || params::enum_param(node, "mode") != Some(1)
        || node.params.contains_key("glossiness_factor")
    {
        return false;
    }
    node.params.insert(
        "glossiness_factor".into(),
        SerializedParamValue::Float {
            value: (1.0 - material.roughness).clamp(0.0, 1.0),
        },
    );
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) fn add_missing_maps(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    metadata: &mut PresetMetadata,
    scene_id: u32,
    source_binding: &StringBindingDef,
    material: &GltfMaterialInfo,
    texture_dims: &[(u32, u32)],
    next_id: &mut u32,
) -> bool {
    let mut generated_nodes = Vec::new();
    let mut generated_wires = Vec::new();
    let mut generated_bindings = Vec::new();
    let mut texture_cache = HashMap::new();
    let mut temp_id = 0;
    let mut fresh_id = || {
        let id = temp_id;
        temp_id += 1;
        id
    };
    wire_map_families(
        material,
        &mut ObjectAssembly {
            k: scene_id as usize,
            path_str: &source_binding.default_value,
            fresh_id: &mut fresh_id,
            group_nodes: &mut generated_nodes,
            group_wires: &mut generated_wires,
            string_bindings: &mut generated_bindings,
            tex_cache: &mut texture_cache,
            scene_object_id: scene_id,
            texture_dims,
        },
    );
    generated_wires.retain(|wire| {
        (matches!(
            wire.to_port.as_str(),
            "diffuse_transmission_map" | "diffuse_transmission_color_map"
        ) || (wire.to_port == "specular_color_map" && material.legacy_specular_factor.is_some()))
            && !wires
                .iter()
                .any(|existing| existing.to_node == scene_id && existing.to_port == wire.to_port)
    });
    if generated_wires.is_empty() {
        return false;
    }
    for mut node in generated_nodes {
        if !generated_wires.iter().any(|wire| wire.from_node == node.id) {
            continue;
        }
        let old_id = node.id;
        node.id = *next_id;
        *next_id = next_id
            .checked_add(1)
            .expect("material upgrade node id capacity");
        node.node_id = NodeId::new(format!(
            "material_upgrade_{}",
            manifold_core::math::short_id()
        ));
        node.handle = Some(node.node_id.as_str().to_string());
        let mut binding = source_binding.clone();
        binding.target = BindingTarget::Node {
            node_id: node.node_id.clone(),
            param: "path".into(),
        };
        metadata.string_bindings.push(binding);
        for wire in generated_wires
            .iter()
            .filter(|wire| wire.from_node == old_id)
        {
            let mut wire = wire.clone();
            wire.from_node = node.id;
            wires.push(wire);
        }
        nodes.push(node);
    }
    true
}
