//! Scene modifiers used to address a node with a scoped reference
//! `{"scope": [group ids], "node": "leaf id"}`. Leaf ids are unique per graph,
//! so the leaf id alone names the node now, and every field that held a
//! scoped reference (modifier scene and targets, mesh frames, impulse recipes)
//! holds a plain id string. This rung turns each scoped reference inside a
//! preset instance or embedded preset into that string. Only objects with
//! exactly the `scope` array and `node` string keys are touched, so any other
//! object is left alone. A 1.24.0 file has no scoped references and passes
//! through unchanged.

use serde_json::{Map, Value};

pub(crate) fn migrate(root: &mut Value) {
    crate::migrate::for_each_preset_instance(root, rewrite_refs);
    if let Some(presets) = root
        .get_mut("embeddedPresets")
        .and_then(|v| v.as_array_mut())
    {
        for preset in presets.iter_mut() {
            if let Some(def) = preset.get_mut("def") {
                rewrite_refs(def);
            }
        }
    }
}

/// The leaf id of a scoped reference, or `None` for any other object.
fn scoped_node_id(map: &Map<String, Value>) -> Option<String> {
    if map.len() != 2 || !map.get("scope").is_some_and(Value::is_array) {
        return None;
    }
    map.get("node")?.as_str().map(str::to_string)
}

fn rewrite_refs(value: &mut Value) {
    let plain = match value {
        Value::Object(map) => scoped_node_id(map),
        _ => None,
    };
    if let Some(id) = plain {
        *value = Value::String(id);
        return;
    }
    match value {
        Value::Object(map) => map.values_mut().for_each(rewrite_refs),
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scoped_modifier_refs_become_plain_node_ids() {
        let before = json!({
            "projectVersion": "1.23.0",
            "settings": {"masterEffects": [{
                "effectType": "x",
                "graph": {"sceneModifiers": [{
                    "id": "m",
                    "scene": {"scope": [], "node": "scene"},
                    "targets": {"explicit": {"objects": [{"scope": ["g"], "node": "obj"}]}},
                    "meshFrames": [{"target": {"scope": ["g"], "node": "obj"}, "source": {"scope": [], "node": "src"}}]
                }]}
            }]}
        });
        let after: Value =
            serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap())
                .unwrap();
        assert_eq!(
            after["projectVersion"],
            manifold_core::project::CURRENT_PROJECT_VERSION
        );
        let modifier = &after["settings"]["masterEffects"][0]["graph"]["sceneModifiers"][0];
        assert_eq!(modifier["scene"], "scene");
        assert_eq!(modifier["targets"]["explicit"]["objects"][0], "obj");
        assert_eq!(modifier["meshFrames"][0]["target"], "obj");
        assert_eq!(modifier["meshFrames"][0]["source"], "src");
    }

    #[test]
    fn a_1_24_file_is_untouched() {
        let before = json!({
            "projectVersion": "1.24.0",
            "settings": {"masterEffects": [{
                "graph": {"sceneModifiers": [{"id": "m", "scene": "scene"}]}
            }]}
        });
        let after: Value =
            serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap())
                .unwrap();
        assert_eq!(after["settings"], before["settings"]);
    }
}
