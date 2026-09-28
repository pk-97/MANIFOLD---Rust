//! Load-time repair for legacy fluid resolution card ranges.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};

const FLUID_SURFACE_TYPE_ID: &str = "node.fluid_surface";
const RESOLUTION_PARAM: &str = "resolution";
const LEGACY_MIN: f32 = 8.0;
const LEGACY_MAX: f32 = 96.0;

/// Repair the bundled fluid resolution card ranges (Basin 8..64, others 8..96)
/// that predate the current primitive range. The exact guard keeps authored
/// custom ranges intact, while `user_added` protects user-created exposures.
pub(super) fn migrate(def: &mut EffectGraphDef) -> bool {
    let mut fluid_node_ids = Vec::new();
    collect_fluid_nodes(&def.nodes, &mut fluid_node_ids);
    if fluid_node_ids.is_empty() {
        return false;
    }

    let Some((new_min, new_max)) = super::metadata_for_node_type(FLUID_SURFACE_TYPE_ID)
        .into_iter()
        .find(|metadata| metadata.name == RESOLUTION_PARAM)
        .map(|metadata| (metadata.min, metadata.max))
    else {
        return false;
    };
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return false;
    };
    let legacy_max = if metadata.id.as_str() == "WaterBasin" {
        64.0
    } else {
        LEGACY_MAX
    };

    let mut changed = false;
    for fluid_node_id in fluid_node_ids {
        let binding_ids: Vec<String> = metadata
            .bindings
            .iter()
            .filter(|binding| {
                !binding.user_added
                    && matches!(
                        &binding.target,
                        BindingTarget::Node { node_id, param }
                            if node_id == &fluid_node_id && param == RESOLUTION_PARAM
                    )
            })
            .map(|binding| binding.id.clone())
            .collect();

        for binding_id in binding_ids {
            let Some(spec) = metadata.params.iter_mut().find(|spec| {
                spec.id == binding_id && spec.min == LEGACY_MIN && spec.max == legacy_max
            }) else {
                continue;
            };
            spec.min = new_min;
            spec.max = new_max;
            changed = true;
        }
    }
    changed
}

fn collect_fluid_nodes(nodes: &[EffectGraphNode], out: &mut Vec<NodeId>) {
    for node in nodes {
        if node.type_id == FLUID_SURFACE_TYPE_ID {
            out.push(node.node_id.clone());
        }
        if let Some(group) = node.group.as_deref() {
            collect_fluid_nodes(&group.nodes, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::EffectGraphDef;

    const WATER_BASIN_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterBasin.json");
    const WATER_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreak.json");
    const HONEY_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/HoneyDamBreak.json");

    fn resolution_spec<'a>(
        def: &'a EffectGraphDef,
        id: &str,
    ) -> &'a manifold_core::effect_graph_def::ParamSpecDef {
        def.preset_metadata
            .as_ref()
            .expect("preset metadata")
            .params
            .iter()
            .find(|spec| spec.id == id)
            .expect("resolution spec")
    }

    fn legacy_def() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 2,
            "presetMetadata": {
                "id": "legacy-fluid",
                "displayName": "Legacy Fluid",
                "category": "Sim",
                "oscPrefix": "legacy_fluid",
                "params": [
                    {"id": "resolution", "name": "Resolution", "min": 8.0, "max": 96.0,
                     "defaultValue": 64.0, "wholeNumbers": true, "curve": "exponential", "invert": true},
                    {"id": "custom", "name": "Custom", "min": 12.0, "max": 80.0,
                     "defaultValue": 32.0, "wholeNumbers": true},
                    {"id": "user_resolution", "name": "User Resolution", "min": 8.0, "max": 96.0,
                     "defaultValue": 48.0, "wholeNumbers": true}
                ],
                "bindings": [
                    {"id": "resolution", "label": "Resolution", "defaultValue": 64.0,
                     "target": {"kind": "node", "nodeId": "fluid", "param": "resolution"},
                     "convert": {"type": "IntRound"}, "scale": 0.5, "offset": 4.0},
                    {"id": "custom", "label": "Custom", "defaultValue": 32.0,
                     "target": {"kind": "node", "nodeId": "fluid", "param": "resolution"},
                     "convert": {"type": "IntRound"}},
                    {"id": "user_resolution", "label": "User Resolution", "defaultValue": 48.0,
                     "target": {"kind": "node", "nodeId": "fluid", "param": "resolution"},
                     "convert": {"type": "IntRound"}, "userAdded": true}
                ]
            },
            "nodes": [
                {"id": 1, "nodeId": "fluid", "typeId": "node.fluid_surface"}
            ],
            "wires": []
        }))
        .expect("legacy fluid fixture parses")
    }

    #[test]
    fn shipped_fluid_presets_use_current_resolution_range() {
        for (json, legacy_max) in [
            (WATER_BASIN_JSON, 64.0),
            (WATER_DAM_BREAK_JSON, 96.0),
            (HONEY_DAM_BREAK_JSON, 96.0),
        ] {
            let mut def: EffectGraphDef = serde_json::from_str(json).expect("preset parses");
            let spec = resolution_spec(&def, "resolution");
            assert_eq!((spec.min, spec.max), (8.0, 512.0));
            def.preset_metadata
                .as_mut()
                .unwrap()
                .params
                .iter_mut()
                .find(|spec| spec.id == "resolution")
                .unwrap()
                .max = legacy_max;
            assert!(migrate(&mut def));
            assert_eq!(resolution_spec(&def, "resolution").max, 512.0);
            assert!(!migrate(&mut def));
        }
    }

    #[test]
    fn exact_legacy_range_migrates_once_and_survives_serialization() {
        let mut def = legacy_def();
        let before_binding = def
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find(|binding| binding.id == "resolution")
            .unwrap()
            .clone();

        assert!(migrate(&mut def));
        assert_eq!(
            (
                resolution_spec(&def, "resolution").min,
                resolution_spec(&def, "resolution").max
            ),
            (8.0, 512.0)
        );
        assert_eq!(
            def.preset_metadata
                .as_ref()
                .unwrap()
                .bindings
                .iter()
                .find(|binding| binding.id == "resolution")
                .unwrap(),
            &before_binding
        );

        let saved = serde_json::to_string(&def).expect("migrated fixture serializes");
        let mut reloaded: EffectGraphDef =
            serde_json::from_str(&saved).expect("migrated fixture reloads");
        assert!(!migrate(&mut reloaded));
        assert_eq!(reloaded, def);
    }

    #[test]
    fn custom_and_user_added_ranges_are_retained() {
        let mut def = legacy_def();
        assert!(migrate(&mut def));
        assert_eq!(
            (
                resolution_spec(&def, "custom").min,
                resolution_spec(&def, "custom").max
            ),
            (12.0, 80.0)
        );
        assert_eq!(
            (
                resolution_spec(&def, "user_resolution").min,
                resolution_spec(&def, "user_resolution").max
            ),
            (8.0, 96.0)
        );
    }

    #[test]
    fn nested_fluid_nodes_are_migrated_by_load_helper() {
        let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "presetMetadata": {
                "id": "nested-fluid",
                "displayName": "Nested Fluid",
                "category": "Sim",
                "oscPrefix": "nested_fluid",
                "params": [{"id": "nested_resolution", "name": "Resolution", "min": 8.0, "max": 96.0,
                            "defaultValue": 64.0, "wholeNumbers": true}],
                "bindings": [{"id": "nested_resolution", "label": "Resolution", "defaultValue": 64.0,
                              "target": {"kind": "node", "nodeId": "nested_fluid", "param": "resolution"},
                              "convert": {"type": "IntRound"}}]
            },
            "nodes": [{
                "id": 1, "nodeId": "fluid_group", "typeId": "group",
                "group": {
                    "interface": {"inputs": [], "outputs": []},
                    "nodes": [{"id": 2, "nodeId": "nested_fluid", "typeId": "node.fluid_surface"}],
                    "wires": []
                }
            }],
            "wires": []
        })).expect("nested fluid fixture parses");

        assert!(migrate(&mut def));
        assert_eq!(
            (
                resolution_spec(&def, "nested_resolution").min,
                resolution_spec(&def, "nested_resolution").max
            ),
            (8.0, 512.0)
        );
    }
}
