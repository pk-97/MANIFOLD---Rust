//! v1.17.0 → v1.18.0: GPU FLIP Domain grew the Solve Level card
//! (`docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` section 11 (Solve Level)). A saved
//! generator layer carries its own graph snapshot and that snapshot is the
//! manifest authority, so a project saved before the card has no `solve_level`
//! card, binding, or domain→step wire, and the solver can't be driven below
//! level 0. This rung gives every stored graph with a `node.gpu_flip_domain`
//! and a `node.gpu_flip_step` node the three pieces the bundled def ships:
//! the wire `domain.solve_level → step.solve_level`, the card binding, and the
//! card itself. No node param is written: the bundled def has none either (the
//! card mirrors the node's manifest default, 0). Idempotent: each piece is added
//! only when missing, so a migrated graph is a byte-identical passthrough.
//!
//! Only top-level nodes are looked at; a domain or step inside a group is
//! reported and left alone (the bundled def and every saved layer so far keep
//! both at the top level).

use serde_json::{json, Value};

const DOMAIN: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const STEP: &str = "node.gpu_flip_step";
const PARAM: &str = "solve_level";

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
            "{migrated} GPU FLIP graph(s) gained the Solve Level card, binding and domain→step wire (v1.18.0)"
        ));
    }
}

/// The three pieces, verbatim from the bundled `WaterDamBreakGpuFlip` def.
pub(crate) fn wire(domain: &Value, step: &Value) -> Value {
    json!({"fromNode": domain, "fromPort": PARAM, "toNode": step, "toPort": PARAM})
}

pub(crate) fn binding(domain_node_id: &str) -> Value {
    json!({
        "convert": {"type": "IntRound"},
        "defaultMirrorsNodeParam": true,
        "defaultValue": 0.0,
        "id": PARAM,
        "label": "Solve Level",
        "target": {"kind": "node", "nodeId": domain_node_id, "param": PARAM}
    })
}

pub(crate) fn card() -> Value {
    json!({
        "defaultValue": 0.0,
        "formatString": "F0",
        "id": PARAM,
        "isToggle": false,
        "isTrigger": false,
        "max": 4.0,
        "min": 0.0,
        "name": "Solve Level",
        "section": "Fluid",
        "wholeNumbers": true
    })
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
    let (domain, step) = match (first_of_type(nodes, DOMAIN), first_of_type(nodes, STEP)) {
        (Some(d), Some(s)) => (d, s),
        _ => {
            if in_group(nodes, DOMAIN) || in_group(nodes, STEP) {
                let name = map.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph");
                super::note_migration(format!(
                    "{name}: GPU FLIP domain or step sits inside a group; the Solve Level card was not added. Re-add the generator from the preset to get it."
                ));
            }
            return false;
        }
    };
    let (domain_id, step_id) = (domain["id"].clone(), step["id"].clone());
    let Some(domain_node_id) = domain.get("nodeId").and_then(Value::as_str).map(str::to_owned) else {
        let name = map.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph");
        super::note_migration(format!("{name}: GPU FLIP domain has no nodeId; the Solve Level card was not added."));
        return false;
    };

    let mut changed = false;
    let wires = map.entry("wires").or_insert_with(|| json!([]));
    if let Some(wires) = wires.as_array_mut()
        && !wires.iter().any(|w| w["toNode"] == step_id && w["toPort"] == PARAM)
    {
        wires.push(wire(&domain_id, &step_id));
        changed = true;
    }

    let meta = map.entry("presetMetadata").or_insert_with(|| json!({}));
    let Value::Object(meta) = meta else { return changed };
    let bindings = meta.entry("bindings").or_insert_with(|| json!([]));
    if let Some(bindings) = bindings.as_array_mut()
        && !bindings.iter().any(|b| b["id"] == PARAM)
    {
        bindings.push(binding(&domain_node_id));
        changed = true;
    }
    let params = meta.entry("params").or_insert_with(|| json!([]));
    if let Some(params) = params.as_array_mut()
        && !params.iter().any(|p| p["id"] == PARAM)
    {
        params.push(card());
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLED: &str = include_str!("../../../manifold-renderer/assets/generator-presets/WaterDamBreakGpuFlip.json");
    const PETER_LAYER: &str = include_str!("../../tests/fixtures/water_layer_graph_v1160.json");

    fn migrate_project(before: &Value) -> Value {
        serde_json::from_str(&crate::migrate::migrate_if_needed(&before.to_string()).unwrap()).unwrap()
    }

    fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
        list.as_array().unwrap().iter().find(|v| v["id"] == id).unwrap()
    }

    #[test]
    fn injected_pieces_are_the_bundled_defs_verbatim() {
        let bundled: Value = serde_json::from_str(BUNDLED).unwrap();
        assert_eq!(binding("domain"), *find(&bundled["presetMetadata"]["bindings"], PARAM));
        assert_eq!(card(), *find(&bundled["presetMetadata"]["params"], PARAM));
        let typed: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(BUNDLED).unwrap();
        let family = manifold_core::effect_graph_def::find_node(&typed.nodes, "water_family")
            .expect("bundled Water family");
        let group = family.group.as_ref().expect("Water family body");
        let shipped = group.wires.iter()
            .find(|wire| wire.to_port == PARAM)
            .expect("bundled solve-level wire");
        let domain = manifold_core::effect_graph_def::find_node(&group.nodes, "domain").expect("bundled domain");
        let step = manifold_core::effect_graph_def::find_node(&group.nodes, "step").expect("bundled solver");
        assert_eq!(wire(&json!(domain.id), &json!(step.id)), serde_json::to_value(shipped).unwrap());
    }

    #[test]
    fn peters_layer_gains_the_card_the_binding_and_the_wire() {
        super::super::take_migration_notes();
        let graph: Value = serde_json::from_str(PETER_LAYER).unwrap();
        assert!(!graph["wires"].as_array().unwrap().iter().any(|w| w["toPort"] == PARAM));
        // From 1.17.0 so the density rung, which drops the body's mass, does not run.
        let before = json!({"projectVersion": "1.17.0", "timeline": {"layers": [{"genParams": {"params": null, "graph": graph}}]}});
        let after = migrate_project(&before);
        assert_eq!(after["projectVersion"], manifold_core::project::CURRENT_PROJECT_VERSION);
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        let wires = graph["wires"].as_array().unwrap();
        assert_eq!(wires.len(), 197 + 3, "this rung's wire, the Max Iterations rung's and the contacts rung's");
        assert!(wires.contains(&wire(&json!(0), &json!(6))));
        assert_eq!(*find(&graph["presetMetadata"]["bindings"], PARAM), binding("domain"));
        assert_eq!(*find(&graph["presetMetadata"]["params"], PARAM), card());
        assert_eq!(graph["nodes"], before["timeline"]["layers"][0]["genParams"]["graph"]["nodes"], "no node param is written");
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.starts_with("1 GPU FLIP graph(s) gained the Solve Level")), "{notes:?}");

        let second = migrate_project(&after);
        assert_eq!(second, after, "idempotent");
        assert!(super::super::take_migration_notes().is_empty());
    }

    #[test]
    fn the_bundled_def_and_graphs_without_flip_are_passthrough() {
        super::super::take_migration_notes();
        // This rung upgrades ungrouped pre-F1a saves; keep the bundled
        // contents in that shape while asserting a complete graph is untouched.
        let authored = serde_json::from_str(BUNDLED).unwrap();
        let flat = manifold_core::flatten::flatten_groups(&authored).unwrap();
        // Read the serialized fixture like a saved graph, including float parsing.
        let bundled: Value = serde_json::from_str(&serde_json::to_string(&flat).unwrap()).unwrap();
        let before = json!({"projectVersion": "1.16.0", "embeddedPresets": [{"def": bundled}],
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
        let before = json!({"projectVersion": "1.16.0", "timeline": {"layers": [{"genParams": {"graph": graph}}]}});
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.starts_with("Grouped: GPU FLIP domain or step sits inside a group; the Solve Level")), "{notes:?}");
    }
}
