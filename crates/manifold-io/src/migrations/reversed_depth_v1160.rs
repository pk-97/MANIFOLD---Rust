//! Camera depth encoding changes without changing authored camera/material values.
//! Built-in depth consumers change with the renderer. Custom raw-depth math cannot
//! be rewritten safely from its node name: report it through the existing load UI.

use std::collections::{BTreeMap, BTreeSet};
use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::Value;

pub(crate) fn audit(value: &Value) {
    match value {
        Value::Object(map) if map.contains_key("nodes") && map.contains_key("wires") => audit_graph(value),
        Value::Object(map) => map.values().for_each(audit),
        Value::Array(values) => values.iter().for_each(audit),
        _ => {}
    }
}

fn audit_graph(value: &Value) {
    // Ignore unrelated 2D/estimated-depth effects, including WireframeDepth.
    fn contains_camera_depth(v: &Value) -> bool {
        match v {
            Value::Object(m) => matches!(m.get("typeId").and_then(Value::as_str),
                Some("node.render_scene" | "node.render_mesh_diagram")) || m.values().any(contains_camera_depth),
            Value::Array(a) => a.iter().any(contains_camera_depth),
            _ => false,
        }
    }
    if !contains_camera_depth(value) { return; }
    let name = value.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph");
    let Ok(mut def) = serde_json::from_value::<EffectGraphDef>(value.clone()) else {
        super::note_migration(format!("{name}: could not audit custom camera-depth wiring for reversed-Z; review raw-depth calculations (near=1, far=0)."));
        return;
    };
    // Only authored routing is audited; modifier expansion adds built-in nodes.
    def.scene_modifiers.clear();
    let Ok(flat) = manifold_core::flatten::flatten_groups(&def) else {
        super::note_migration(format!("{name}: could not resolve grouped camera-depth wiring for reversed-Z; review raw-depth calculations (near=1, far=0)."));
        return;
    };
    let nodes: BTreeMap<_, _> = flat.nodes.iter().map(|n| (n.id, n)).collect();
    let mut custom = BTreeSet::new();
    for wire in &flat.wires {
        let (Some(source), Some(target)) = (nodes.get(&wire.from_node), nodes.get(&wire.to_node)) else { continue; };
        if wire.from_port != "depth" || !matches!(source.type_id.as_str(), "node.render_scene" | "node.render_mesh_diagram") { continue; }
        let supported = match target.type_id.as_str() {
            "node.coc_from_depth" | "node.ssao_gtao" | "node.ssao_from_depth" | "node.bilateral_blur" => wire.to_port == "depth",
            "node.render_mesh_diagram" => matches!(wire.to_port.as_str(), "surface_depth" | "scene_depth"),
            _ => false,
        };
        if !supported { custom.insert(target.handle.as_deref().unwrap_or(&target.type_id).to_owned()); }
    }
    if !custom.is_empty() {
        super::note_migration(format!("{name}: camera raw depth now uses near=1 and far/background=0. Review custom depth consumer(s): {}. Their graph and shader code were preserved; raw-depth math may need updating. Camera and material settings are unchanged.", custom.into_iter().collect::<Vec<_>>().join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project(consumer: &str) -> Value {
        json!({"projectVersion":"1.15.0", "timeline":{"layers":[{"genParams":{
            "params":{"5_near":{"value":0.001,"base":0.001},"10_roughness":{"value":0.247}},
            "graph":{"version":1,"name":"Depth test","nodes":[
                {"id":1,"typeId":"node.render_scene"},
                {"id":2,"typeId":consumer,"handle":"consumer"}
            ],"wires":[{"fromNode":1,"fromPort":"depth","toNode":2,"toPort":"depth"}]}
        }}]}})
    }

    #[test]
    fn built_in_depth_upgrade_preserves_all_authored_values() {
        super::super::take_migration_notes();
        let before = project("node.coc_from_depth");
        let after: Value = serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap()).unwrap();
        assert_eq!(after["timeline"], before["timeline"]);
        assert_eq!(after["projectVersion"], "1.16.0");
        assert!(super::super::take_migration_notes().is_empty());
        let second: Value = serde_json::from_str(&crate::migrate::migrate_if_needed(&after.to_string()).unwrap()).unwrap();
        assert_eq!(second, after);
    }

    #[test]
    fn custom_depth_math_is_preserved_and_reported() {
        super::super::take_migration_notes();
        let before = project("node.wgsl_compute");
        let after: Value = serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap()).unwrap();
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("consumer"));
        assert!(notes[0].contains("near=1"));
    }

    #[test]
    fn estimated_depth_without_camera_render_is_untouched() {
        super::super::take_migration_notes();
        let mut value = project("node.wgsl_compute");
        value["timeline"]["layers"][0]["genParams"]["graph"]["nodes"][0]["typeId"] = json!("node.depth_estimate_midas");
        audit(&value);
        assert!(super::super::take_migration_notes().is_empty());
    }
}
