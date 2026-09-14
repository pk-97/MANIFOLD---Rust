//! Load-time repair for periodic scene-modifier controls.
//!
//! Periodicity is an authored parameter property.  This module only repairs
//! known scene-modifier controls after checking their actual node binding and
//! mapped range; it deliberately does not infer periodicity from angle hints.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect_graph_def::{
    AliasEntry, BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef,
    SerializedParamValue,
};
use crate::effects::ParamConvert;
use crate::id::NodeId;

#[derive(Clone, Copy)]
struct QualifiedBinding {
    period: f32,
    scale: f32,
}

/// Repair stale `wraps` metadata on scene-modifier graphs and their host
/// scene-modifier bindings.
///
/// Returns `true` when one or more `wraps` flags were changed.  Existing
/// `wraps: true` values are preserved, and no other serialized field is
/// modified.
pub fn repair_scene_modifier_periodicity(def: &mut EffectGraphDef) -> bool {
    let mut changed = false;
    let mut local_qualifications: BTreeMap<(String, String), Vec<QualifiedBinding>> =
        BTreeMap::new();
    let mut local_aliases: BTreeMap<String, Vec<AliasEntry>> = BTreeMap::new();

    for modifier in &mut def.scene_modifiers {
        let local = modifier.graph.as_mut();
        if local.preset_metadata.is_none() {
            continue;
        }

        let qualifications = qualify_local_metadata(local);
        let Some(metadata) = local.preset_metadata.as_mut() else {
            continue;
        };
        local_aliases.insert(
            modifier.id.as_str().to_string(),
            metadata.param_aliases.clone(),
        );
        for (param_id, bindings) in &qualifications {
            if let Some(param) = metadata.params.iter_mut().find(|p| {
                resolve_alias(&metadata.param_aliases, &p.id).as_deref() == Some(param_id.as_str())
            }) && !param.wraps
            {
                param.wraps = true;
                changed = true;
            }
            local_qualifications.insert(
                (modifier.id.as_str().to_string(), param_id.clone()),
                bindings.clone(),
            );
        }
    }

    let Some(metadata) = def.preset_metadata.as_mut() else {
        return changed;
    };

    // A host macro may fan out to several bindings.  It is only safe to mark
    // it periodic when every binding in that fan-out is a qualified modifier
    // binding and every mapped local target retains a whole period.
    let aliases = metadata.param_aliases.clone();
    let mut host_param_ids = BTreeSet::new();
    for param in &metadata.params {
        let Some(canonical) = resolve_alias(&aliases, &param.id) else {
            continue;
        };
        let bindings: Vec<&BindingDef> = metadata
            .bindings
            .iter()
            .filter(|binding| {
                resolve_alias(&aliases, &binding.id).as_deref() == Some(canonical.as_str())
            })
            .collect();
        if bindings.is_empty() {
            continue;
        }

        let mut qualified = true;
        for binding in bindings {
            if binding.convert != ParamConvert::Float || !binding.offset.is_finite() {
                qualified = false;
                break;
            }
            let BindingTarget::SceneModifier {
                modifier_id,
                param_id,
            } = &binding.target
            else {
                qualified = false;
                break;
            };
            let Some(local_param_id) = local_aliases
                .get(modifier_id.as_str())
                .and_then(|aliases| resolve_alias(aliases, param_id))
            else {
                qualified = false;
                break;
            };
            let Some(local_bindings) =
                local_qualifications.get(&(modifier_id.as_str().to_string(), local_param_id))
            else {
                qualified = false;
                break;
            };
            if !host_span_is_periodic(param, binding, local_bindings) {
                qualified = false;
                break;
            }
        }
        if qualified {
            host_param_ids.insert(canonical);
        }
    }

    for param in &mut metadata.params {
        if !param.wraps
            && resolve_alias(&aliases, &param.id).is_some_and(|id| host_param_ids.contains(&id))
        {
            param.wraps = true;
            changed = true;
        }
    }

    changed
}

fn qualify_local_metadata(
    metadata_graph: &EffectGraphDef,
) -> BTreeMap<String, Vec<QualifiedBinding>> {
    let Some(metadata) = metadata_graph.preset_metadata.as_ref() else {
        return BTreeMap::new();
    };
    let aliases = &metadata.param_aliases;
    let mut out = BTreeMap::new();

    for param in &metadata.params {
        let Some(canonical) = resolve_alias(aliases, &param.id) else {
            continue;
        };
        let bindings: Vec<&BindingDef> = metadata
            .bindings
            .iter()
            .filter(|binding| {
                resolve_alias(aliases, &binding.id).as_deref() == Some(canonical.as_str())
            })
            .collect();
        if bindings.is_empty() {
            continue;
        }
        let mut qualified = Vec::with_capacity(bindings.len());
        for binding in bindings {
            if binding.convert != ParamConvert::Float || !binding.offset.is_finite() {
                qualified.clear();
                break;
            }
            let BindingTarget::Node {
                node_id,
                param: target_param,
            } = &binding.target
            else {
                qualified.clear();
                break;
            };
            let Some(node) = find_unique_node(&metadata_graph.nodes, node_id) else {
                qualified.clear();
                break;
            };
            let Some(period) = periodicity(node.type_id.as_str(), target_param) else {
                qualified.clear();
                break;
            };
            if !matches!(
                node.params.get(target_param),
                None | Some(SerializedParamValue::Float { .. })
            ) || !span_is_periodic(param.min, param.max, binding.scale, period)
            {
                qualified.clear();
                break;
            }
            qualified.push(QualifiedBinding {
                period,
                scale: binding.scale,
            });
        }
        if !qualified.is_empty() {
            out.insert(canonical, qualified);
        }
    }
    out
}

fn resolve_alias(entries: &[AliasEntry], id: &str) -> Option<String> {
    let mut current = id.to_string();
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let Some(alias) = entries.iter().find(|entry| entry.old == current) else {
            return Some(current);
        };
        current = alias.new.clone()?;
    }
}

fn host_span_is_periodic(
    host_param: &ParamSpecDef,
    host_binding: &BindingDef,
    local_bindings: &[QualifiedBinding],
) -> bool {
    span_is_periodic_with_periods(
        host_param.min,
        host_param.max,
        host_binding.scale,
        local_bindings,
    )
}

fn span_is_periodic_with_periods(
    min: f32,
    max: f32,
    host_scale: f32,
    local_bindings: &[QualifiedBinding],
) -> bool {
    if !min.is_finite() || !max.is_finite() || !host_scale.is_finite() || max <= min {
        return false;
    }
    local_bindings.iter().all(|binding| {
        let scale = host_scale * binding.scale;
        span_is_periodic(min, max, scale, binding.period)
    })
}

fn span_is_periodic(min: f32, max: f32, scale: f32, period: f32) -> bool {
    if !min.is_finite()
        || !max.is_finite()
        || !scale.is_finite()
        || !period.is_finite()
        || max <= min
        || scale == 0.0
    {
        return false;
    }
    let span = ((max - min) * scale).abs();
    if !span.is_finite() || span == 0.0 {
        return false;
    }
    let multiple = span / period;
    if !multiple.is_finite() {
        return false;
    }
    let nearest = multiple.round();
    nearest >= 1.0 && (multiple - nearest).abs() <= 8.0 * f32::EPSILON * multiple.abs().min(16.0)
}

fn periodicity(type_id: &str, param: &str) -> Option<f32> {
    let phase = matches!(
        (type_id, param),
        ("node.transform_mesh_patches", "phase")
            | ("node.normal_wave_mesh", "phase")
            | ("node.wave_shear_mesh", "phase")
            | ("node.analytic_echo_instances", "phase")
    );
    if phase {
        return Some(1.0);
    }
    let orientation = matches!(
        (type_id, param),
        ("node.transform_mesh_patches", "yaw" | "pitch")
            | ("node.normal_wave_mesh", "yaw" | "pitch")
            | ("node.wave_shear_mesh", "yaw" | "pitch")
            | ("node.mesh_spatial_mask", "yaw" | "pitch")
            | ("node.mesh_stagger_envelope", "yaw" | "pitch")
            | ("node.loop_camera", "yaw" | "pitch" | "roll")
    );
    orientation.then_some(std::f32::consts::TAU)
}

fn find_unique_node<'a>(
    nodes: &'a [EffectGraphNode],
    node_id: &NodeId,
) -> Option<&'a EffectGraphNode> {
    fn visit<'a>(
        nodes: &'a [EffectGraphNode],
        node_id: &NodeId,
        found: &mut Option<&'a EffectGraphNode>,
        count: &mut usize,
    ) {
        for node in nodes {
            if node.node_id == *node_id {
                *count += 1;
                *found = Some(node);
            }
            if let Some(group) = &node.group {
                visit(&group.nodes, node_id, found, count);
            }
        }
    }
    let mut found = None;
    let mut count = 0;
    visit(nodes, node_id, &mut found, &mut count);
    if count == 1 { found } else { None }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::repair_scene_modifier_periodicity;
    use crate::effect_graph_def::EffectGraphDef;

    fn param(id: &str, min: f32, max: f32) -> Value {
        json!({"id": id, "name": id, "min": min, "max": max, "defaultValue": min})
    }

    fn node(type_id: &str, node_id: &str, params: &[&str]) -> Value {
        let values = params
            .iter()
            .map(|name| ((*name).to_string(), json!({"type": "Float", "value": 0.0})))
            .collect::<serde_json::Map<_, _>>();
        json!({"id": 1, "nodeId": node_id, "typeId": type_id, "params": values})
    }

    fn binding(id: &str, node_id: &str, target_param: &str, scale: f32) -> Value {
        json!({
            "id": id,
            "label": id,
            "defaultValue": 0.0,
            "scale": scale,
            "target": {"kind": "node", "nodeId": node_id, "param": target_param}
        })
    }

    fn local_graph(params: Value, bindings: Value, nodes: Value) -> Value {
        json!({
            "version": 3,
            "nodes": nodes,
            "wires": [],
            "presetMetadata": {
                "id": "local",
                "displayName": "Local",
                "category": "Geometry",
                "oscPrefix": "local",
                "params": params,
                "bindings": bindings
            }
        })
    }

    fn owner(local: Value, host_params: Value, host_bindings: Value) -> EffectGraphDef {
        serde_json::from_value(json!({
            "version": 3,
            "nodes": [],
            "wires": [],
            "presetMetadata": {
                "id": "owner",
                "displayName": "Owner",
                "category": "Geometry",
                "oscPrefix": "owner",
                "params": host_params,
                "bindings": host_bindings
            },
            "sceneModifiers": [{
                "id": "modifier",
                "scene": {"node": "scene"},
                "targets": "allObjects",
                "graph": local
            }]
        }))
        .expect("test graph parses")
    }

    #[test]
    fn repairs_local_and_host_and_is_idempotent_through_serde() {
        let local = local_graph(
            json!([param("phase", 0.0, 1.0)]),
            json!([binding("phase", "wave", "phase", 1.0)]),
            json!([node("node.normal_wave_mesh", "wave", &["phase"])]),
        );
        let mut graph = owner(
            local,
            json!([param("phase", 0.0, 1.0)]),
            json!([{
                "id": "phase",
                "label": "phase",
                "defaultValue": 0.0,
                "target": {"kind": "sceneModifier", "modifierId": "modifier", "paramId": "phase"}
            }]),
        );
        assert!(repair_scene_modifier_periodicity(&mut graph));
        assert!(
            graph.scene_modifiers[0]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params[0]
                .wraps
        );
        assert!(graph.preset_metadata.as_ref().unwrap().params[0].wraps);
        assert!(!repair_scene_modifier_periodicity(&mut graph));
        let wire = serde_json::to_value(&graph).expect("graph serializes");
        let mut reparsed: EffectGraphDef = serde_json::from_value(wire).expect("graph reparses");
        assert!(!repair_scene_modifier_periodicity(&mut reparsed));
        assert_eq!(graph, reparsed);
    }

    #[test]
    fn leaves_nonperiodic_controls_half_cycles_and_unqualified_fanout_alone() {
        let local = local_graph(
            json!([
                param("orbit", -1.0, 1.0),
                param("curl", -1.0, 1.0),
                param("recon", 0.0, 1.0),
                param("fov_y", 0.0, std::f32::consts::TAU),
                param("translate_x", -1.0, 1.0),
                param("phase", 0.0, 0.5),
                param("fanout", 0.0, 1.0)
            ]),
            json!([
                binding("orbit", "other", "orbit", 1.0),
                binding("curl", "other", "curl", 1.0),
                binding("recon", "other", "recon", 1.0),
                binding("fov_y", "camera", "fov_y", 1.0),
                binding("translate_x", "other", "translate_x", 1.0),
                binding("phase", "wave", "phase", 1.0),
                binding("fanout", "wave", "phase", 1.0),
                binding("fanout", "other", "unqualified", 1.0)
            ]),
            json!([
                node(
                    "node.other",
                    "other",
                    &["orbit", "curl", "recon", "translate_x"]
                ),
                node("node.loop_camera", "camera", &["fov_y"]),
                node("node.normal_wave_mesh", "wave", &["phase"])
            ]),
        );
        let mut graph = owner(local, json!([]), json!([]));
        assert!(!repair_scene_modifier_periodicity(&mut graph));
        let params = &graph.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params;
        assert!(params.iter().all(|param| !param.wraps));
    }

    #[test]
    fn accepts_full_cycles_after_both_binding_scales() {
        let local = local_graph(
            json!([param("phase", 0.0, 0.5)]),
            json!([binding("phase", "wave", "phase", 2.0)]),
            json!([node("node.wave_shear_mesh", "wave", &["phase"])]),
        );
        let mut graph = owner(
            local,
            json!([param("phase", 0.0, 1.0)]),
            json!([{
                "id": "phase",
                "label": "phase",
                "defaultValue": 0.0,
                "scale": 0.5,
                "target": {"kind": "sceneModifier", "modifierId": "modifier", "paramId": "phase"}
            }]),
        );
        assert!(repair_scene_modifier_periodicity(&mut graph));
        assert!(
            graph.scene_modifiers[0]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params[0]
                .wraps
        );
        assert!(graph.preset_metadata.as_ref().unwrap().params[0].wraps);
    }

    #[test]
    fn defaulted_echo_phase_is_periodic_but_discrete_bindings_are_not() {
        let make = |convert: &str, outer_convert: &str| {
            let mut local_binding = binding("phase", "echo", "phase", 1.0);
            local_binding["convert"] = json!({"type":convert});
            owner(
                local_graph(
                    json!([param("phase", 0.0, 1.0)]),
                    json!([local_binding]),
                    json!([node("node.analytic_echo_instances", "echo", &[])]),
                ),
                json!([param("outer_phase", 0.0, 1.0)]),
                json!([{
                    "id": "outer_phase", "label": "Phase", "defaultValue": 0.0,
                    "convert": {"type":outer_convert},
                    "target": {"kind":"sceneModifier", "modifierId":"modifier", "paramId":"phase"}
                }]),
            )
        };
        let mut graph = make("Float", "Float");
        assert!(repair_scene_modifier_periodicity(&mut graph));
        assert!(graph.preset_metadata.as_ref().unwrap().params[0].wraps);
        let mut discrete_local = make("IntRound", "Float");
        assert!(!repair_scene_modifier_periodicity(&mut discrete_local));
        let mut discrete_host = make("Float", "IntRound");
        assert!(repair_scene_modifier_periodicity(&mut discrete_host));
        assert!(!discrete_host.preset_metadata.as_ref().unwrap().params[0].wraps);
    }

    #[test]
    fn only_actual_periodic_angles_are_repaired() {
        for (type_id, name, expected) in [
            ("node.mesh_spatial_mask", "yaw", true),
            ("node.mesh_spatial_mask", "pitch", true),
            ("node.mesh_spatial_mask", "center_x", false),
            ("node.transform_mesh_patches", "yaw", true),
            ("node.transform_mesh_patches", "orbit", false),
            ("node.transform_mesh_patches", "rotation", false),
            ("node.ordered_recon_mesh", "rotation", false),
            ("node.loop_camera", "fov_y", false),
        ] {
            let mut graph = owner(
                local_graph(
                    json!([param(
                        "control",
                        -std::f32::consts::PI,
                        std::f32::consts::PI
                    )]),
                    json!([binding("control", "node", name, 1.0)]),
                    json!([node(type_id, "node", &[name])]),
                ),
                json!([]),
                json!([]),
            );
            assert_eq!(
                repair_scene_modifier_periodicity(&mut graph),
                expected,
                "{type_id}.{name}"
            );
        }
    }
}
