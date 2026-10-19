//! The physics box spawner lives on the copies body. A `node.rigid_body`
//! wired into a `node.physics_world`'s `copies` input is the prototype the
//! world copies, and it carries the copy count, spacing, columns and
//! arrangement. Those controls show only on a copies body, so a scene with
//! no boxes has no box sliders, and any scene gets boxes by wiring a body
//! into `copies`. This module names that role for the scene stamp and moves
//! the controls off the world for graphs saved before they lived here.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire};

pub const PHYSICS_WORLD_TYPE_ID: &str = "node.physics_world";
pub const RIGID_BODY_TYPE_ID: &str = "node.rigid_body";
/// The world input whose body is copied.
pub const COPIES_PORT: &str = "copies";
const BODY_OUTPUT: &str = "body";
/// The spawner's params, on the copies body today and on the world before.
/// Each is also a scalar input of the same name.
pub const COPY_PARAMS: [&str; 4] = ["copy_count", "copy_spacing", "copy_columns", "copy_layout"];

/// Whether `param` of `type_id` is a spawner control, stamped only on a
/// copies body.
pub fn is_copies_body_param(type_id: &str, param: &str) -> bool {
    type_id == RIGID_BODY_TYPE_ID && COPY_PARAMS.contains(&param)
}

/// Whether the node with doc id `node` feeds a Physics World's `copies`
/// input in this graph level.
pub fn is_copies_body(node: u32, nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> bool {
    wires.iter().any(|w| {
        w.from_node == node
            && w.from_port == BODY_OUTPUT
            && w.to_port == COPIES_PORT
            && nodes.iter().any(|n| n.id == w.to_node && n.type_id == PHYSICS_WORLD_TYPE_ID)
    })
}

/// What [`migrate_world_copies`] did to one graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldCopiesMigration {
    /// The graph carried spawner controls on a Physics World and lost them.
    pub changed: bool,
    /// A world's controls moved onto its copies body.
    pub moved: bool,
    /// A world carried controls but copies no body: they did nothing and
    /// are gone.
    pub no_copies_body: bool,
}

/// Whether `def` still carries spawner controls on a Physics World: a
/// stored or exposed copy param, a wire into a copy input, or a card that
/// drives one. Lets the common load borrow the document instead of cloning.
pub fn carries_world_copies(def: &EffectGraphDef) -> bool {
    if any_world_copies(&def.nodes, &def.wires) {
        return true;
    }
    let mut worlds = BTreeSet::new();
    collect_worlds(&def.nodes, &mut worlds);
    !worlds.is_empty()
        && def
            .preset_metadata
            .as_ref()
            .is_some_and(|m| m.bindings.iter().any(|b| world_copy_target(&worlds, &b.target)))
}

/// Move every Physics World's spawner controls onto the body wired into its
/// `copies` input: stored values, exposure, wires into the copy inputs and
/// the cards that drive them, keeping each card's id so a saved card value
/// still reaches it. Whatever the body already has wins; nothing on the
/// body is overwritten. A world that copies no body loses its controls,
/// which never did anything. Runs only on a graph that still carries them,
/// so it is idempotent.
pub fn migrate_world_copies(def: &mut EffectGraphDef) -> WorldCopiesMigration {
    if !carries_world_copies(def) {
        return WorldCopiesMigration::default();
    }
    let mut report = WorldCopiesMigration { changed: true, ..WorldCopiesMigration::default() };
    // World stable id → its copies body's stable id, for the cards.
    let mut bodies = BTreeMap::new();
    migrate_level(&mut def.nodes, &mut def.wires, &mut bodies, &mut report);

    let Some(meta) = def.preset_metadata.as_mut() else { return report };
    let mut bound: BTreeSet<(String, String)> = meta
        .bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::Node { node_id, param } => Some((node_id.to_string(), param.clone())),
            _ => None,
        })
        .collect();
    let mut dead = vec![false; meta.bindings.len()];
    for (index, binding) in meta.bindings.iter_mut().enumerate() {
        let BindingTarget::Node { node_id, param } = &mut binding.target else { continue };
        let Some(body) = bodies.get(node_id.as_str()) else { continue };
        if !COPY_PARAMS.contains(&param.as_str()) {
            continue;
        }
        match body {
            Some(body) if bound.insert((body.to_string(), param.clone())) => {
                *node_id = body.clone();
                report.moved = true;
            }
            Some(_) => dead[index] = true,
            None => {
                dead[index] = true;
                report.no_copies_body = true;
            }
        }
    }
    let mut dropped = BTreeSet::new();
    let mut index = 0;
    meta.bindings.retain(|b| {
        let keep = !dead[index];
        index += 1;
        if !keep {
            dropped.insert(b.id.clone());
        }
        keep
    });
    // A card that still drives something else stays.
    dropped.retain(|id| !meta.bindings.iter().any(|b| &b.id == id));
    meta.params.retain(|p| !dropped.contains(&p.id));
    meta.param_aliases
        .retain(|a| !dropped.contains(&a.old) && a.new.as_ref().is_none_or(|id| !dropped.contains(id)));
    meta.value_aliases.retain(|a| !dropped.contains(&a.param_id));
    report
}

fn migrate_level(
    nodes: &mut [EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    bodies: &mut BTreeMap<String, Option<crate::NodeId>>,
    report: &mut WorldCopiesMigration,
) {
    let worlds: Vec<u32> = nodes.iter().filter(|n| n.type_id == PHYSICS_WORLD_TYPE_ID).map(|n| n.id).collect();
    for world in worlds {
        let body = wires
            .iter()
            .find(|w| w.to_node == world && w.to_port == COPIES_PORT)
            .map(|w| w.from_node)
            .filter(|id| nodes.iter().any(|n| n.id == *id && n.type_id == RIGID_BODY_TYPE_ID));
        let Some(world_index) = nodes.iter().position(|n| n.id == world) else { continue };
        let mut values = Vec::new();
        let mut exposed = Vec::new();
        {
            let world_node = &mut nodes[world_index];
            for param in COPY_PARAMS {
                if let Some(value) = world_node.params.remove(param) {
                    values.push((param, value));
                }
                if world_node.exposed_params.remove(param) {
                    exposed.push(param);
                }
            }
        }
        let had_wires = wires.iter().any(|w| w.to_node == world && COPY_PARAMS.contains(&w.to_port.as_str()));
        let had_state = !values.is_empty() || !exposed.is_empty() || had_wires;
        let world_id = nodes[world_index].node_id.to_string();
        match body {
            Some(body) => {
                let body_node = nodes.iter_mut().find(|n| n.id == body).expect("body found above");
                for (param, value) in values {
                    body_node.params.entry(param.to_string()).or_insert(value);
                }
                for param in exposed {
                    body_node.exposed_params.insert(param.to_string());
                }
                let body_id = body_node.node_id.clone();
                let wired: BTreeSet<String> =
                    wires.iter().filter(|w| w.to_node == body).map(|w| w.to_port.clone()).collect();
                wires.retain_mut(|w| {
                    if w.to_node != world || !COPY_PARAMS.contains(&w.to_port.as_str()) {
                        return true;
                    }
                    if wired.contains(&w.to_port) {
                        return false;
                    }
                    w.to_node = body;
                    true
                });
                report.moved |= had_state;
                bodies.insert(world_id, Some(body_id));
            }
            None => {
                wires.retain(|w| w.to_node != world || !COPY_PARAMS.contains(&w.to_port.as_str()));
                report.no_copies_body |= had_state;
                bodies.insert(world_id, None);
            }
        }
    }
    for node in nodes {
        if let Some(group) = node.group.as_mut() {
            let group = &mut **group;
            migrate_level(&mut group.nodes, &mut group.wires, bodies, report);
        }
    }
}

fn world_copy_target(worlds: &BTreeSet<String>, target: &BindingTarget) -> bool {
    matches!(target, BindingTarget::Node { node_id, param }
        if worlds.contains(node_id.as_str()) && COPY_PARAMS.contains(&param.as_str()))
}

fn collect_worlds(nodes: &[EffectGraphNode], worlds: &mut BTreeSet<String>) {
    for node in nodes {
        if node.type_id == PHYSICS_WORLD_TYPE_ID && !node.node_id.as_str().is_empty() {
            worlds.insert(node.node_id.to_string());
        }
        if let Some(group) = &node.group {
            collect_worlds(&group.nodes, worlds);
        }
    }
}

fn any_world_copies(nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> bool {
    let is_world = |id: u32| nodes.iter().any(|n| n.id == id && n.type_id == PHYSICS_WORLD_TYPE_ID);
    nodes.iter().any(|n| {
        (n.type_id == PHYSICS_WORLD_TYPE_ID
            && COPY_PARAMS.iter().any(|p| n.params.contains_key(*p) || n.exposed_params.contains(*p)))
            || n.group.as_ref().is_some_and(|g| any_world_copies(&g.nodes, &g.wires))
    }) || wires.iter().any(|w| COPY_PARAMS.contains(&w.to_port.as_str()) && is_world(w.to_node))
}

impl crate::project::Project {
    /// The physics box spawner (Count, Spacing, Columns, Arrangement) used
    /// to sit on the Physics World and did nothing without a copies body; it
    /// now lives on the body wired into the world's `copies` input
    /// (`copies_body::migrate_world_copies`). Every saved graph moves the
    /// controls there, cards keeping their ids so saved card values still
    /// land, and a show that had boxes keeps the same boxes. A world with no
    /// copies body loses controls that never did anything. Idempotent: a
    /// migrated world carries none.
    pub(crate) fn migrate_world_copies_to_body(&mut self) {
        use std::collections::BTreeSet;

        use crate::copies_body::{WorldCopiesMigration, migrate_world_copies};
        use crate::effect_graph_def::EffectGraphDef;
        use crate::effects::PresetInstance;

        fn note(def: &EffectGraphDef, outcome: WorldCopiesMigration, notes: &mut BTreeSet<String>) {
            let name = def
                .preset_metadata
                .as_ref()
                .map(|m| m.display_name.as_str())
                .filter(|n| !n.is_empty())
                .or(def.name.as_deref())
                .unwrap_or("a scene");
            if outcome.moved {
                notes.insert(format!(
                    "Physics boxes: \"{name}\" box Count, Spacing, Columns and Arrangement moved \
                     from the Physics node onto its copies body."
                ));
            }
            if outcome.no_copies_body {
                notes.insert(format!(
                    "Physics boxes: \"{name}\" had box controls on its Physics node and no copies \
                     body; they did nothing and are gone."
                ));
            }
        }
        fn migrate_instance(fx: &mut PresetInstance, notes: &mut BTreeSet<String>) {
            let Some(mut def) = fx.graph.take() else { return };
            let outcome = migrate_world_copies(&mut def);
            note(&def, outcome, notes);
            fx.graph = Some(def);
            if outcome.changed {
                fx.refresh_manifest_from_graph();
            }
        }

        let mut notes = BTreeSet::new();
        for preset in &mut self.embedded_presets {
            let outcome = migrate_world_copies(&mut preset.def);
            note(&preset.def, outcome, &mut notes);
        }
        self.for_each_preset_instance_mut(|fx| migrate_instance(fx, &mut notes));
        for line in notes {
            eprintln!("[Migration] {line}");
            self.load_report.migration_notes.push(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::SerializedParamValue;
    use crate::preset_def::PresetKind;
    use crate::project::{EmbeddedOrigin, EmbeddedPreset, Project};

    /// A Physics Boxes graph from before the spawner moved: the four
    /// controls stored on the world, Count exposed on a card, Spacing wired.
    fn old_boxes(with_body: bool) -> EffectGraphDef {
        let float = |v: f32| serde_json::json!({"type": "Float", "value": v});
        let mut wires = vec![
            serde_json::json!({"fromNode": 5, "fromPort": "out", "toNode": 40, "toPort": "copy_spacing"}),
        ];
        if with_body {
            wires.push(serde_json::json!({"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}));
        }
        serde_json::from_value(serde_json::json!({
            "version": 2,
            "name": "Boxes",
            "presetMetadata": {"id": "Boxes", "displayName": "Boxes", "category": "Geometry", "oscPrefix": "b",
                "params": [
                    {"id": "40_copy_count", "name": "Box Count", "min": 0.0, "max": 4000.0, "defaultValue": 256.0},
                    {"id": "40_reset", "name": "Reset", "min": 0.0, "max": 1.0, "defaultValue": 0.0}
                ],
                "bindings": [
                    {"id": "40_copy_count", "label": "Box Count", "defaultValue": 256.0,
                        "target": {"kind": "node", "nodeId": "world", "param": "copy_count"}, "convert": {"type": "IntRound"}},
                    {"id": "40_reset", "label": "Reset", "defaultValue": 0.0,
                        "target": {"kind": "node", "nodeId": "world", "param": "reset"}, "convert": {"type": "Trigger"}}
                ]},
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world",
                    "params": {"copy_count": float(256.0), "copy_spacing": float(1.85), "copy_columns": float(24.0),
                        "copy_layout": {"type": "Enum", "value": 1}, "speed": float(1.0)},
                    "exposedParams": ["copy_count", "reset"]},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body", "params": {"bounce": float(0.38)}},
                {"id": 5, "nodeId": "lfo", "typeId": "node.lfo"}
            ],
            "wires": wires
        }))
        .expect("old boxes graph")
    }

    #[test]
    fn world_controls_move_onto_the_copies_body_with_their_cards() {
        let mut def = old_boxes(true);
        let report = migrate_world_copies(&mut def);
        assert_eq!(report, WorldCopiesMigration { changed: true, moved: true, no_copies_body: false });

        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(COPY_PARAMS.iter().all(|p| !world.params.contains_key(*p) && !world.exposed_params.contains(*p)));
        assert!(world.params.contains_key("speed") && world.exposed_params.contains("reset"), "the world keeps its own");
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        assert_eq!(body.params.get("copy_count"), Some(&SerializedParamValue::Float { value: 256.0 }));
        assert_eq!(body.params.get("copy_spacing"), Some(&SerializedParamValue::Float { value: 1.85 }));
        assert_eq!(body.params.get("copy_columns"), Some(&SerializedParamValue::Float { value: 24.0 }));
        assert_eq!(body.params.get("copy_layout"), Some(&SerializedParamValue::Enum { value: 1 }));
        assert!(body.exposed_params.contains("copy_count"));
        assert!(def.wires.iter().any(|w| w.from_node == 5 && w.to_node == 121 && w.to_port == "copy_spacing"), "the wire follows");
        let meta = def.preset_metadata.as_ref().unwrap();
        let count = meta.bindings.iter().find(|b| b.id == "40_copy_count").expect("the card keeps its id");
        assert!(matches!(&count.target, BindingTarget::Node { node_id, param } if node_id == "box_body" && param == "copy_count"));
        assert!(meta.params.iter().any(|p| p.id == "40_copy_count"));

        let migrated = def.clone();
        assert_eq!(migrate_world_copies(&mut def), WorldCopiesMigration::default(), "idempotent");
        assert_eq!(def, migrated);
    }

    #[test]
    fn a_world_with_no_copies_body_loses_its_dead_controls() {
        let mut def = old_boxes(false);
        let report = migrate_world_copies(&mut def);
        assert_eq!(report, WorldCopiesMigration { changed: true, moved: false, no_copies_body: true });
        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(COPY_PARAMS.iter().all(|p| !world.params.contains_key(*p)));
        assert!(!def.wires.iter().any(|w| w.to_port == "copy_spacing"));
        let meta = def.preset_metadata.as_ref().unwrap();
        assert!(!meta.params.iter().any(|p| p.id == "40_copy_count"));
        assert!(meta.params.iter().any(|p| p.id == "40_reset"));
    }

    #[test]
    fn the_body_s_own_controls_and_wiring_win() {
        let mut def = old_boxes(true);
        let body = def.nodes.iter_mut().find(|n| n.node_id == "box_body").unwrap();
        body.params.insert("copy_count".into(), SerializedParamValue::Float { value: 9.0 });
        def.wires.push(EffectGraphWire { from_node: 5, from_port: "out".into(), to_node: 121, to_port: "copy_spacing".into() });
        migrate_world_copies(&mut def);
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        assert_eq!(body.params.get("copy_count"), Some(&SerializedParamValue::Float { value: 9.0 }));
        assert_eq!(def.wires.iter().filter(|w| w.to_port == "copy_spacing").count(), 1, "no double wire");
    }

    #[test]
    fn only_a_body_wired_into_copies_is_a_copies_body() {
        let def = old_boxes(true);
        assert!(is_copies_body(121, &def.nodes, &def.wires));
        assert!(!is_copies_body(5, &def.nodes, &def.wires));
        assert!(is_copies_body_param("node.rigid_body", "copy_count"));
        assert!(!is_copies_body_param("node.physics_world", "copy_count"));
    }

    /// A Physics Boxes scene saved before the box spawner moved: Count,
    /// Spacing, Columns and Arrangement stored on the Physics node, Count on
    /// a card, and a body wired into `copies`.
    fn world_copies_scene() -> EmbeddedPreset {
        let float = |v: f32| serde_json::json!({"type": "Float", "value": v});
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "name": "Box Scene",
            "presetMetadata": {"id": "BoxScene", "displayName": "Box Scene", "category": "Geometry",
                "oscPrefix": "box",
                "params": [{"id": "40_copy_count", "name": "Box Count", "min": 0.0, "max": 4000.0, "defaultValue": 256.0}],
                "bindings": [{"id": "40_copy_count", "label": "Box Count", "defaultValue": 256.0,
                    "target": {"kind": "node", "nodeId": "world", "param": "copy_count"}, "convert": {"type": "IntRound"}}]},
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world", "handle": "world",
                    "params": {"copy_count": float(256.0), "copy_spacing": float(1.85), "copy_columns": float(24.0),
                        "copy_layout": {"type": "Enum", "value": 1}},
                    "exposedParams": ["copy_count"]},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body", "handle": "box_body"}
            ],
            "wires": [{"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies"}]
        }))
        .expect("box scene def");
        EmbeddedPreset { kind: PresetKind::Generator, def, origin: EmbeddedOrigin::Saved }
    }

    /// The four controls land on the copies body with their values, the
    /// Physics node is left with none, the card keeps its id, the load
    /// report has one line, and loading again changes nothing.
    #[test]
    fn world_box_controls_move_onto_the_copies_body_at_load() {
        use crate::effect_graph_def::{BindingTarget, SerializedParamValue};

        let mut p = Project::default();
        p.embedded_presets.push(world_copies_scene());
        p.on_after_deserialize();

        let def = &p.embedded_presets[0].def;
        let world = def.nodes.iter().find(|n| n.node_id == "world").unwrap();
        assert!(world.params.keys().all(|k| !k.starts_with("copy_")), "{:?}", world.params);
        assert!(world.exposed_params.iter().all(|k| !k.starts_with("copy_")));
        let body = def.nodes.iter().find(|n| n.node_id == "box_body").unwrap();
        for (param, value) in [
            ("copy_count", SerializedParamValue::Float { value: 256.0 }),
            ("copy_spacing", SerializedParamValue::Float { value: 1.85 }),
            ("copy_columns", SerializedParamValue::Float { value: 24.0 }),
            ("copy_layout", SerializedParamValue::Enum { value: 1 }),
        ] {
            assert_eq!(body.params.get(param), Some(&value), "{param}");
        }
        let meta = def.preset_metadata.as_ref().unwrap();
        assert!(meta.bindings.iter().any(|b| b.id == "40_copy_count"
            && matches!(&b.target, BindingTarget::Node { node_id, param } if node_id == "box_body" && param == "copy_count")));
        assert_eq!(p.load_report.migration_notes.len(), 1, "{:?}", p.load_report.migration_notes);
        assert!(p.load_report.migration_notes[0].contains("\"Box Scene\""));
        assert!(p.load_report.migration_notes[0].contains("onto its copies body"));

        let migrated = p.embedded_presets[0].def.clone();
        p.on_after_deserialize();
        assert_eq!(p.embedded_presets[0].def, migrated, "idempotent");
        assert_eq!(p.load_report.migration_notes.len(), 1, "no second note");
    }
}
