//! v1.18.0 → v1.19.0: GPU FLIP Domain grew the Max Iterations card
//! (`docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md` section 7 (Control contract)). A
//! saved generator layer carries its own graph snapshot, and that snapshot is
//! the manifest authority, so a project saved before the card has no
//! `max_iterations` card, binding, or domain→step wire. This rung gives the
//! first top-level `node.gpu_flip_domain` / `node.gpu_flip_step` pair in each
//! stored graph the three pieces the bundled def ships: the wire
//! `domain.max_iterations → step.max_iterations`, the binding, and the card,
//! placed after Solve Level. No node param is written: the card mirrors the
//! node's manifest default, 900, which is the cap the step already ran Auto
//! within, so a migrated project solves exactly as before.
//!
//! Each piece is added only when missing: an authored wire into
//! `step.max_iterations`, or an existing card or binding with the id, is kept,
//! and migrating twice is a byte-identical passthrough. A domain or step
//! inside a group is reported and left alone.

use serde_json::{json, Value};

const DOMAIN: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const STEP: &str = "node.gpu_flip_step";
const PARAM: &str = "max_iterations";
const BESIDE: &str = "solve_level";

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
            "{migrated} GPU FLIP graph(s) gained the Max Iterations card, binding and domain→step wire (v1.19.0)"
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
        "defaultValue": 900.0,
        "id": PARAM,
        "label": "Max Iterations",
        "target": {"kind": "node", "nodeId": domain_node_id, "param": PARAM}
    })
}

pub(crate) fn card() -> Value {
    json!({
        "defaultValue": 900.0,
        "formatString": "F0",
        "id": PARAM,
        "isToggle": false,
        "isTrigger": false,
        "max": 900.0,
        "min": 1.0,
        "name": "Max Iterations",
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

/// Adds `entry` after the Solve Level entry, or last when there is none.
/// Returns true when it was missing.
fn add_beside(list: &mut Vec<Value>, entry: Value) -> bool {
    if list.iter().any(|e| e["id"] == PARAM) {
        return false;
    }
    let at = list.iter().position(|e| e["id"] == BESIDE).map_or(list.len(), |i| i + 1);
    list.insert(at, entry);
    true
}

/// Migrate one graph-shaped value. Returns true when it was modified.
fn migrate_graph_value(graph: &mut Value) -> bool {
    let Value::Object(map) = graph else { return false };
    let Some(nodes) = map.get("nodes").and_then(Value::as_array) else { return false };
    let name = || map.get("name").and_then(Value::as_str).unwrap_or("Unnamed graph").to_owned();
    let (domain, step) = match (first_of_type(nodes, DOMAIN), first_of_type(nodes, STEP)) {
        (Some(d), Some(s)) => (d, s),
        _ => {
            if in_group(nodes, DOMAIN) || in_group(nodes, STEP) {
                super::note_migration(format!(
                    "{}: GPU FLIP domain or step sits inside a group; the Max Iterations card was not added. Re-add the generator from the preset to get it.",
                    name()
                ));
            }
            return false;
        }
    };
    let (domain_id, step_id) = (domain["id"].clone(), step["id"].clone());
    let Some(domain_node_id) = domain.get("nodeId").and_then(Value::as_str).map(str::to_owned) else {
        super::note_migration(format!("{}: GPU FLIP domain has no nodeId; the Max Iterations card was not added.", name()));
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
    if let Some(bindings) = meta.entry("bindings").or_insert_with(|| json!([])).as_array_mut() {
        changed |= add_beside(bindings, binding(&domain_node_id));
    }
    if let Some(params) = meta.entry("params").or_insert_with(|| json!([])).as_array_mut() {
        changed |= add_beside(params, card());
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

    fn ids(list: &Value) -> Vec<String> {
        list.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect()
    }

    /// Peter's layer at 1.18.0: the Solve Level rung already ran on it.
    fn layer_at_1180() -> Value {
        let graph: Value = serde_json::from_str(PETER_LAYER).unwrap();
        let mut root = json!({"timeline": {"layers": [{"genParams": {"params": null, "graph": graph}}]}});
        super::super::solve_level_card_v1180::migrate(&mut root);
        root["timeline"]["layers"][0]["genParams"]["graph"].take()
    }

    fn project(graph: Value) -> Value {
        json!({"projectVersion": "1.18.0", "timeline": {"layers": [{"genParams": {"params": null, "graph": graph}}]}})
    }

    #[test]
    fn injected_pieces_are_the_bundled_defs_verbatim() {
        let bundled: Value = serde_json::from_str(BUNDLED).unwrap();
        assert_eq!(binding("domain"), *find(&bundled["presetMetadata"]["bindings"], PARAM));
        assert_eq!(card(), *find(&bundled["presetMetadata"]["params"], PARAM));
        let shipped = bundled["wires"].as_array().unwrap().iter().find(|w| w["toPort"] == PARAM).unwrap();
        assert_eq!(wire(&json!(0), &json!(6)), *shipped);
        let params = ids(&bundled["presetMetadata"]["params"]);
        let at = params.iter().position(|id| id == BESIDE).unwrap();
        assert_eq!(params[at + 1], PARAM, "the card sits after Solve Level");
    }

    #[test]
    fn peters_layer_gains_the_card_the_binding_and_the_wire() {
        super::super::take_migration_notes();
        let graph = layer_at_1180();
        let wires_before = graph["wires"].as_array().unwrap().len();
        let before = project(graph);
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["projectVersion"], "1.19.0");
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        let wires = graph["wires"].as_array().unwrap();
        assert_eq!(wires.len(), wires_before + 1);
        assert_eq!(*wires.last().unwrap(), wire(&json!(0), &json!(6)));
        assert_eq!(*find(&graph["presetMetadata"]["bindings"], PARAM), binding("domain"));
        assert_eq!(*find(&graph["presetMetadata"]["params"], PARAM), card());
        let params = ids(&graph["presetMetadata"]["params"]);
        assert_eq!(params[params.iter().position(|id| id == BESIDE).unwrap() + 1], PARAM);
        assert_eq!(graph["nodes"], before["timeline"]["layers"][0]["genParams"]["graph"]["nodes"], "no node param is written");
        let notes = super::super::take_migration_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("1 GPU FLIP graph(s) gained the Max Iterations"));

        let second = migrate_project(&after);
        assert_eq!(second, after, "idempotent");
        assert!(super::super::take_migration_notes().is_empty());
    }

    #[test]
    fn the_bundled_def_and_graphs_without_flip_are_passthrough() {
        super::super::take_migration_notes();
        let bundled: Value = serde_json::from_str(BUNDLED).unwrap();
        let before = json!({"projectVersion": "1.18.0", "embeddedPresets": [{"def": bundled}],
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
        let before = project(graph);
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("Grouped: GPU FLIP domain or step sits inside a group; the Max Iterations"));
    }

    /// An authored wire into the step's input and an existing card and
    /// binding are kept as they are; nothing duplicates them.
    #[test]
    fn authored_wires_cards_and_bindings_are_preserved() {
        super::super::take_migration_notes();
        let mut graph = layer_at_1180();
        let authored = json!({"fromNode": 99, "fromPort": "value", "toNode": 6, "toPort": PARAM});
        graph["wires"].as_array_mut().unwrap().push(authored.clone());
        let custom_card = json!({"id": PARAM, "name": "Solver Cap", "min": 1.0, "max": 200.0, "defaultValue": 50.0, "section": "Mine"});
        let custom_binding = json!({"id": PARAM, "label": "Solver Cap", "target": {"kind": "node", "nodeId": "domain", "param": PARAM}});
        graph["presetMetadata"]["params"].as_array_mut().unwrap().push(custom_card.clone());
        graph["presetMetadata"]["bindings"].as_array_mut().unwrap().push(custom_binding.clone());
        let before = project(graph);
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"], "every piece was already there");
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert_eq!(graph["wires"].as_array().unwrap().iter().filter(|w| w["toPort"] == PARAM).count(), 1);
        assert_eq!(*find(&graph["presetMetadata"]["params"], PARAM), custom_card);
        assert_eq!(*find(&graph["presetMetadata"]["bindings"], PARAM), custom_binding);
        assert!(super::super::take_migration_notes().is_empty());
    }

    /// A saved fixed Iterations count, saved node params on the domain, and
    /// card overrides on the layer survive: the rung only adds.
    #[test]
    fn fixed_iterations_params_and_overrides_are_preserved() {
        super::super::take_migration_notes();
        let mut graph = layer_at_1180();
        let nodes = graph["nodes"].as_array_mut().unwrap();
        let step = nodes.iter_mut().find(|n| n["typeId"] == STEP).unwrap();
        step["params"]["iterations"] = json!({"value": 24});
        let domain = nodes.iter_mut().find(|n| n["typeId"] == DOMAIN).unwrap();
        domain["params"]["max_iterations"] = json!({"value": 300});
        let nodes_before = graph["nodes"].clone();
        let overrides = json!({"resolution": 96.0, "solve_level": 1.0, "max_iterations": 300.0});
        let before = json!({"projectVersion": "1.18.0", "timeline": {"layers": [{"genParams": {"params": overrides, "graph": graph}}]}});
        let after = migrate_project(&before);
        let layer = &after["timeline"]["layers"][0]["genParams"];
        assert_eq!(layer["params"], overrides, "card overrides kept");
        assert_eq!(layer["graph"]["nodes"], nodes_before, "node params, the fixed count among them, kept");
    }

    /// A graph with only some of the pieces gets the rest.
    #[test]
    fn a_partially_migrated_graph_is_completed() {
        super::super::take_migration_notes();
        let mut graph = layer_at_1180();
        graph["wires"].as_array_mut().unwrap().push(wire(&json!(0), &json!(6)));
        let before = project(graph);
        let after = migrate_project(&before);
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert_eq!(graph["wires"].as_array().unwrap().iter().filter(|w| w["toPort"] == PARAM).count(), 1);
        assert_eq!(*find(&graph["presetMetadata"]["bindings"], PARAM), binding("domain"));
        assert_eq!(*find(&graph["presetMetadata"]["params"], PARAM), card());
        assert_eq!(migrate_project(&after), after, "twice is a no-op");
    }
}
