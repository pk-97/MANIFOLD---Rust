use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};

use super::{MaterialUpgradeCache, upgrade_material_graph};
use crate::node_graph::gltf_import::synthetic_glbs::write_synthetic_multimaterial_glb;
use crate::node_graph::gltf_import::tests::full_material;
use crate::node_graph::gltf_load;

fn imported_fixture() -> (PathBuf, EffectGraphDef) {
    let path = write_synthetic_multimaterial_glb(1);
    let (graph, _) = super::super::assemble_import_graph(&path).expect("synthetic import");
    (path, graph)
}

fn visit_group_nodes<F: FnMut(&mut manifold_core::effect_graph_def::EffectGraphNode)>(
    nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
    f: &mut F,
) {
    for node in nodes {
        f(node);
        if let Some(group) = node.group.as_mut() {
            visit_group_nodes(&mut group.nodes, f);
        }
    }
}

fn cached_material(path: &Path, material: gltf_load::GltfMaterialInfo) -> MaterialUpgradeCache {
    let mut summary = gltf_load::gltf_import_summary(path).expect("fixture summary");
    summary.materials = vec![material];
    let mut cache = MaterialUpgradeCache::default();
    cache.summaries.insert(path.to_path_buf(), Ok(summary));
    cache
}

fn find_node_mut<'a>(
    nodes: &'a mut [manifold_core::effect_graph_def::EffectGraphNode],
    type_id: &str,
) -> Option<&'a mut manifold_core::effect_graph_def::EffectGraphNode> {
    for node in nodes {
        if node.type_id == type_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_mut()
            && let Some(found) = find_node_mut(&mut group.nodes, type_id)
        {
            return Some(found);
        }
    }
    None
}

fn material_node(
    graph: &mut EffectGraphDef,
) -> &mut manifold_core::effect_graph_def::EffectGraphNode {
    find_node_mut(&mut graph.nodes, "node.pbr_material").expect("pbr material")
}

#[test]
fn ordinary_graphs_are_a_noop_without_import_notices() {
    let mut graph = EffectGraphDef {
        version: 3,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let result = upgrade_material_graph(&mut graph, &mut MaterialUpgradeCache::default());
    assert_eq!(result, Default::default());
}

#[test]
fn stripped_legacy_import_restores_fields_and_is_idempotent() {
    let (path, mut graph) = imported_fixture();
    visit_group_nodes(&mut graph.nodes, &mut |node| {
        if node.type_id == "node.gltf_mesh_source" {
            node.params.remove("vertex_colors");
        }
        if node.type_id == "node.pbr_material" {
            node.params.remove("normal_scale");
            node.params.remove("occlusion_strength");
            node.params.remove("nrm_uv_m00");
            node.params.remove("mr_uv_m00");
            node.params.insert(
                "custom_preserved".to_string(),
                SerializedParamValue::Float { value: 7.0 },
            );
        }
    });
    let first = upgrade_material_graph(&mut graph, &mut MaterialUpgradeCache::default());
    assert!(first.changed, "legacy fields should be restored");
    let mut restored = (false, false, false, false, false);
    visit_group_nodes(&mut graph.nodes, &mut |node| {
        if node.type_id == "node.gltf_mesh_source" {
            restored.0 = matches!(node.params.get("vertex_colors"), Some(SerializedParamValue::Bool { value }) if !value);
        }
        if node.type_id == "node.pbr_material" {
            restored.1 = node.params.contains_key("normal_scale");
            restored.2 = node.params.contains_key("occlusion_strength");
            restored.3 = node.params.contains_key("custom_preserved");
            restored.4 = matches!(node.params.get("color_r"), Some(SerializedParamValue::Float { value }) if (*value - 0.5).abs() < 1e-6);
        }
    });
    assert_eq!(restored, (true, true, true, true, true));
    let before_second = serde_json::to_string(&graph).expect("serialize upgraded graph");
    let second = upgrade_material_graph(&mut graph, &mut MaterialUpgradeCache::default());
    assert!(!second.changed, "second load must be idempotent");
    assert_eq!(
        before_second,
        serde_json::to_string(&graph).expect("serialize again")
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn missing_source_is_retryable_and_does_not_mutate_graph() {
    let (path, mut graph) = imported_fixture();
    visit_group_nodes(&mut graph.nodes, &mut |node| {
        if node.type_id == "node.gltf_mesh_source" {
            node.params.remove("vertex_colors");
        }
    });
    for param in graph
        .preset_metadata
        .as_mut()
        .expect("import metadata")
        .string_params
        .iter_mut()
    {
        if param.id == "model_file" {
            param.default_value = "/missing/legacy-model.glb".to_string();
        }
    }
    let before = serde_json::to_string(&graph).expect("serialize before retry");
    let result = upgrade_material_graph(&mut graph, &mut MaterialUpgradeCache::default());
    assert!(!result.changed);
    assert!(!result.notices.is_empty());
    assert_eq!(
        before,
        serde_json::to_string(&graph).expect("serialize after retry")
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn explicit_vertex_color_flags_complete_without_reparsing_missing_source() {
    for explicit in [false, true] {
        let (path, mut graph) = imported_fixture();
        visit_group_nodes(&mut graph.nodes, &mut |node| {
            if node.type_id == "node.gltf_mesh_source" {
                node.params.insert(
                    "vertex_colors".to_string(),
                    SerializedParamValue::Bool { value: explicit },
                );
            }
        });
        for param in graph
            .preset_metadata
            .as_mut()
            .expect("import metadata")
            .string_params
            .iter_mut()
        {
            if param.id == "model_file" {
                param.default_value = "/missing/explicit-completion.glb".to_string();
            }
        }
        let result = upgrade_material_graph(&mut graph, &mut MaterialUpgradeCache::default());
        assert!(
            !result.changed,
            "explicit vertex_colors={explicit} must mark source complete"
        );
        assert!(
            result.notices.is_empty(),
            "completed source must not reparse or report: {:?}",
            result.notices
        );
        std::fs::remove_file(path).ok();
    }
}

#[test]
fn varying_color_summary_enables_vertex_colors() {
    let (path, mut graph) = imported_fixture();
    visit_group_nodes(&mut graph.nodes, &mut |node| {
        if node.type_id == "node.gltf_mesh_source" {
            node.params.remove("vertex_colors");
        }
    });
    let mut material = full_material(0, "Varying", 3);
    material.vertex_color_varies = true;
    let mut cache = cached_material(&path, material);
    let result = upgrade_material_graph(&mut graph, &mut cache);
    assert!(result.changed);
    let source = find_node_mut(&mut graph.nodes, "node.gltf_mesh_source").expect("mesh source");
    assert_eq!(
        source.params.get("vertex_colors"),
        Some(&SerializedParamValue::Bool { value: true })
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn legacy_spec_gloss_recovers_factors_binding_and_gloss_default_once() {
    let (path, mut graph) = imported_fixture();
    find_node_mut(&mut graph.nodes, "node.gltf_mesh_source")
        .unwrap()
        .params
        .remove("vertex_colors");
    let mat_id = material_node(&mut graph).node_id.clone();
    let mut material = full_material(0, "LegacySG", 3);
    material.legacy_specular_factor = Some(0.2);
    material.specular_color_factor = [0.5, 0.75, 1.0];
    material.mr_texture_is_gloss_alpha = true;
    material.roughness = 0.8;
    let mut cache = cached_material(&path, material);
    let meta = graph.preset_metadata.as_mut().expect("import metadata");
    let mut binding = meta.bindings.iter().find(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == &mat_id)
    }).cloned().expect("material binding fixture");
    binding.id = "legacy_specular".to_string();
    binding.label = "Legacy Specular".to_string();
    binding.default_value = 0.2;
    binding.target = BindingTarget::Node {
        node_id: mat_id.clone(),
        param: "specular".to_string(),
    };
    meta.bindings.push(binding);
    {
        let mat = material_node(&mut graph);
        mat.params.insert(
            "specular".to_string(),
            SerializedParamValue::Float { value: 0.2 },
        );
        for (name, value) in [
            ("specular_tint_r", 1.0),
            ("specular_tint_g", 1.0),
            ("specular_tint_b", 1.0),
        ] {
            mat.params
                .insert(name.to_string(), SerializedParamValue::Float { value });
        }
        mat.params.insert(
            "roughness".to_string(),
            SerializedParamValue::Float { value: 0.8 },
        );
    }
    let result = upgrade_material_graph(&mut graph, &mut cache);
    assert!(result.changed);
    let mat = material_node(&mut graph);
    assert_eq!(
        mat.params.get("specular"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        mat.params.get("specular_tint_r"),
        Some(&SerializedParamValue::Float { value: 0.5 })
    );
    assert_eq!(
        mat.params.get("specular_tint_g"),
        Some(&SerializedParamValue::Float { value: 0.75 })
    );
    assert_eq!(
        mat.params.get("specular_tint_b"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert_eq!(
        mat.params.get("roughness"),
        Some(&SerializedParamValue::Float { value: 1.0 })
    );
    assert!(
        result
            .binding_updates
            .iter()
            .any(|update| update.id == "legacy_specular"
                && update.old_value == 0.2
                && update.new_value == 1.0)
    );
    mat.params.insert(
        "specular".to_string(),
        SerializedParamValue::Float { value: 0.7 },
    );
    mat.params.insert(
        "specular_tint_r".to_string(),
        SerializedParamValue::Float { value: 0.9 },
    );
    let second = upgrade_material_graph(&mut graph, &mut cache);
    assert!(!second.changed);
    assert_eq!(
        material_node(&mut graph).params.get("specular"),
        Some(&SerializedParamValue::Float { value: 0.7 })
    );
    assert_eq!(
        material_node(&mut graph).params.get("specular_tint_r"),
        Some(&SerializedParamValue::Float { value: 0.9 })
    );
    std::fs::remove_file(path).ok();
}

fn collect_group_wires(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    wires: &[manifold_core::effect_graph_def::EffectGraphWire],
    out: &mut Vec<(u32, String, u32, String)>,
) {
    out.extend(wires.iter().map(|wire| {
        (
            wire.from_node,
            wire.from_port.clone(),
            wire.to_node,
            wire.to_port.clone(),
        )
    }));
    for node in nodes {
        if let Some(group) = &node.group {
            collect_group_wires(&group.nodes, &group.wires, out);
        }
    }
}

#[test]
fn missing_transmission_and_legacy_specular_maps_use_original_binding_id() {
    let (path, mut graph) = imported_fixture();
    find_node_mut(&mut graph.nodes, "node.gltf_mesh_source")
        .unwrap()
        .params
        .remove("vertex_colors");
    let source_id = find_node_mut(&mut graph.nodes, "node.gltf_mesh_source")
        .expect("mesh source")
        .node_id
        .clone();
    let meta = graph.preset_metadata.as_mut().expect("import metadata");
    let original = meta
        .string_params
        .iter()
        .find(|param| param.id == "model_file")
        .cloned()
        .expect("model string parameter");
    let source_binding = meta.string_bindings.iter_mut().find(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &source_id && param == "path")
    }).expect("source path binding");
    source_binding.id = "merged_model_1".to_string();
    let mut merged_param = original;
    merged_param.id = "merged_model_1".to_string();
    meta.string_params.push(merged_param);
    let mut material = full_material(0, "MappedLegacy", 3);
    material.diffuse_transmission_texture = Some(5);
    material.diffuse_transmission_color_texture = Some(6);
    material.legacy_specular_factor = Some(0.4);
    material.specular_color_texture = Some(7);
    let mut cache = cached_material(&path, material);
    let result = upgrade_material_graph(&mut graph, &mut cache);
    assert!(result.changed);
    let mut wires = Vec::new();
    collect_group_wires(&graph.nodes, &graph.wires, &mut wires);
    for port in [
        "diffuse_transmission_map",
        "diffuse_transmission_color_map",
        "specular_color_map",
    ] {
        assert!(
            wires.iter().any(|(_, _, _, to_port)| to_port == port),
            "missing upgraded {port} wire"
        );
    }
    let metadata = graph.preset_metadata.as_ref().expect("metadata");
    let mut upgraded_bindings = metadata.string_bindings.iter().filter(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id.as_str().starts_with("material_upgrade_") && param == "path")
    });
    assert!(upgraded_bindings.clone().count() >= 3);
    assert!(upgraded_bindings.all(|binding| binding.id == "merged_model_1"));
    std::fs::remove_file(path).ok();
}

#[test]
fn custom_material_and_manual_transform_are_not_marked_completed() {
    let (path, mut graph) = imported_fixture();
    visit_group_nodes(&mut graph.nodes, &mut |node| {
        if node.type_id == "node.gltf_mesh_source" {
            node.params.remove("vertex_colors");
        }
        if node.type_id == "node.pbr_material" {
            node.type_id = "node.custom_material".to_string();
            node.params.insert(
                "custom_manual".to_string(),
                SerializedParamValue::Float { value: 0.77 },
            );
        }
        if node.type_id == "node.transform_3d" {
            node.params.insert(
                "pos_x".to_string(),
                SerializedParamValue::Float { value: 3.5 },
            );
        }
    });
    let mut cache = cached_material(&path, full_material(0, "Custom", 3));
    let result = upgrade_material_graph(&mut graph, &mut cache);
    assert!(
        !result.notices.is_empty(),
        "custom material must remain retryable"
    );
    assert!(
        !result.changed,
        "unsupported custom topology must not be marked complete"
    );
    assert!(
        !find_node_mut(&mut graph.nodes, "node.gltf_mesh_source")
            .expect("mesh source")
            .params
            .contains_key("vertex_colors")
    );
    let custom = find_node_mut(&mut graph.nodes, "node.custom_material").expect("custom material");
    assert_eq!(
        custom.params.get("custom_manual"),
        Some(&SerializedParamValue::Float { value: 0.77 })
    );
    assert_eq!(
        find_node_mut(&mut graph.nodes, "node.transform_3d")
            .expect("transform")
            .params
            .get("pos_x"),
        Some(&SerializedParamValue::Float { value: 3.5 })
    );
    std::fs::remove_file(path).ok();
}
