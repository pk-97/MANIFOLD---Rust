//! v1.24.0 → v1.25.0: Motion Blur reads scene depth so a moving foreground smears over the background and the background never bleeds over it. Saved graphs have no depth wire; this rung adds one from the same node that feeds its velocity. Idempotent.

use serde_json::{Value, json};

const MOTION_BLUR: &str = "node.motion_blur";

pub(crate) fn migrate(root: &mut Value) {
    let mut migrated = 0usize;
    crate::migrate::for_each_preset_instance(root, |fx| {
        let Value::Object(map) = fx else { return };
        if let Some(graph) = map.get_mut("graph") {
            migrated += usize::from(migrate_graph_value(graph));
        }
    });
    if let Some(presets) = root
        .get_mut("embeddedPresets")
        .and_then(|v| v.as_array_mut())
    {
        for preset in presets.iter_mut() {
            if let Some(def) = preset.get_mut("def") {
                migrated += usize::from(migrate_graph_value(def));
            }
        }
    }
    if migrated > 0 {
        super::note_migration(format!(
            "{migrated} graph(s) gave Motion Blur the scene depth wire (v1.25.0)"
        ));
    }
}

/// Migrate one graph-shaped value. Returns true when it was modified.
fn migrate_graph_value(graph: &mut Value) -> bool {
    let name = graph
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Unnamed graph")
        .to_string();
    migrate_body(graph, &name)
}

/// One wire scope: a graph or a group's body. Wires are local to their scope,
/// so each body is migrated against its own wires, groups first.
fn migrate_body(body: &mut Value, name: &str) -> bool {
    let mut changed = false;
    if let Some(nodes) = body.get_mut("nodes").and_then(Value::as_array_mut) {
        for node in nodes.iter_mut() {
            if let Some(group) = node.get_mut("group") {
                changed |= migrate_body(group, name);
            }
        }
    }
    let wires = body
        .get("wires")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let blur_ids: Vec<Value> = body
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| n.get("typeId").and_then(Value::as_str) == Some(MOTION_BLUR))
        .map(|n| n["id"].clone())
        .collect();
    let mut added = Vec::new();
    for id in &blur_ids {
        if wire_into(&wires, id, "depth").is_some() {
            continue;
        }
        match wire_into(&wires, id, "velocity") {
            Some(w) if w["fromPort"] == "velocity" => added.push(json!({
                "fromNode": w["fromNode"],
                "fromPort": "depth",
                "toNode": id,
                "toPort": "depth"
            })),
            _ => super::note_migration(format!(
                "{name}: Motion Blur's velocity doesn't come straight from a scene render, so its new depth input was left unwired. Wire the scene's depth into it."
            )),
        }
    }
    if added.is_empty() {
        return changed;
    }
    let Some(map) = body.as_object_mut() else {
        return changed;
    };
    let Some(out) = map
        .entry("wires")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return changed;
    };
    out.extend(added);
    true
}

/// The first wire feeding `port` on node `to`.
fn wire_into<'a>(wires: &'a [Value], to: &Value, port: &str) -> Option<&'a Value> {
    wires
        .iter()
        .find(|w| w["toNode"] == *to && w["toPort"] == port)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/cliftest_project.json");

    /// Every wire scope in the project: a graph, a group body or an embedded def.
    fn wire_scopes<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(map) => {
                if map.get("nodes").is_some_and(Value::is_array)
                    && map.get("wires").is_some_and(Value::is_array)
                {
                    out.push(v);
                }
                for child in map.values() {
                    wire_scopes(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    wire_scopes(item, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_velocity_fed_motion_blur_gains_one_depth_wire_and_a_rerun_adds_none() {
        let mut root: Value = serde_json::from_str(FIXTURE).unwrap();
        migrate(&mut root);
        let mut scopes = Vec::new();
        wire_scopes(&root, &mut scopes);
        let mut checked = 0;
        for scope in scopes {
            let wires = scope["wires"].as_array().unwrap();
            for node in scope["nodes"].as_array().unwrap() {
                if node["typeId"] != MOTION_BLUR {
                    continue;
                }
                let velocity: Vec<&Value> = wires
                    .iter()
                    .filter(|w| {
                        w["toNode"] == node["id"]
                            && w["toPort"] == "velocity"
                            && w["fromPort"] == "velocity"
                    })
                    .collect();
                if velocity.is_empty() {
                    continue;
                }
                checked += 1;
                let depth: Vec<&Value> = wires
                    .iter()
                    .filter(|w| w["toNode"] == node["id"] && w["toPort"] == "depth")
                    .collect();
                assert_eq!(depth.len(), 1, "motion blur {}", node["id"]);
                assert_eq!(depth[0]["fromNode"], velocity[0]["fromNode"]);
                assert_eq!(depth[0]["fromPort"], "depth");
            }
        }
        assert!(checked > 0, "fixture has no velocity-fed motion blur");

        let once = root.clone();
        migrate(&mut root);
        assert_eq!(root, once, "second run changes nothing");
    }

    #[test]
    fn velocity_from_another_port_gets_no_depth_wire() {
        let mut root = json!({"projectVersion": "1.24.0", "timeline": {"layers": [{"genParams": {"graph": {
            "name": "Saved",
            "nodes": [
                {"id": 0, "nodeId": "scene", "typeId": "node.render_scene"},
                {"id": 1, "nodeId": "motion_blur", "typeId": MOTION_BLUR}
            ],
            "wires": [{"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "velocity"}]
        }}}]}});
        migrate(&mut root);
        let wires = root["timeline"]["layers"][0]["genParams"]["graph"]["wires"]
            .as_array()
            .unwrap();
        assert_eq!(wires.len(), 1);
        assert!(wires.iter().all(|w| w["toPort"] != "depth"));
    }
}
