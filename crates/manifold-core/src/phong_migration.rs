//! One-shot migration from the retired Phong material node to PBR.
//!
//! This is deliberately a data migration.  The renderer no longer needs a
//! Phong primitive after a document has been opened once, and graph bindings
//! continue to address the same outer controls while their target is moved to
//! the small ordinary-math conversion chain used for live shininess controls.

use std::collections::{BTreeMap, BTreeSet};

use ahash::AHashSet;

use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use crate::id::NodeId;
use crate::math::short_id;

const PHONG_TYPE_ID: &str = "node.phong_material";
const PBR_TYPE_ID: &str = "node.pbr_material";

/// Convert all legacy Phong material nodes in `def` to PBR.
///
/// Returns `true` when the definition changed.  The structure check makes the
/// operation idempotent, including for nested groups and scene-modifier graph
/// definitions.  Existing node ids, wires, texture connections, binding ids,
/// and binding ranges are retained; only the retired material parameter names
/// and the target of a live shininess binding are changed.
pub fn migrate_phong_to_pbr(def: &mut EffectGraphDef) -> bool {
    let mut next_id = max_node_id_recursive(&def.nodes).saturating_add(1);
    let mut replacements = Vec::new();
    let mut migrated_nodes = Vec::new();
    let bound_targets = binding_targets(def);
    let mut changed = false;

    migrate_scope(
        &mut def.nodes,
        &mut def.wires,
        &bound_targets,
        &AHashSet::new(),
        &mut replacements,
        &mut migrated_nodes,
        &mut next_id,
        &mut changed,
    );
    ensure_neutral_environments(
        &mut def.nodes,
        &mut def.wires,
        &migrated_nodes,
        &mut next_id,
        &mut changed,
    );
    rewrite_bindings(def, &replacements, &migrated_nodes, &mut changed);

    // Scene modifier recipes carry their own executable graph definitions.
    // They are independent documents, so each one gets its own metadata pass
    // while sharing the outer id allocator only for the lifetime of this call.
    for modifier in &mut def.scene_modifiers {
        if migrate_phong_to_pbr(&mut modifier.graph) {
            changed = true;
        }
    }

    changed
}

/// Fast structural gate for callers that can avoid cloning the common
/// already-migrated document. This includes nested groups and modifier graphs.
pub fn contains_phong_materials(def: &EffectGraphDef) -> bool {
    fn nodes_contain(nodes: &[EffectGraphNode]) -> bool {
        nodes.iter().any(|node| {
            node.type_id == PHONG_TYPE_ID
                || node
                    .group
                    .as_deref()
                    .is_some_and(|group| nodes_contain(&group.nodes))
        })
    }

    nodes_contain(&def.nodes)
        || def
            .scene_modifiers
            .iter()
            .any(|modifier| contains_phong_materials(&modifier.graph))
}

#[derive(Clone)]
struct PowerReplacement {
    old_node_id: NodeId,
    old_handle: Option<String>,
    math_node_id: NodeId,
    math_handle: Option<String>,
}

#[derive(Clone)]
struct MigratedNode {
    old_node_id: NodeId,
    old_handle: Option<String>,
}

fn ensure_neutral_environments(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    migrated_nodes: &[MigratedNode],
    next_id: &mut u32,
    changed: &mut bool,
) {
    for node in nodes.iter_mut() {
        if let Some(group) = node.group.as_deref_mut() {
            ensure_neutral_environments(
                &mut group.nodes,
                &mut group.wires,
                migrated_nodes,
                next_id,
                changed,
            );
        }
    }

    let consumers: Vec<u32> = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.type_id.as_str(),
                "node.render_mesh" | "node.render_copies" | "node.render_scene"
            )
        })
        .filter(|node| {
            !wires
                .iter()
                .any(|wire| wire.to_node == node.id && wire.to_port == "envmap")
        })
        .filter(|node| {
            let material_source = if node.type_id == "node.render_scene" {
                wires.iter().find(|wire| {
                    wire.to_node == node.id
                        && (wire.to_port.starts_with("material_")
                            || wire.to_port.starts_with("object_"))
                        && source_is_migrated(
                            nodes,
                            wires,
                            wire.from_node,
                            &wire.from_port,
                            migrated_nodes,
                        )
                })
            } else {
                wires.iter().find(|wire| {
                    wire.to_node == node.id
                        && wire.to_port == "material"
                        && source_is_migrated(
                            nodes,
                            wires,
                            wire.from_node,
                            &wire.from_port,
                            migrated_nodes,
                        )
                })
            };
            material_source.is_some()
        })
        .map(|node| node.id)
        .collect();

    for consumer_id in consumers {
        let bake_id = *next_id;
        *next_id = next_id.saturating_add(1);
        let consumer = nodes.iter().find(|node| node.id == consumer_id);
        let consumer_id_name = consumer
            .map(|node| node.node_id.clone())
            .unwrap_or_default();
        let consumer_handle = consumer.and_then(|node| node.handle.as_deref());
        let suffix = if consumer_id_name.is_empty() {
            consumer_id.to_string()
        } else {
            consumer_id_name.to_string()
        };
        let bake_node_id = NodeId::new(format!("phong-{suffix}-environment"));
        let bake_handle = consumer_handle.map(|handle| format!("{handle}_phong_environment"));
        let mut params = BTreeMap::new();
        params.insert(
            "intensity".to_string(),
            SerializedParamValue::Float { value: 0.0 },
        );
        nodes.push(EffectGraphNode {
            id: bake_id,
            node_id: bake_node_id,
            type_id: "node.bake_environment".to_string(),
            handle: bake_handle,
            params,
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        wires.push(EffectGraphWire {
            from_node: bake_id,
            from_port: "envmap".to_string(),
            to_node: consumer_id,
            to_port: "envmap".to_string(),
        });
        *changed = true;
    }
}

fn source_is_migrated(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    source_id: u32,
    source_port: &str,
    migrated_nodes: &[MigratedNode],
) -> bool {
    let mut visited = AHashSet::new();
    source_is_migrated_inner(
        nodes,
        wires,
        source_id,
        source_port,
        migrated_nodes,
        &mut visited,
    )
}

fn source_is_migrated_inner(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    source_id: u32,
    source_port: &str,
    migrated_nodes: &[MigratedNode],
    visited: &mut AHashSet<(usize, u32, String)>,
) -> bool {
    let key = (nodes.as_ptr() as usize, source_id, source_port.to_string());
    if !visited.insert(key) {
        return false;
    }
    let Some(source) = nodes.iter().find(|node| node.id == source_id) else {
        return false;
    };
    if migrated_nodes.iter().any(|node| {
        (!node.old_node_id.is_empty() && source.node_id == node.old_node_id)
            || (node.old_node_id.is_empty()
                && node
                    .old_handle
                    .as_deref()
                    .is_some_and(|handle| source.handle.as_deref() == Some(handle)))
    }) {
        return true;
    }
    if source.type_id == "node.scene_object" {
        return wires
            .iter()
            .find(|wire| wire.to_node == source.id && wire.to_port == "material")
            .is_some_and(|wire| {
                source_is_migrated_inner(
                    nodes,
                    wires,
                    wire.from_node,
                    &wire.from_port,
                    migrated_nodes,
                    visited,
                )
            });
    }
    let Some(group) = source.group.as_deref() else {
        return false;
    };
    let Some(output) = group
        .nodes
        .iter()
        .find(|node| node.type_id == "system.group_output")
    else {
        return false;
    };
    let Some(wire) = group
        .wires
        .iter()
        .find(|wire| wire.to_node == output.id && wire.to_port == source_port)
    else {
        return false;
    };
    source_is_migrated_inner(
        &group.nodes,
        &group.wires,
        wire.from_node,
        &wire.from_port,
        migrated_nodes,
        visited,
    )
}

fn binding_targets(def: &EffectGraphDef) -> AHashSet<NodeId> {
    def.preset_metadata
        .as_ref()
        .into_iter()
        .flat_map(|metadata| metadata.bindings.iter())
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param } if param == "specular_power" => {
                Some(node_id.clone())
            }
            _ => None,
        })
        .collect()
}

fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| {
            let inner = node
                .group
                .as_deref()
                .map(|group| max_node_id_recursive(&group.nodes))
                .unwrap_or(0);
            node.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

fn migrate_scope(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    bound_targets: &AHashSet<NodeId>,
    bound_handles: &AHashSet<String>,
    replacements: &mut Vec<PowerReplacement>,
    migrated_nodes: &mut Vec<MigratedNode>,
    next_id: &mut u32,
    changed: &mut bool,
) {
    let original_len = nodes.len();
    for index in 0..original_len {
        if let Some(group) = nodes[index].group.as_deref_mut() {
            let group_bound_handles = group
                .interface
                .params
                .iter()
                .filter(|param| param.target_param == "specular_power")
                .map(|param| param.target_handle.clone())
                .collect();
            migrate_scope(
                &mut group.nodes,
                &mut group.wires,
                bound_targets,
                &group_bound_handles,
                replacements,
                migrated_nodes,
                next_id,
                changed,
            );
            rewrite_group_params(group, replacements, migrated_nodes, changed);
        }

        if nodes[index].type_id != PHONG_TYPE_ID {
            continue;
        }

        let node_id = nodes[index].id;
        let handle = nodes[index].handle.clone();
        let stable_id =
            if nodes[index].node_id.is_empty() && handle.as_deref().is_none_or(str::is_empty) {
                let minted = derived_node_id(&NodeId::default(), node_id, "material");
                nodes[index].node_id = minted.clone();
                minted
            } else {
                nodes[index].node_id.clone()
            };
        let power = nodes[index]
            .params
            .get("specular_power")
            .and_then(serialized_float)
            .unwrap_or(32.0);
        let wired = wires
            .iter()
            .any(|wire| wire.to_node == node_id && wire.to_port == "specular_power");
        let bound = wired
            || (!stable_id.is_empty() && bound_targets.contains(&stable_id))
            || (stable_id.is_empty()
                && handle
                    .as_deref()
                    .is_some_and(|handle| bound_targets.contains(&NodeId::new(handle))))
            || handle
                .as_deref()
                .is_some_and(|handle| bound_handles.contains(handle));

        migrated_nodes.push(MigratedNode {
            old_node_id: stable_id.clone(),
            old_handle: handle.clone(),
        });

        let had_power_exposure = nodes[index].exposed_params.remove("specular_power");
        nodes[index].type_id = PBR_TYPE_ID.to_string();
        rename_param(
            &mut nodes[index].params,
            "specular_color_r",
            "specular_tint_r",
        );
        rename_param(
            &mut nodes[index].params,
            "specular_color_g",
            "specular_tint_g",
        );
        rename_param(
            &mut nodes[index].params,
            "specular_color_b",
            "specular_tint_b",
        );
        rename_exposed_param(
            &mut nodes[index].exposed_params,
            "specular_color_r",
            "specular_tint_r",
        );
        rename_exposed_param(
            &mut nodes[index].exposed_params,
            "specular_color_g",
            "specular_tint_g",
        );
        rename_exposed_param(
            &mut nodes[index].exposed_params,
            "specular_color_b",
            "specular_tint_b",
        );
        for wire in wires.iter_mut() {
            if wire.to_node == node_id {
                rename_port(&mut wire.to_port);
            }
        }

        if bound {
            let max_id = *next_id;
            *next_id = next_id.saturating_add(1);
            let add_id = *next_id;
            *next_id = next_id.saturating_add(1);
            let divide_id = *next_id;
            *next_id = next_id.saturating_add(1);
            let sqrt_id = *next_id;
            *next_id = next_id.saturating_add(1);

            let max_node_id = derived_node_id(&stable_id, node_id, "phong_max");
            let add_node_id = derived_node_id(&stable_id, node_id, "phong_add");
            let divide_node_id = derived_node_id(&stable_id, node_id, "phong_divide");
            let sqrt_node_id = derived_node_id(&stable_id, node_id, "phong_sqrt");

            for wire in wires.iter_mut() {
                if wire.to_node == node_id && wire.to_port == "specular_power" {
                    wire.to_node = max_id;
                    wire.to_port = "a".to_string();
                }
            }
            wires.push(EffectGraphWire {
                from_node: max_id,
                from_port: "out".to_string(),
                to_node: add_id,
                to_port: "a".to_string(),
            });
            wires.push(EffectGraphWire {
                from_node: add_id,
                from_port: "out".to_string(),
                to_node: divide_id,
                to_port: "b".to_string(),
            });
            wires.push(EffectGraphWire {
                from_node: divide_id,
                from_port: "out".to_string(),
                to_node: sqrt_id,
                to_port: "a".to_string(),
            });
            wires.push(EffectGraphWire {
                from_node: sqrt_id,
                from_port: "out".to_string(),
                to_node: node_id,
                to_port: "roughness".to_string(),
            });

            let mut max_node = math_node(
                max_id,
                max_node_id.clone(),
                handle.as_deref(),
                5,
                Some(power),
                Some(1.0),
            );
            let add_node = math_node(
                add_id,
                add_node_id.clone(),
                handle.as_deref(),
                0,
                None,
                Some(2.0),
            );
            if had_power_exposure {
                max_node.exposed_params.insert("a".to_string());
            }
            let math_handle = max_node.handle.clone();
            nodes.push(max_node);
            nodes.push(add_node);
            nodes.push(math_node(
                divide_id,
                divide_node_id,
                handle.as_deref(),
                3,
                Some(2.0),
                None,
            ));
            nodes.push(math_node(
                sqrt_id,
                sqrt_node_id,
                handle.as_deref(),
                14,
                None,
                None,
            ));

            nodes[index].params.remove("specular_power");
            replacements.push(PowerReplacement {
                old_node_id: stable_id,
                old_handle: handle,
                math_node_id: max_node_id,
                math_handle,
            });
        } else {
            let roughness = phong_roughness(power);
            nodes[index].params.remove("specular_power");
            nodes[index].params.insert(
                "roughness".to_string(),
                SerializedParamValue::Float { value: roughness },
            );
            if had_power_exposure {
                nodes[index].exposed_params.insert("roughness".to_string());
            }
        }

        *changed = true;
    }
}

fn rewrite_bindings(
    def: &mut EffectGraphDef,
    replacements: &[PowerReplacement],
    migrated_nodes: &[MigratedNode],
    changed: &mut bool,
) {
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return;
    };
    for binding in &mut metadata.bindings {
        let BindingTarget::Node { node_id, param } = &mut binding.target else {
            continue;
        };
        if param.starts_with("specular_color_")
            && migrated_nodes
                .iter()
                .any(|node| node_matches(node_id, node))
        {
            let old = std::mem::take(param);
            *param = old.replacen("specular_color_", "specular_tint_", 1);
            *changed = true;
            continue;
        }
        if param != "specular_power" {
            continue;
        }
        let Some(replacement) = replacements
            .iter()
            .find(|replacement| binding_matches_node(node_id, replacement))
        else {
            continue;
        };
        *node_id = replacement.math_node_id.clone();
        *param = "a".to_string();
        *changed = true;
    }
}

fn rewrite_group_params(
    group: &mut crate::effect_graph_def::GroupDef,
    replacements: &[PowerReplacement],
    migrated_nodes: &[MigratedNode],
    changed: &mut bool,
) {
    for param in &mut group.interface.params {
        let migrated = migrated_nodes.iter().any(|node| {
            node.old_handle
                .as_deref()
                .is_some_and(|handle| handle == param.target_handle)
        });
        if !migrated {
            continue;
        }
        if param.target_param.starts_with("specular_color_") {
            let old = std::mem::take(&mut param.target_param);
            param.target_param = old.replacen("specular_color_", "specular_tint_", 1);
            *changed = true;
        } else if param.target_param == "specular_power"
            && let Some(replacement) = replacements.iter().find(|replacement| {
                replacement
                    .old_handle
                    .as_deref()
                    .is_some_and(|handle| handle == param.target_handle)
            })
            && let Some(math_handle) = replacement.math_handle.as_ref()
        {
            param.target_handle = math_handle.clone();
            param.target_param = "a".to_string();
            *changed = true;
        }
    }
    for node in &mut group.nodes {
        if let Some(inner) = node.group.as_deref_mut() {
            rewrite_group_params(inner, replacements, migrated_nodes, changed);
        }
    }
}

fn node_matches(node_id: &NodeId, node: &MigratedNode) -> bool {
    (!node.old_node_id.is_empty() && node_id == &node.old_node_id)
        || (node.old_node_id.is_empty()
            && node
                .old_handle
                .as_deref()
                .is_some_and(|handle| node_id.as_str() == handle))
}

fn binding_matches_node(node_id: &NodeId, replacement: &PowerReplacement) -> bool {
    (!replacement.old_node_id.is_empty() && node_id == &replacement.old_node_id)
        || (replacement.old_node_id.is_empty()
            && replacement
                .old_handle
                .as_deref()
                .is_some_and(|handle| node_id.as_str() == handle))
}

fn rename_param(params: &mut BTreeMap<String, SerializedParamValue>, old: &str, new: &str) {
    if let Some(value) = params.remove(old) {
        params.insert(new.to_string(), value);
    }
}

fn rename_exposed_param(params: &mut BTreeSet<String>, old: &str, new: &str) {
    if params.remove(old) {
        params.insert(new.to_string());
    }
}

fn rename_port(port: &mut String) {
    if let Some(suffix) = port.strip_prefix("specular_color_") {
        *port = format!("specular_tint_{suffix}");
    }
}

fn serialized_float(value: &SerializedParamValue) -> Option<f32> {
    match value {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        _ => None,
    }
}

fn phong_roughness(power: f32) -> f32 {
    (2.0 / (power.max(1.0) + 2.0)).sqrt()
}

fn derived_node_id(old: &NodeId, document_id: u32, suffix: &str) -> NodeId {
    if old.is_empty() {
        NodeId::new(format!("phong-{document_id}-{suffix}-{}", short_id()))
    } else {
        NodeId::new(format!("{}-{suffix}", old.as_str()))
    }
}

fn math_node(
    id: u32,
    node_id: NodeId,
    source_handle: Option<&str>,
    op: u32,
    a: Option<f32>,
    b: Option<f32>,
) -> EffectGraphNode {
    let suffix = match op {
        0 => "shininess_add",
        3 => "shininess_divide",
        5 => "shininess_max",
        _ => "shininess_sqrt",
    };
    let handle = source_handle.map(|handle| format!("{handle}_{suffix}"));
    let mut params = BTreeMap::new();
    params.insert("op".to_string(), SerializedParamValue::Enum { value: op });
    if let Some(a) = a {
        params.insert("a".to_string(), SerializedParamValue::Float { value: a });
    }
    if let Some(b) = b {
        params.insert("b".to_string(), SerializedParamValue::Float { value: b });
    }
    EffectGraphNode {
        id,
        node_id,
        type_id: "node.math".to_string(),
        handle,
        params,
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{
        BindingDef, EffectGraphDef, GROUP_INPUT_TYPE_ID, GroupDef, GroupInterface, GroupParamDef,
        InterfacePortDef, ParamSpecDef, PresetMetadata,
    };
    use crate::effects::ParamConvert;

    fn node(id: u32, node_id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: Some(node_id.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn metadata(binding: BindingDef) -> PresetMetadata {
        PresetMetadata {
            id: crate::PresetTypeId::new("legacy"),
            display_name: "Legacy".to_string(),
            category: "Test".to_string(),
            osc_prefix: "legacy".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: vec![ParamSpecDef {
                id: binding.id.clone(),
                name: "Shininess".to_string(),
                min: 1.0,
                max: 256.0,
                default_value: 32.0,
                ..Default::default()
            }],
            bindings: vec![binding],
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: None,
            scene_modifier: None,
        }
    }

    #[test]
    fn nested_wired_and_bound_phong_round_trips_and_is_idempotent() {
        let mut material = node(1, "mat", PHONG_TYPE_ID);
        material.params.insert(
            "specular_color_r".to_string(),
            SerializedParamValue::Float { value: 0.25 },
        );
        material
            .exposed_params
            .insert("specular_color_r".to_string());
        material.params.insert(
            "specular_power".to_string(),
            SerializedParamValue::Float { value: 64.0 },
        );
        material.exposed_params.insert("specular_power".to_string());
        let mut source = node(2, "shine_source", "node.value");
        source.params.insert(
            "value".to_string(),
            SerializedParamValue::Float { value: 64.0 },
        );

        let mut inner = node(11, "inner_mat", PHONG_TYPE_ID);
        inner.params.insert(
            "specular_power".to_string(),
            SerializedParamValue::Float { value: 8.0 },
        );
        let group = GroupDef {
            interface: GroupInterface {
                inputs: vec![InterfacePortDef {
                    name: "shine_in".to_string(),
                    port_type: "Scalar(F32)".to_string(),
                }],
                outputs: Vec::new(),
                params: vec![GroupParamDef {
                    name: "shine".to_string(),
                    target_handle: "inner_mat".to_string(),
                    target_param: "specular_power".to_string(),
                    default: None,
                }],
            },
            nodes: vec![inner, node(12, "group_input", GROUP_INPUT_TYPE_ID)],
            wires: vec![EffectGraphWire {
                from_node: 12,
                from_port: "shine_in".to_string(),
                to_node: 11,
                to_port: "specular_power".to_string(),
            }],
            tint: None,
        };
        let mut grouped = node(10, "group", "system.group");
        grouped.group = Some(Box::new(group));

        let binding = BindingDef {
            id: "shine".to_string(),
            label: "Shininess".to_string(),
            default_value: 64.0,
            target: BindingTarget::Node {
                node_id: NodeId::new("mat"),
                param: "specular_power".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: true,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(metadata(binding)),
            scene_modifiers: Vec::new(),
            nodes: vec![material, source, grouped],
            wires: vec![
                EffectGraphWire {
                    from_node: 2,
                    from_port: "out".to_string(),
                    to_node: 1,
                    to_port: "specular_power".to_string(),
                },
                EffectGraphWire {
                    from_node: 2,
                    from_port: "out".to_string(),
                    to_node: 1,
                    to_port: "specular_color_r".to_string(),
                },
                EffectGraphWire {
                    from_node: 2,
                    from_port: "out".to_string(),
                    to_node: 10,
                    to_port: "shine_in".to_string(),
                },
            ],
        };
        let original = def.clone();

        assert!(migrate_phong_to_pbr(&mut def));
        assert_eq!(def.nodes[0].type_id, PBR_TYPE_ID);
        assert_eq!(
            def.nodes[0].params.get("specular_tint_r"),
            Some(&SerializedParamValue::Float { value: 0.25 })
        );
        assert!(def.nodes[0].exposed_params.contains("specular_tint_r"));
        assert!(!def.nodes[0].exposed_params.contains("specular_color_r"));
        assert!(def.nodes.iter().any(|node| node.type_id == "node.math"));
        assert!(def.nodes.iter().any(|node| {
            node.type_id == "node.math"
                && node.params.get("op") == Some(&SerializedParamValue::Enum { value: 5 })
                && node.params.get("b") == Some(&SerializedParamValue::Float { value: 1.0 })
                && node.exposed_params.contains("a")
        }));
        let BindingTarget::Node { node_id, param } =
            &def.preset_metadata.as_ref().unwrap().bindings[0].target
        else {
            panic!("expected node binding")
        };
        assert_eq!(param, "a");
        assert_ne!(node_id, &NodeId::new("mat"));
        let nested = def.nodes[2].group.as_ref().unwrap();
        assert_eq!(nested.nodes[0].type_id, PBR_TYPE_ID);
        assert!(def.wires.iter().any(|wire| wire.to_port == "roughness"));
        assert!(
            def.wires
                .iter()
                .any(|wire| wire.to_port == "specular_tint_r")
        );
        assert!(
            nested
                .wires
                .iter()
                .any(|wire| wire.to_port == "a" && wire.to_node != 11)
        );
        assert_eq!(
            nested.interface.params[0].target_handle,
            "inner_mat_shininess_max"
        );
        assert_eq!(nested.interface.params[0].target_param, "a");

        let json = serde_json::to_string(&def).unwrap();
        let mut round_trip: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert!(!migrate_phong_to_pbr(&mut round_trip));
        assert_eq!(round_trip, def);
        assert_ne!(original, def);
    }

    #[test]
    fn unbound_power_uses_the_pbr_roughness_formula_and_current_pbr_is_unchanged() {
        let mut legacy = node(1, "mat", PHONG_TYPE_ID);
        legacy.params.insert(
            "specular_power".to_string(),
            SerializedParamValue::Float { value: 32.0 },
        );
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![legacy],
            wires: Vec::new(),
        };
        assert!(migrate_phong_to_pbr(&mut def));
        assert_eq!(
            def.nodes[0].params.get("roughness"),
            Some(&SerializedParamValue::Float {
                value: (2.0_f32 / 34.0).sqrt()
            })
        );

        let mut current = def.clone();
        assert!(!migrate_phong_to_pbr(&mut current));
        assert_eq!(current, def);
    }

    #[test]
    fn roughness_formula_clamps_negative_and_zero_power() {
        let floor = (2.0_f32 / 3.0).sqrt();
        assert_eq!(phong_roughness(-32.0), floor);
        assert_eq!(phong_roughness(0.0), floor);
        assert_eq!(phong_roughness(32.0), (2.0_f32 / 34.0).sqrt());
    }

    #[test]
    fn unrelated_specular_binding_is_left_alone() {
        let legacy = node(1, "mat", PHONG_TYPE_ID);
        let binding = BindingDef {
            id: "other".to_string(),
            label: "Other".to_string(),
            default_value: 0.0,
            target: BindingTarget::Node {
                node_id: NodeId::new("other_node"),
                param: "specular_color_r".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(metadata(binding)),
            scene_modifiers: Vec::new(),
            nodes: vec![legacy, node(2, "other_node", "node.value")],
            wires: Vec::new(),
        };

        assert!(migrate_phong_to_pbr(&mut def));
        let BindingTarget::Node { node_id, param } =
            &def.preset_metadata.as_ref().unwrap().bindings[0].target
        else {
            panic!("expected node binding")
        };
        assert_eq!(node_id, &NodeId::new("other_node"));
        assert_eq!(param, "specular_color_r");
    }

    #[test]
    fn missing_identifiers_are_minted_for_scene_environment_repair() {
        let mut material = node(1, "", PHONG_TYPE_ID);
        material.handle = None;
        let mut render = node(2, "render", "node.render_scene");
        render.params.insert(
            "objects".to_string(),
            SerializedParamValue::Int { value: 1 },
        );
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![material, render],
            wires: vec![EffectGraphWire {
                from_node: 1,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "material_0".to_string(),
            }],
        };

        assert!(migrate_phong_to_pbr(&mut def));
        assert!(!def.nodes[0].node_id.is_empty());
        assert!(def.nodes.iter().any(|node| {
            node.type_id == "node.bake_environment"
                && node.params.get("intensity") == Some(&SerializedParamValue::Float { value: 0.0 })
        }));
        let snapshot = def.clone();
        assert!(!migrate_phong_to_pbr(&mut def));
        assert_eq!(def, snapshot);
    }

    #[test]
    fn cyclic_scene_object_material_link_does_not_recurse() {
        let mut material = node(1, "", PHONG_TYPE_ID);
        material.handle = None;
        let object = node(2, "object", "node.scene_object");
        let render = node(3, "render", "node.render_scene");
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![material, object, render],
            wires: vec![
                EffectGraphWire {
                    from_node: 2,
                    from_port: "object".to_string(),
                    to_node: 3,
                    to_port: "object_0".to_string(),
                },
                EffectGraphWire {
                    from_node: 2,
                    from_port: "object".to_string(),
                    to_node: 2,
                    to_port: "material".to_string(),
                },
            ],
        };

        assert!(migrate_phong_to_pbr(&mut def));
        assert!(
            !def.nodes
                .iter()
                .any(|node| node.type_id == "node.bake_environment")
        );
    }
}
