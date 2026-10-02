//! `node.rigid_body` takes density, not mass: the renderer derives mass from
//! density times the installed hull's volume, so mass follows size. A stored
//! mass was size-blind, so it is dropped rather than converted; the body falls
//! back to the default density. Wires into the old `mass` port and card
//! controls bound to it go with it.

use std::collections::BTreeSet;

use serde_json::Value;

const RIGID_BODY: &str = "node.rigid_body";

pub(crate) fn migrate(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("nodes").is_some_and(Value::is_array) {
                migrate_graph(value);
            }
            if let Value::Object(map) = value {
                map.values_mut().for_each(migrate);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(migrate),
        _ => {}
    }
}

fn migrate_graph(graph: &mut Value) {
    let name = graph.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph").to_owned();
    let mut dropped = 0usize;
    let mut body_ids = BTreeSet::new();
    if let Some(nodes) = graph.get_mut("nodes").and_then(Value::as_array_mut) {
        for node in nodes.iter_mut() {
            if node.get("typeId").and_then(Value::as_str) != Some(RIGID_BODY) {
                continue;
            }
            if let Some(id) = node.get("id").and_then(Value::as_u64) {
                body_ids.insert(id);
            }
            if let Some(params) = node.get_mut("params").and_then(Value::as_object_mut) {
                params.remove("mass");
            }
        }
    }
    if let Some(wires) = graph.get_mut("wires").and_then(Value::as_array_mut) {
        let before = wires.len();
        wires.retain(|wire| {
            !(wire.get("toPort").and_then(Value::as_str) == Some("mass")
                && wire.get("toNode").and_then(Value::as_u64).is_some_and(|id| body_ids.contains(&id)))
        });
        dropped += before - wires.len();
    }

    let mut body_node_ids = BTreeSet::new();
    collect_body_node_ids(graph, &mut body_node_ids);
    if let Some(meta) = graph.get_mut("presetMetadata").and_then(Value::as_object_mut) {
        let mut removed = BTreeSet::new();
        if let Some(bindings) = meta.get_mut("bindings").and_then(Value::as_array_mut) {
            bindings.retain(|binding| {
                let target = binding.get("target");
                let hit = target.and_then(|t| t.get("param")).and_then(Value::as_str) == Some("mass")
                    && target
                        .and_then(|t| t.get("nodeId"))
                        .and_then(Value::as_str)
                        .is_some_and(|id| body_node_ids.contains(id));
                if hit && let Some(id) = binding.get("id").and_then(Value::as_str) {
                    removed.insert(id.to_owned());
                }
                !hit
            });
        }
        if let Some(params) = meta.get_mut("params").and_then(Value::as_array_mut) {
            params.retain(|param| {
                !param.get("id").and_then(Value::as_str).is_some_and(|id| removed.contains(id))
            });
        }
        dropped += removed.len();
    }
    if dropped > 0 {
        super::note_migration(format!(
            "{name}: rigid bodies now take density and derive mass from their size. {dropped} mass wire(s) or card control(s) were removed; bodies use the default density."
        ));
    }
}

fn collect_body_node_ids(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("typeId").and_then(Value::as_str) == Some(RIGID_BODY)
                && let Some(id) = map.get("nodeId").and_then(Value::as_str)
            {
                out.insert(id.to_owned());
            }
            map.values().for_each(|v| collect_body_node_ids(v, out));
        }
        Value::Array(values) => values.iter().for_each(|v| collect_body_node_ids(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    fn project() -> Value {
        json!({"projectVersion":"1.16.0", "timeline":{"layers":[{"genParams":{
            "graph":{"version":1,"name":"Box","nodes":[
                {"id":1,"typeId":"node.scalar","nodeId":"knob"},
                {"id":2,"typeId":"node.rigid_body","nodeId":"box_body","params":{
                    "mass":{"type":"Float","value":62.5},"friction":{"type":"Float","value":0.4}}},
                {"id":3,"typeId":"node.transform_3d","nodeId":"t","params":{"mass":{"type":"Float","value":1.0}}}
            ],"wires":[
                {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"mass"},
                {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"friction"}
            ],"presetMetadata":{
                "params":[{"id":"box_mass"},{"id":"box_friction"}],
                "bindings":[
                    {"id":"box_mass","target":{"kind":"node","nodeId":"box_body","param":"mass"}},
                    {"id":"box_friction","target":{"kind":"node","nodeId":"box_body","param":"friction"}}
                ]}}
        }}]}})
    }

    #[test]
    fn mass_param_wires_and_cards_are_dropped_and_rerun_is_a_no_op() {
        super::super::take_migration_notes();
        let after: Value = serde_json::from_str(&crate::migrate::migrate_if_needed(&project().to_string()).unwrap()).unwrap();
        assert_eq!(after["projectVersion"], manifold_core::project::CURRENT_PROJECT_VERSION);
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert!(graph["nodes"][1]["params"].get("mass").is_none());
        assert_eq!(graph["nodes"][1]["params"]["friction"]["value"], 0.4);
        assert_eq!(graph["nodes"][2]["params"]["mass"]["value"], 1.0, "only rigid bodies change");
        assert_eq!(graph["wires"].as_array().unwrap().len(), 1);
        assert_eq!(graph["wires"][0]["toPort"], "friction");
        let meta = &graph["presetMetadata"];
        assert_eq!(meta["bindings"].as_array().unwrap().len(), 1);
        assert_eq!(meta["params"], json!([{"id":"box_friction"}]));
        assert_eq!(super::super::take_migration_notes().len(), 1);

        let second: Value = serde_json::from_str(&crate::migrate::migrate_if_needed(&after.to_string()).unwrap()).unwrap();
        assert_eq!(second, after);
    }
}
