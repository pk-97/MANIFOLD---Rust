//! v1.19.0 → v1.20.0: GPU FLIP Step reads the rigid bodies' touching contacts
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 2.1 (Body handoff amendment),
//! D16). A saved generator layer carries its own graph snapshot, so a project
//! saved before the port has no `domain.contacts → step.contacts` wire. Without
//! it the step sees no supports, treats a box resting on the floor as free to
//! sink, and the water drains under it (BUG-beblk (water drains under a box3d)).
//! This rung gives the first top-level `node.gpu_flip_domain` /
//! `node.gpu_flip_step` pair in each stored graph the wire the bundled def
//! ships. Idempotent: a step whose `contacts` input is already wired is left
//! alone. A domain or step inside a group is reported and left alone.

use serde_json::{json, Value};

const DOMAIN: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const STEP: &str = "node.gpu_flip_step";
const PORT: &str = "contacts";

pub(crate) fn migrate(root: &mut Value) {
    let mut migrated = 0usize;
    crate::migrate::for_each_preset_instance(root, |fx| {
        let Value::Object(map) = fx else { return };
        if let Some(graph) = map.get_mut("graph") {
            migrated += usize::from(migrate_graph_value(graph));
        }
    });
    if let Some(presets) = root.get_mut("embeddedPresets").and_then(|v| v.as_array_mut()) {
        for preset in presets.iter_mut() {
            if let Some(def) = preset.get_mut("def") {
                migrated += usize::from(migrate_graph_value(def));
            }
        }
    }
    if migrated > 0 {
        super::note_migration(format!(
            "{migrated} GPU FLIP graph(s) gained the domain→step contacts wire, so water holds under resting bodies (v1.20.0)"
        ));
    }
}

/// The wire, verbatim from the bundled `WaterDamBreakGpuFlip` def.
pub(crate) fn wire(domain: &Value, step: &Value) -> Value {
    json!({"fromNode": domain, "fromPort": PORT, "toNode": step, "toPort": PORT})
}

fn first_of_type<'a>(nodes: &'a [Value], type_id: &str) -> Option<&'a Value> {
    nodes.iter().find(|n| n.get("typeId").and_then(Value::as_str) == Some(type_id))
}

fn in_group(nodes: &[Value], type_id: &str) -> bool {
    nodes.iter().any(|n| {
        n.get("group")
            .and_then(|g| g.get("nodes"))
            .and_then(Value::as_array)
            .is_some_and(|inner| first_of_type(inner, type_id).is_some() || in_group(inner, type_id))
    })
}

/// Migrate one graph-shaped value. Returns true when it was modified.
fn migrate_graph_value(graph: &mut Value) -> bool {
    let Value::Object(map) = graph else { return false };
    let Some(nodes) = map.get("nodes").and_then(Value::as_array) else { return false };
    let (domain_id, step_id) = match (first_of_type(nodes, DOMAIN), first_of_type(nodes, STEP)) {
        (Some(d), Some(s)) => (d["id"].clone(), s["id"].clone()),
        _ => {
            if in_group(nodes, DOMAIN) || in_group(nodes, STEP) {
                let name = map.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph");
                super::note_migration(format!(
                    "{name}: GPU FLIP domain or step sits inside a group; the contacts wire was not added, so water can drain under resting bodies. Re-add the generator from the preset to get it."
                ));
            }
            return false;
        }
    };
    let wires = map.entry("wires").or_insert_with(|| json!([]));
    let Some(wires) = wires.as_array_mut() else { return false };
    if wires.iter().any(|w| w["toNode"] == step_id && w["toPort"] == PORT) {
        return false;
    }
    wires.push(wire(&domain_id, &step_id));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLED: &str = include_str!("../../../manifold-renderer/assets/generator-presets/WaterDamBreakGpuFlip.json");
    const PARTICLES: &str = include_str!("../../../manifold-renderer/assets/generator-presets/WaterDamBreakParticles.json");
    const PETER_LAYER: &str = include_str!("../../tests/fixtures/water_layer_graph_v1160.json");

    fn migrate_project(before: &Value) -> Value {
        serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap()).unwrap()
    }

    fn contacts_wires(graph: &Value) -> Vec<&Value> {
        graph["wires"].as_array().unwrap().iter().filter(|w| w["toPort"] == PORT).collect()
    }

    /// A bundled def in the shape this rung upgrades: ungrouped, read back
    /// like a saved graph.
    fn flat_bundled(def: &str) -> Value {
        let flat = manifold_core::flatten::flatten_groups(&serde_json::from_str(def).unwrap()).unwrap();
        serde_json::from_str(&serde_json::to_string(&flat).unwrap()).unwrap()
    }

    #[test]
    fn the_wire_is_the_bundled_defs_verbatim() {
        for def in [BUNDLED, PARTICLES] {
            let bundled = flat_bundled(def);
            let nodes = bundled["nodes"].as_array().unwrap();
            let domain = first_of_type(nodes, DOMAIN).expect("bundled domain");
            let step = first_of_type(nodes, STEP).expect("bundled step");
            assert_eq!(contacts_wires(&bundled), [&wire(&domain["id"], &step["id"])]);
        }
    }

    #[test]
    fn peters_layer_gains_the_wire() {
        super::super::take_migration_notes();
        let graph: Value = serde_json::from_str(PETER_LAYER).unwrap();
        assert!(contacts_wires(&graph).is_empty());
        let before = json!({"projectVersion": "1.19.0", "timeline": {"layers": [{"genParams": {"params": null, "graph": graph}}]}});
        let after = migrate_project(&before);
        assert_eq!(after["projectVersion"], "1.20.0");
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert_eq!(contacts_wires(graph), [&wire(&json!(0), &json!(6))]);
        assert_eq!(graph["nodes"], before["timeline"]["layers"][0]["genParams"]["graph"]["nodes"]);
        let notes = super::super::take_migration_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("1 GPU FLIP graph(s) gained the domain→step contacts wire"));

        let second = migrate_project(&after);
        assert_eq!(second, after, "idempotent");
        assert!(super::super::take_migration_notes().is_empty());
    }

    #[test]
    fn the_bundled_def_and_graphs_without_flip_are_passthrough() {
        super::super::take_migration_notes();
        let before = json!({"projectVersion": "1.19.0",
            "embeddedPresets": [{"def": flat_bundled(BUNDLED)}, {"def": flat_bundled(PARTICLES)}],
            "timeline": {"layers": [{"genParams": {"graph": {"version": 3, "nodes": [{"id": 1, "typeId": "node.value"}], "wires": []}}}]}});
        let after = migrate_project(&before);
        assert_eq!(after["embeddedPresets"], before["embeddedPresets"]);
        assert_eq!(after["timeline"], before["timeline"]);
        assert!(super::super::take_migration_notes().is_empty());
    }

    #[test]
    fn a_grouped_domain_is_reported_not_edited() {
        super::super::take_migration_notes();
        let graph = json!({"name": "Grouped", "nodes": [{"id": 1, "typeId": "group", "group": {"nodes": [
            {"id": 0, "nodeId": "domain", "typeId": DOMAIN}, {"id": 6, "nodeId": "step", "typeId": STEP}], "wires": []}}], "wires": []});
        let before = json!({"projectVersion": "1.19.0", "timeline": {"layers": [{"genParams": {"graph": graph}}]}});
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.starts_with("Grouped: GPU FLIP domain or step sits inside a group; the contacts wire")), "{notes:?}");
    }
}
