//! Starter scenes: picker presets that open working. Each is derived from a
//! shipped preset at build time and written out as JSON, so the starter never
//! hand-copies the template's nodes (SCENE_SETUP_PROGRAMME.md section 3F).
//!
//! `UPDATE_STARTER_SCENES=1 cargo nextest run -p manifold-nodes starter_scenes`
//! rewrites the JSON files; the freshness test fails while they differ. Every
//! param a builder writes must exist on its node's manifest and every card
//! binding must land on a real node param, so a rename in a base preset or a
//! node fails the test instead of writing a broken file.

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_node_engine::persistence::PrimitiveRegistry;
use serde_json::{Value, json};

use crate::bundled_presets::bundled_preset_json;

/// Shipped Dam Break the water starter derives from.
const WATER_BASE: &str = "WaterDamBreakGpuFlip";
/// Shipped Ocean Cliff the scan nodes and the scan path are lifted from.
const CLIFF_BASE: &str = "OceanCliff";
/// Shipped Physics Boxes the rubble starter derives from.
const RUBBLE_BASE: &str = "PhysicsBoxes";

pub const WATER_SCAN_PRESET: &str = "WaterInScan";
pub const RUBBLE_PRESET: &str = "RubblePile";
pub const SWARM_PRESET: &str = "SwarmCloud";

/// The Burst: centred on the swarm, wide enough to take all of it.
const SWARM_BURST: [(&str, f32); 5] = [("center_x", 0.0), ("center_y", 0.0), ("center_z", 0.0), ("radius", 5.0), ("impulse_strength", 14.0)];

/// Dam Break's box obstacle: transform, role, mesh, material, object.
const OBSTACLE_NODES: [&str; 5] = ["obstacle_transform", "obstacle_collider", "obstacle_mesh", "obstacle_material", "obstacle_object"];
/// Ocean Cliff's scan chain, lifted by stable id.
const CLIFF_SCAN_NODES: [&str; 6] = ["cliff_mesh", "cliff_base_color_map", "cliff_normal_map", "cliff_mr_map", "cliff_material", "cliff_object"];

fn base(name: &'static str) -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
    serde_json::from_str(&json).expect("the shipped preset parses")
}

fn f(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn find_node<'a>(nodes: &'a [Value], key: &str) -> Option<&'a Value> {
    for node in nodes {
        if node["nodeId"] == key {
            return Some(node);
        }
        if let Some(inner) = node["group"]["nodes"].as_array()
            && let Some(found) = find_node(inner, key)
        {
            return Some(found);
        }
    }
    None
}

fn node_mut<'a>(def: &'a mut Value, key: &str) -> &'a mut Value {
    def["nodes"]
        .as_array_mut()
        .expect("nodes")
        .iter_mut()
        .find(|n| n["nodeId"] == key)
        .unwrap_or_else(|| panic!("the base preset has no node {key}"))
}

fn node_id_of(def: &Value, id: u64) -> String {
    def["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == id)
        .and_then(|n| n["nodeId"].as_str())
        .unwrap_or_else(|| panic!("the base preset's node {id} has no stable id"))
        .to_owned()
}

fn manifest_has(type_id: &str, param: &str) -> bool {
    PrimitiveRegistry::with_builtin()
        .construct(type_id)
        .unwrap_or_else(|| panic!("{type_id} is not registered"))
        .parameters()
        .iter()
        .any(|p| p.name == param)
}

/// Write typed params onto a node; each key must exist on the node's manifest.
fn set_params(def: &mut Value, key: &str, values: &[(&str, Value)]) {
    let node = node_mut(def, key);
    let type_id = node["typeId"].as_str().expect("typeId").to_owned();
    if node["params"].is_null() {
        node["params"] = json!({});
    }
    for (param, value) in values {
        assert!(manifest_has(&type_id, param), "{key} ({type_id}) has no param `{param}`");
        node["params"][*param] = value.clone();
    }
}

fn set_floats(def: &mut Value, key: &str, values: &[(&str, f64)]) {
    let typed: Vec<(&str, Value)> = values.iter().map(|(k, v)| (*k, f(*v))).collect();
    set_params(def, key, &typed);
}

fn set_handle(def: &mut Value, key: &str, handle: &str) {
    node_mut(def, key)["handle"] = json!(handle);
}

fn wire(from: u64, from_port: &str, to: u64, to_port: &str) -> Value {
    json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port})
}

fn param_spec(id: &str, name: &str, section: &str, min: f64, max: f64, default: f64, whole: bool, trigger: bool, format: &str, card: bool) -> Value {
    json!({
        "id": id, "name": name, "min": min, "max": max, "defaultValue": default,
        "wholeNumbers": whole, "isToggle": false, "isTrigger": trigger,
        "formatString": format, "oscSuffix": id, "section": section, "cardVisible": card
    })
}

fn binding(id: &str, label: &str, default: f64, node_id: &str, param: &str, convert: &str) -> Value {
    json!({
        "id": id, "label": label, "defaultValue": default,
        "target": {"kind": "node", "nodeId": node_id, "param": param},
        "convert": {"type": convert}
    })
}

/// Every card binding lands on a real node param.
fn assert_bindings_resolve(name: &str, def: &Value) {
    let nodes = def["nodes"].as_array().expect("nodes");
    let bindings = def["presetMetadata"]["bindings"].as_array().into_iter().chain(def["presetMetadata"]["stringBindings"].as_array());
    for binding in bindings.flatten() {
        if binding["target"]["kind"] != "node" {
            continue;
        }
        let node_id = binding["target"]["nodeId"].as_str().expect("nodeId");
        let param = binding["target"]["param"].as_str().expect("param");
        let node = find_node(nodes, node_id).unwrap_or_else(|| panic!("{name}: binding `{}` targets missing node {node_id}", binding["id"]));
        let type_id = node["typeId"].as_str().expect("typeId");
        assert!(manifest_has(type_id, param), "{name}: binding `{}` targets {node_id}.{param}, which {type_id} does not have", binding["id"]);
    }
}

/// The Dam Break with its box swapped for the cliff photoscan: an open mesh
/// the water treats as a shell (Exact Mesh, Shell Thickness), drawn with the
/// scan's own textures. The scan chain is lifted from Ocean Cliff and every
/// file read is bound to one "Scan Model" picker.
pub fn water_in_scan() -> Value {
    let mut def = base(WATER_BASE);
    let cliff = base(CLIFF_BASE);
    let scan_path = cliff["presetMetadata"]["stringParams"]
        .as_array()
        .expect("cliff string params")
        .iter()
        .find(|p| p["id"] == "cliff_file")
        .and_then(|p| p["defaultValue"].as_str())
        .expect("Ocean Cliff's Cliff Model default")
        .to_owned();

    let mut dropped = Vec::new();
    for key in OBSTACLE_NODES {
        dropped.push(find_node(def["nodes"].as_array().expect("nodes"), key).unwrap_or_else(|| panic!("Dam Break has no {key}"))["id"].as_u64().expect("id"));
    }
    def["nodes"].as_array_mut().expect("nodes").retain(|n| !dropped.contains(&n["id"].as_u64().expect("id")));
    def["wires"].as_array_mut().expect("wires").retain(|w| {
        !dropped.contains(&w["fromNode"].as_u64().expect("from")) && !dropped.contains(&w["toNode"].as_u64().expect("to"))
    });

    // Lift the scan chain from Ocean Cliff under fresh ids and `scan_` names.
    let mut next_id = def["nodes"].as_array().expect("nodes").iter().map(|n| n["id"].as_u64().expect("id")).max().expect("nodes") + 1;
    let mut lifted = std::collections::BTreeMap::new();
    let mut new_nodes = Vec::new();
    for key in CLIFF_SCAN_NODES {
        let mut node = find_node(cliff["nodes"].as_array().expect("cliff nodes"), key).unwrap_or_else(|| panic!("Ocean Cliff has no {key}")).clone();
        let scan_key = key.replacen("cliff_", "scan_", 1);
        node["id"] = json!(next_id);
        node["nodeId"] = json!(scan_key);
        node["handle"] = json!(scan_key.split('_').map(|w| { let mut c = w.chars(); c.next().map(|h| h.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" "));
        lifted.insert(key, next_id);
        new_nodes.push(node);
        next_id += 1;
    }
    let transform = next_id;
    let collider = next_id + 1;
    new_nodes.push(json!({"id": transform, "typeId": "node.transform_3d", "handle": "Scan Transform", "nodeId": "scan_transform", "params": {
        "pos_x": f(0.35), "pos_y": f(0.47), "pos_z": f(0.0), "rot_y": f(std::f64::consts::FRAC_PI_2),
        "scale_x": f(0.095), "scale_y": f(0.095), "scale_z": f(0.095)}}));
    new_nodes.push(json!({"id": collider, "typeId": "node.fluid_role_source", "handle": "Scan Collider", "nodeId": "scan_collider", "params": {
        "role": {"type": "Enum", "value": 3}, "geometry": {"type": "Enum", "value": 1},
        "path": {"type": "String", "value": scan_path}, "recenter": {"type": "Bool", "value": true},
        "shell_thickness": f(0.125)}}));
    def["nodes"].as_array_mut().expect("nodes").extend(new_nodes);
    // The mesh sits where the collider does: recentred, no fit.
    set_params(&mut def, "scan_mesh", &[("recenter", json!({"type": "Bool", "value": true}))]);

    let water = def["nodes"].as_array().expect("nodes").iter().find(|n| n["nodeId"] == "water_family").expect("water family")["id"].as_u64().expect("id");
    let render = def["nodes"].as_array().expect("nodes").iter().find(|n| n["nodeId"] == "scene").expect("scene")["id"].as_u64().expect("id");
    let id = |key: &str| lifted[key];
    let wires = def["wires"].as_array_mut().expect("wires");
    wires.push(wire(transform, "transform", collider, "transform"));
    wires.push(wire(collider, "role", water, "role_0"));
    wires.push(wire(id("cliff_mesh"), "vertices", id("cliff_object"), "vertices"));
    wires.push(wire(id("cliff_material"), "out", id("cliff_object"), "material"));
    wires.push(wire(transform, "transform", id("cliff_object"), "transform"));
    wires.push(wire(id("cliff_base_color_map"), "out", id("cliff_object"), "base_color_map"));
    wires.push(wire(id("cliff_normal_map"), "out", id("cliff_object"), "normal_map"));
    wires.push(wire(id("cliff_mr_map"), "out", id("cliff_object"), "mr_map"));
    wires.push(wire(id("cliff_object"), "object", render, "object_1"));

    let meta = &mut def["presetMetadata"];
    meta["id"] = json!(WATER_SCAN_PRESET);
    meta["displayName"] = json!("Water in a Photoscan");
    meta["oscPrefix"] = json!("waterinscan");
    meta["stringParams"].as_array_mut().expect("string params").push(json!({
        "defaultValue": scan_path, "file_path": true, "id": "scan_file", "isFilePicker": true, "name": "Scan Model", "useDropdown": false
    }));
    for key in ["scan_mesh", "scan_base_color_map", "scan_normal_map", "scan_mr_map", "scan_collider"] {
        meta["stringBindings"].as_array_mut().expect("string bindings").push(json!({
            "defaultValue": scan_path, "id": "scan_file", "label": "Scan Model",
            "target": {"kind": "node", "nodeId": key, "param": "path"}
        }));
    }
    assert_bindings_resolve(WATER_SCAN_PRESET, &def);
    def
}

/// Physics Boxes retuned into a rubble pile: a chute, a wall, a card of what
/// a performer reaches for, and an Explosion that blows the pile apart.
pub fn rubble_pile() -> Value {
    let mut def = base(RUBBLE_BASE);
    let world = node_id_of(&def, 40);
    let boxes = node_id_of(&def, 121);
    let transform = node_id_of(&def, 120);
    let boxes_object = node_id_of(&def, 124);
    let keys: std::collections::BTreeMap<u64, String> = def["nodes"].as_array().expect("nodes").iter().filter_map(|n| Some((n["id"].as_u64()?, n["nodeId"].as_str()?.to_owned()))).collect();
    let k = |id: u64| keys.get(&id).unwrap_or_else(|| panic!("the base preset has no node {id}")).clone();
    let (camera, sun, ground, chute, wall) = (k(1), k(4), k(100), k(140), k(160));
    set_floats(&mut def, &camera, &[("distance", 20.0), ("look_y", 2.2), ("orbit", -0.6), ("tilt", 0.38), ("fov_y", 0.9)]);
    set_floats(&mut def, &sun, &[("intensity", 2.2), ("pos_x", -10.0), ("pos_y", 18.0), ("pos_z", 12.0)]);
    set_floats(&mut def, &ground, &[("scale_x", 16.0), ("scale_y", 0.25), ("scale_z", 12.0)]);
    set_floats(&mut def, &boxes, &[("copy_count", 160.0), ("copy_columns", 10.0), ("copy_spacing", 0.95), ("friction", 0.45), ("bounce", 0.2)]);
    set_params(&mut def, &boxes, &[("copy_layout", json!({"type": "Enum", "value": 1}))]);
    set_floats(&mut def, &transform, &[("pos_x", -2.5), ("pos_y", 8.0), ("pos_z", 0.0), ("scale_x", 0.55), ("scale_y", 0.55), ("scale_z", 0.55)]);
    set_floats(&mut def, &chute, &[("pos_x", -2.5), ("pos_y", 1.3), ("pos_z", 0.0), ("rot_y", 0.0), ("rot_z", -0.35), ("scale_x", 7.0), ("scale_y", 0.4), ("scale_z", 7.0)]);
    set_floats(&mut def, &wall, &[("pos_x", 5.0), ("pos_y", 0.7), ("pos_z", 0.0), ("rot_x", 0.0), ("rot_y", 0.0), ("rot_z", 0.0), ("scale_x", 0.6), ("scale_y", 1.4), ("scale_z", 9.0)]);
    for (id, c) in [(123_u64, [0.78, 0.66, 0.5]), (143, [0.22, 0.24, 0.3]), (163, [0.22, 0.24, 0.3])] {
        let key = k(id);
        set_floats(&mut def, &key, &[("color_r", c[0]), ("color_g", c[1]), ("color_b", c[2]), ("roughness", 0.75), ("metallic", 0.0)]);
    }
    let floor_material = k(103);
    set_floats(&mut def, &floor_material, &[("color_r", 0.12), ("color_g", 0.13), ("color_b", 0.16)]);
    set_handle(&mut def, &world, "Physics");
    for (first, name) in [(140_u64, "Chute"), (160, "Wall")] {
        for (offset, part) in ["Transform", "Body", "Mesh", "Material", ""].into_iter().enumerate() {
            let key = k(first + offset as u64);
            set_handle(&mut def, &key, format!("{name} {part}").trim_end());
        }
    }

    let meta = &mut def["presetMetadata"];
    meta["id"] = json!(RUBBLE_PRESET);
    meta["displayName"] = json!("Rubble Pile");
    meta["oscPrefix"] = json!("rubblepile");
    let s = "Rubble";
    // Count, Size and Spacing are setup rows: the world rebuilds on a Size
    // change and Count and Spacing only apply on Reset.
    meta["params"] = json!([
        param_spec("count", "Count", s, 0.0, 4000.0, 160.0, true, false, "F0", false),
        param_spec("size", "Size", s, 0.1, 2.0, 0.55, false, false, "F2", false),
        param_spec("spacing", "Spacing", s, 0.3, 4.0, 0.95, false, false, "F2", false),
        param_spec("bounce", "Bounce", s, 0.0, 1.0, 0.2, false, false, "F2", true),
        param_spec("friction", "Friction", s, 0.0, 1.0, 0.45, false, false, "F2", true),
        param_spec("speed", "Speed", s, 0.0, 4.0, 1.0, false, false, "F1", true),
        param_spec("reset", "Reset", s, 0.0, 1.0, 0.0, true, true, "F0", true),
    ]);
    meta["bindings"] = json!([
        binding("count", "Count", 160.0, &boxes, "copy_count", "IntRound"),
        binding("size", "Size", 0.55, &transform, "scale_x", "Float"),
        binding("size", "Size", 0.55, &transform, "scale_y", "Float"),
        binding("size", "Size", 0.55, &transform, "scale_z", "Float"),
        binding("spacing", "Spacing", 0.95, &boxes, "copy_spacing", "IntRound"),
        binding("bounce", "Bounce", 0.2, &boxes, "bounce", "Float"),
        binding("friction", "Friction", 0.45, &boxes, "friction", "Float"),
        binding("speed", "Speed", 1.0, &world, "speed", "Float"),
        binding("reset", "Reset", 0.0, &world, "reset", "Trigger"),
    ]);
    meta["bindings"][4]["convert"] = json!({"type": "Float"});
    assert_bindings_resolve(RUBBLE_PRESET, &def);
    add_blast(def, &boxes_object)
}

/// One performer hit: an Explosion attached to `targets` of `scene`, centred and
/// sized by `tuning`, with its Impulse on the card as `label` under `section`.
fn add_hit(def: Value, id: &str, scene: SceneNodeRef, targets: SceneTargetSelection, section: &str, label: &str, tuning: &[(&str, f32)]) -> Value {
    let owner: EffectGraphDef = serde_json::from_value(def).expect("the starter def parses");
    let recipe: EffectGraphDef = serde_json::from_str(&bundled_preset_json(&PresetTypeId::new("Explosion")).expect("Explosion is bundled")).expect("Explosion parses");
    let mut instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(&owner, &recipe, manifold_core::NodeId::new(id), scene, targets)
        .expect("the Explosion prepares against the scene");
    manifold_nodes_scene::node_graph::scene_modifier_authoring::tune_force_card(&mut instance.graph, tuning).unwrap_or_else(|error| panic!("the Explosion tunes: {error}"));
    let mut edited = manifold_core::scene_modifier_edit::insert_scene_modifier(&owner, owner.scene_modifiers.len(), instance).expect("the Explosion inserts").graph;
    let host = edited.preset_metadata.as_mut().expect("host metadata");
    let of_this = |b: &&manifold_core::effect_graph_def::BindingDef| matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id, .. } if modifier_id.as_str() == id);
    let fire_ids: Vec<String> = host
        .bindings
        .iter()
        .filter(of_this)
        .filter(|b| matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id == "fire"))
        .map(|b| b.id.clone())
        .collect();
    assert_eq!(fire_ids.len(), 1, "the host has one row bound to the Explosion's fire: {fire_ids:?}");
    // The Explosion's other rows stay on the scene inspector, off the card.
    let modifier_rows: Vec<String> = host.bindings.iter().filter(of_this).map(|b| b.id.clone()).collect();
    for param in host.params.iter_mut().filter(|p| modifier_rows.contains(&p.id)) {
        param.card_visible = false;
    }
    let hit = host.params.iter_mut().find(|p| p.id == fire_ids[0]).expect("the host fire param");
    hit.name = label.into();
    hit.card_visible = true;
    hit.section = Some(section.into());
    serde_json::to_value(edited).expect("serialise")
}

/// Rubble's Blast: the Explosion on the boxes, centred on the pile's resting
/// place and wide enough to take every box.
fn add_blast(def: Value, target: &str) -> Value {
    let scene = {
        let owner: EffectGraphDef = serde_json::from_value(def.clone()).expect("the rubble def parses");
        owner.nodes.iter().find(|n| n.type_id == "node.render_scene").expect("render_scene").node_id.clone()
    };
    add_hit(
        def,
        "rubble_blast",
        SceneNodeRef { scope: Vec::new(), node: scene },
        SceneTargetSelection::Explicit { objects: vec![SceneNodeRef { scope: Vec::new(), node: manifold_core::NodeId::new(target) }] },
        "Rubble",
        "Blast",
        &[("center_x", 3.0), ("center_y", 0.6), ("center_z", 0.0), ("radius", 7.0), ("impulse_strength", 9.0)],
    )
}

/// The shipped Swarm, as it ships, plus one performer hit: an
/// Explosion on the swarm with Burst on the card. The swarm comes whole from
/// the public builder; nothing of it is copied here.
pub fn swarm_cloud() -> Value {
    let swarm = manifold_nodes_scene::node_graph::scene_templates::swarm_preset_def();
    let (scene, targets) = {
        let force = swarm.scene_modifiers.first().expect("the swarm brings its forces");
        (force.scene.clone(), force.targets.clone())
    };
    let mut def = serde_json::to_value(swarm).expect("serialise");
    let meta = &mut def["presetMetadata"];
    meta["id"] = json!(SWARM_PRESET);
    meta["displayName"] = json!("Swarm Cloud");
    meta["oscPrefix"] = json!("swarmcloud");
    assert_bindings_resolve(SWARM_PRESET, &def);
    add_hit(def, "swarm_burst", scene, targets, "Swarm", "Burst", &SWARM_BURST)
}

/// Every starter and the file it ships as.
pub fn starters() -> Vec<(&'static str, Value)> {
    vec![(WATER_SCAN_PRESET, water_in_scan()), (RUBBLE_PRESET, rubble_pile()), (SWARM_PRESET, swarm_cloud())]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped starter files are what the builders write. Set
    /// `UPDATE_STARTER_SCENES=1` to rewrite them after a template or tuning
    /// change.
    #[test]
    fn starter_scene_files_are_what_the_builders_write() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/generator-presets");
        for (name, built) in starters() {
            let path = dir.join(format!("{name}.json"));
            if std::env::var("UPDATE_STARTER_SCENES").is_ok() {
                let mut text = serde_json::to_string_pretty(&built).expect("serialise");
                text.push('\n');
                std::fs::write(&path, text).expect("write the starter");
            }
            let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}.json: {e}"))).expect("parses");
            assert!(shipped == built, "{name}.json differs from its builder; rerun with UPDATE_STARTER_SCENES=1");
        }
    }

    /// The Blast reaches the boxes: the Explosion's impulse resolves to the
    /// physics world as a rigid recipient that includes the copies.
    #[test]
    fn rubble_blast_reaches_the_copies() {
        let def: EffectGraphDef = serde_json::from_value(rubble_pile()).expect("parses");
        let instance = &def.scene_modifiers[0];
        let targets = manifold_node_engine::load::expand::impulse_recipients(&def, &instance.scene, &instance.targets, &PrimitiveRegistry::with_builtin()).expect("recipients resolve");
        let world = targets.iter().find(|(id, _)| id.as_str() == "physics_demo_40").unwrap_or_else(|| panic!("the physics world is a recipient: {targets:?}"));
        assert!(matches!(world.1, manifold_core::scene_impulse::ImpulseTarget::Rigid(t) if t.copies), "the blast reaches the copies: {:?}", world.1);
    }

    /// The Burst reaches the swarm through its own lattice recipient.
    #[test]
    fn swarm_burst_reaches_the_swarm() {
        let def: EffectGraphDef = serde_json::from_value(swarm_cloud()).expect("parses");
        let instance = def.scene_modifiers.iter().find(|m| m.id.as_str() == "swarm_burst").expect("the Burst is attached");
        let targets = manifold_node_engine::load::expand::impulse_recipients(&def, &instance.scene, &instance.targets, &PrimitiveRegistry::with_builtin()).expect("recipients resolve");
        assert!(targets.iter().any(|(_, t)| *t == manifold_core::scene_impulse::ImpulseTarget::Swarm), "the Burst reaches the swarm: {targets:?}");
    }
}
