//! v1.18.0 → v1.19.0: GPU FLIP Domain grew the Max Iterations card
//! (`docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md` section 7 (Control contract)). A
//! saved generator layer carries its own graph snapshot, and that snapshot is
//! the manifest authority, so a project saved before the card has no
//! `max_iterations` card, binding, or domain→step wire. This rung gives the
//! first top-level `node.gpu_flip_domain` / `node.gpu_flip_step` pair in each
//! stored graph the three pieces the bundled def ships: the wire
//! `domain.max_iterations → step.max_iterations`, the binding, and the card,
//! placed after Solve Level. The card mirrors the domain's param, default 900,
//! the cap the step already ran Auto within.
//!
//! The rung adds pieces only where the card then drives the step's cap and
//! that cap is unchanged. The wire overrides the step's own param, so it joins
//! only a domain carrying the step's effective cap (its saved value, else
//! 900); a saved step cap over an unset domain moves onto the domain. A
//! different saved domain cap, a cap wire from another node, or a binding
//! aimed at the step's own param leaves the graph alone, reported. A migrated
//! project solves exactly as before.
//!
//! Each piece is added only when missing: an existing domain→step wire, card
//! or binding with the id is kept, and migrating twice is a byte-identical
//! passthrough. A domain or step inside a group is reported and left alone.

use serde_json::{json, Value};

const DOMAIN: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const STEP: &str = "node.gpu_flip_step";
const PARAM: &str = "max_iterations";
const BESIDE: &str = "solve_level";
/// The step's and domain's `max_iterations` default (`MAX_ITERATIONS`).
const DEFAULT_CAP: i64 = 900;

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
            "{migrated} GPU FLIP graph(s) gained the missing Max Iterations card, binding or domain→step wire (v1.19.0)"
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

/// A node's saved `max_iterations` param, rounded, when it has one.
fn authored(node: &Value) -> Option<i64> {
    let value = &node["params"][PARAM]["value"];
    value.as_i64().or_else(|| value.as_f64().filter(|v| v.is_finite()).map(|v| v.round() as i64))
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
    let step_cap = authored(step);
    let domain_cap = authored(domain);
    let step_node_id = step.get("nodeId").and_then(Value::as_str).map(str::to_owned);
    let graph_name = name();
    let has_card = map
        .get("presetMetadata")
        .and_then(|m| m.get("params"))
        .and_then(Value::as_array)
        .is_some_and(|params| params.iter().any(|p| p["id"] == PARAM));
    let skip = |why: String| {
        if !has_card {
            super::note_migration(format!(
                "{graph_name}: {why}; the Max Iterations card was not added. Re-add the generator from the preset to get it."
            ));
        }
        false
    };

    let mut changed = false;
    let cap_wire = map
        .get("wires")
        .and_then(Value::as_array)
        .and_then(|wires| wires.iter().find(|w| w["toNode"] == step_id && w["toPort"] == PARAM))
        .map(|w| (w["fromNode"].clone(), w["fromPort"].clone()));
    match cap_wire {
        Some((from, port)) if from == domain_id && port == PARAM => {}
        Some(_) => return skip("the GPU FLIP step's Max Iterations is wired from another node, which a card on the domain could not drive".into()),
        None => {
            // The wire overrides the step's own param, so it may only join a
            // domain that already carries the step's effective cap, and never
            // where a card drives the step's param directly.
            let binds_step = step_node_id.is_some_and(|id| {
                map.get("presetMetadata")
                    .and_then(|m| m.get("bindings"))
                    .and_then(Value::as_array)
                    .is_some_and(|bindings| {
                        bindings.iter().any(|b| b["target"]["nodeId"] == id.as_str() && b["target"]["param"] == PARAM)
                    })
            });
            if binds_step {
                return skip("a card already drives the GPU FLIP step's Max Iterations, and the domain→step wire would override it".into());
            }
            let step_effective = step_cap.unwrap_or(DEFAULT_CAP);
            match domain_cap {
                Some(domain) if domain != step_effective => {
                    return skip(format!(
                        "the GPU FLIP step's Max Iterations ({step_effective}) differs from the domain's ({domain}), so the step keeps solving at {step_effective}"
                    ));
                }
                Some(_) => {}
                None => {
                    // An authored step cap moves onto the domain, where the
                    // card shows it.
                    if let Some(cap) = step_cap
                        && let Some(domain) = map.get_mut("nodes").and_then(Value::as_array_mut).and_then(|nodes| nodes.iter_mut().find(|n| n["id"] == domain_id))
                    {
                        let params = domain.as_object_mut().expect("a node is an object").entry("params").or_insert_with(|| json!({}));
                        params[PARAM] = json!({"type": "Int", "value": cap});
                    }
                }
            }
            let wires = map.entry("wires").or_insert_with(|| json!([]));
            if let Some(wires) = wires.as_array_mut() {
                wires.push(wire(&domain_id, &step_id));
                changed = true;
            }
        }
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
        let mut graph = root["timeline"]["layers"][0]["genParams"]["graph"].take();
        // The later contacts rung's wire, so these tests see only this rung's edits.
        graph["wires"].as_array_mut().unwrap().push(super::super::contacts_wire_v1200::wire(&json!(0), &json!(6)));
        graph
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
        assert_eq!(after["projectVersion"], manifold_core::project::CURRENT_PROJECT_VERSION);
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
        assert!(notes[0].starts_with("1 GPU FLIP graph(s) gained the missing Max Iterations"));

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
        assert!(notes.iter().any(|n| n.starts_with("Grouped: GPU FLIP domain or step sits inside a group; the Max Iterations")), "{notes:?}");
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

    fn node<'a>(graph: &'a Value, type_id: &str) -> &'a Value {
        graph["nodes"].as_array().unwrap().iter().find(|n| n["typeId"] == type_id).unwrap()
    }

    fn with_step_cap(cap: i64, domain_cap: Option<i64>) -> Value {
        let mut graph = layer_at_1180();
        for n in graph["nodes"].as_array_mut().unwrap() {
            if n["typeId"] == STEP {
                n["params"][PARAM] = json!({"type": "Int", "value": cap});
            }
            if n["typeId"] == DOMAIN && let Some(d) = domain_cap {
                n["params"][PARAM] = json!({"type": "Int", "value": d});
            }
        }
        graph
    }

    /// A step with an authored cap of 120 and no incoming cap wire keeps
    /// solving at 120: the value moves onto the domain, which the new wire
    /// and the card both carry.
    #[test]
    fn an_authored_step_cap_stays_effective() {
        super::super::take_migration_notes();
        let before = project(with_step_cap(120, None));
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert_eq!(node(graph, DOMAIN)["params"][PARAM], json!({"type": "Int", "value": 120}), "the domain carries the step's cap");
        assert_eq!(node(graph, STEP)["params"][PARAM]["value"], 120, "the step's own param is left as saved");
        assert!(graph["wires"].as_array().unwrap().contains(&wire(&json!(0), &json!(6))));
        let card = find(&graph["presetMetadata"]["bindings"], PARAM);
        assert_eq!(card["defaultMirrorsNodeParam"], true, "the card shows the domain's 120");
        assert_eq!(migrate_project(&after), after, "twice is a no-op");
    }

    /// When the domain already carries a different cap, the wire would
    /// override the step's and a domain-bound card could not drive it: the
    /// graph is left as saved and the rung says so.
    #[test]
    fn a_conflicting_domain_cap_leaves_the_graph_alone() {
        super::super::take_migration_notes();
        let before = project(with_step_cap(120, Some(300)));
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"], "no wire, card or binding added");
        let notes = super::super::take_migration_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("step's Max Iterations (120) differs from the domain's (300), so the step keeps solving at 120; the Max Iterations card was not added"), "{notes:?}");
    }

    /// A step on its default 900 runs Auto within 900; a domain saved at 300
    /// would lower that through the wire, so the graph is left alone.
    #[test]
    fn a_default_step_cap_conflicts_with_a_lower_domain_cap() {
        let mut graph = layer_at_1180();
        for n in graph["nodes"].as_array_mut().unwrap() {
            if n["typeId"] == DOMAIN {
                n["params"][PARAM] = json!({"type": "Int", "value": 300});
            }
        }
        let before = project(graph);
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.contains("step's Max Iterations (900) differs from the domain's (300)")), "{notes:?}");
    }

    /// A cap wire from another node keeps driving the step; a card on the
    /// domain could not, so none is added.
    #[test]
    fn a_cap_wire_from_another_node_gets_no_domain_card() {
        let mut graph = layer_at_1180();
        let step_id = node(&graph, STEP)["id"].clone();
        graph["wires"].as_array_mut().unwrap().push(json!({"fromNode": 99, "fromPort": "value", "toNode": step_id, "toPort": PARAM}));
        let before = project(graph);
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.contains("wired from another node")), "{notes:?}");
    }

    /// A binding aimed at the step's own param would be disabled by the
    /// domain→step wire, so the graph is left alone.
    #[test]
    fn a_binding_on_the_step_param_blocks_the_wire() {
        let mut graph = layer_at_1180();
        let step_node_id = node(&graph, STEP)["nodeId"].as_str().unwrap().to_owned();
        graph["presetMetadata"]["bindings"].as_array_mut().unwrap().push(json!({
            "id": "solver_cap", "label": "Solver Cap", "target": {"kind": "node", "nodeId": step_node_id, "param": PARAM}}));
        let before = project(graph);
        super::super::take_migration_notes();
        let after = migrate_project(&before);
        assert_eq!(after["timeline"], before["timeline"]);
        let notes = super::super::take_migration_notes();
        assert!(notes.iter().any(|n| n.contains("a card already drives the GPU FLIP step's Max Iterations")), "{notes:?}");
    }

    /// A step cap equal to the domain's is wired without any change to either.
    #[test]
    fn a_matching_domain_cap_is_wired() {
        let before = project(with_step_cap(120, Some(120)));
        let after = migrate_project(&before);
        let graph = &after["timeline"]["layers"][0]["genParams"]["graph"];
        assert!(graph["wires"].as_array().unwrap().contains(&wire(&json!(0), &json!(6))));
        assert_eq!(graph["nodes"], before["timeline"]["layers"][0]["genParams"]["graph"]["nodes"]);
    }
}
