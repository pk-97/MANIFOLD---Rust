//! Starter scenes: picker presets that open working. Each is derived from a
//! shipped preset at build time and written out as JSON, so the starter never
//! hand-copies the template's nodes (SCENE_SETUP_PROGRAMME.md section 3F).
//!
//! `UPDATE_STARTER_SCENES=1 cargo nextest run -p manifold-nodes starter_scenes`
//! rewrites the JSON files; the freshness test fails while they differ.

use manifold_core::PresetTypeId;
use serde_json::{Value, json};

use crate::bundled_presets::bundled_preset_json;

/// Shipped Dam Break the water starter derives from.
const WATER_BASE: &str = "WaterDamBreakGpuFlip";
/// Shipped Physics Boxes the rubble starter derives from.
const RUBBLE_BASE: &str = "PhysicsBoxes";

pub const WATER_SCAN_PRESET: &str = "WaterInScan";
pub const RUBBLE_PRESET: &str = "RubblePile";

/// The CC0 Poly Haven cliff photoscan the Ocean Cliff preset already uses.
const SCAN_PATH: &str = "/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/coastal_cliff_02/coastal_cliff_02_2k.gltf";

fn base(name: &'static str) -> Value {
    let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
    serde_json::from_str(&json).expect("the shipped preset parses")
}

fn f(v: f64) -> Value {
    json!({"type": "Float", "value": v})
}

fn node_mut(def: &mut Value, id: u64) -> &mut Value {
    def["nodes"]
        .as_array_mut()
        .expect("nodes")
        .iter_mut()
        .find(|n| n["id"] == id)
        .unwrap_or_else(|| panic!("the base preset has no node {id}"))
}

fn set_floats(def: &mut Value, id: u64, values: &[(&str, f64)]) {
    let node = node_mut(def, id);
    if node["params"].is_null() {
        node["params"] = json!({});
    }
    for (k, v) in values {
        node["params"][*k] = f(*v);
    }
}

fn wire(from: u64, from_port: &str, to: u64, to_port: &str) -> Value {
    json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port})
}

fn param_spec(id: &str, name: &str, section: &str, min: f64, max: f64, default: f64, whole: bool, trigger: bool, format: &str) -> Value {
    json!({
        "id": id, "name": name, "min": min, "max": max, "defaultValue": default,
        "wholeNumbers": whole, "isToggle": false, "isTrigger": trigger,
        "formatString": format, "oscSuffix": id, "section": section, "cardVisible": true
    })
}

fn binding(id: &str, label: &str, default: f64, node_id: &str, param: &str, convert: &str) -> Value {
    json!({
        "id": id, "label": label, "defaultValue": default,
        "target": {"kind": "node", "nodeId": node_id, "param": param},
        "convert": {"type": convert}
    })
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

/// The Dam Break with its box swapped for the cliff photoscan: an open mesh
/// the water treats as a shell (Exact Mesh, Shell Thickness), drawn with the
/// scan's own textures.
pub fn water_in_scan() -> Value {
    let mut def = base(WATER_BASE);
    // The base's box obstacle: transform, role, mesh, material, object.
    let dropped = [476_u64, 477, 478, 479, 480];
    def["nodes"].as_array_mut().expect("nodes").retain(|n| !dropped.contains(&n["id"].as_u64().expect("id")));
    def["wires"].as_array_mut().expect("wires").retain(|w| {
        !dropped.contains(&w["fromNode"].as_u64().expect("from")) && !dropped.contains(&w["toNode"].as_u64().expect("to"))
    });
    let texture = |id: u64, handle: &str, node_id: &str, index: i64, colour_space: i64| {
        json!({"id": id, "typeId": "node.gltf_texture_source", "handle": handle, "nodeId": node_id, "params": {
            "path": {"type": "String", "value": SCAN_PATH},
            "texture_index": {"type": "Int", "value": index},
            "color_space": {"type": "Enum", "value": colour_space},
            "width": {"type": "Int", "value": 2048},
            "height": {"type": "Int", "value": 2048}}})
    };
    let nodes = def["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({"id": 601, "typeId": "node.transform_3d", "handle": "Scan Transform", "nodeId": "scan_transform", "params": {
        "pos_x": f(0.35), "pos_y": f(0.47), "pos_z": f(0.0), "rot_y": f(std::f64::consts::FRAC_PI_2),
        "scale_x": f(0.095), "scale_y": f(0.095), "scale_z": f(0.095)}}));
    nodes.push(json!({"id": 602, "typeId": "node.fluid_role_source", "handle": "Scan Collider", "nodeId": "scan_collider", "params": {
        "role": {"type": "Enum", "value": 3}, "geometry": {"type": "Enum", "value": 1},
        "path": {"type": "String", "value": SCAN_PATH}, "fit": {"type": "Enum", "value": 0},
        "recenter": {"type": "Bool", "value": true}, "shell_thickness": f(0.06)}}));
    nodes.push(json!({"id": 603, "typeId": "node.gltf_mesh_source", "handle": "Scan Mesh", "nodeId": "scan_mesh", "params": {
        "path": {"type": "String", "value": SCAN_PATH}, "fit": {"type": "Enum", "value": 0},
        "recenter": {"type": "Bool", "value": true}, "max_capacity": {"type": "Int", "value": 2900000}}}));
    nodes.push(texture(604, "Scan Base Colour", "scan_base_color_map", 1, 0));
    nodes.push(texture(605, "Scan Normal", "scan_normal_map", 0, 1));
    nodes.push(texture(606, "Scan Roughness", "scan_mr_map", 2, 1));
    nodes.push(json!({"id": 607, "typeId": "node.pbr_material", "handle": "Scan Material", "nodeId": "scan_material", "params": {
        "color_r": f(1.0), "color_g": f(1.0), "color_b": f(1.0), "metallic": f(0.0), "roughness": f(1.0),
        "ior": f(1.45), "ambient": f(0.0)}}));
    nodes.push(json!({"id": 608, "typeId": "node.scene_object", "handle": "Scan", "nodeId": "scan_object", "params": {}}));
    let wires = def["wires"].as_array_mut().expect("wires");
    wires.push(wire(601, "transform", 602, "transform"));
    wires.push(wire(602, "role", 475, "role_0"));
    wires.push(wire(603, "vertices", 608, "vertices"));
    wires.push(wire(607, "out", 608, "material"));
    wires.push(wire(601, "transform", 608, "transform"));
    wires.push(wire(604, "out", 608, "base_color_map"));
    wires.push(wire(605, "out", 608, "normal_map"));
    wires.push(wire(606, "out", 608, "mr_map"));
    wires.push(wire(608, "object", 463, "object_1"));
    def["presetMetadata"]["id"] = json!(WATER_SCAN_PRESET);
    def["presetMetadata"]["displayName"] = json!("Water in a Photoscan");
    def["presetMetadata"]["oscPrefix"] = json!("waterinscan");
    def
}

/// Physics Boxes retuned into a rubble pile: a chute, a wall, a card of what
/// a performer reaches for.
pub fn rubble_pile() -> Value {
    let mut def = base(RUBBLE_BASE);
    set_floats(&mut def, 1, &[("distance", 20.0), ("look_y", 2.2), ("orbit", -0.6), ("tilt", 0.38), ("fov_y", 0.9)]);
    set_floats(&mut def, 4, &[("intensity", 2.2), ("pos_x", -10.0), ("pos_y", 18.0), ("pos_z", 12.0)]);
    set_floats(&mut def, 100, &[("scale_x", 16.0), ("scale_y", 0.25), ("scale_z", 12.0)]);
    set_floats(&mut def, 121, &[("copy_count", 160.0), ("copy_columns", 10.0), ("copy_spacing", 0.95), ("friction", 0.45), ("bounce", 0.2)]);
    node_mut(&mut def, 121)["params"]["copy_layout"] = json!({"type": "Enum", "value": 1});
    set_floats(&mut def, 120, &[("pos_x", -2.5), ("pos_y", 8.0), ("pos_z", 0.0), ("scale_x", 0.55), ("scale_y", 0.55), ("scale_z", 0.55)]);
    set_floats(&mut def, 140, &[("pos_x", -2.5), ("pos_y", 1.3), ("pos_z", 0.0), ("rot_y", 0.0), ("rot_z", -0.35), ("scale_x", 7.0), ("scale_y", 0.4), ("scale_z", 7.0)]);
    set_floats(&mut def, 160, &[("pos_x", 5.0), ("pos_y", 0.7), ("pos_z", 0.0), ("rot_x", 0.0), ("rot_y", 0.0), ("rot_z", 0.0), ("scale_x", 0.6), ("scale_y", 1.4), ("scale_z", 9.0)]);
    for (id, c) in [(123_u64, [0.78, 0.66, 0.5]), (143, [0.22, 0.24, 0.3]), (163, [0.22, 0.24, 0.3])] {
        set_floats(&mut def, id, &[("color_r", c[0]), ("color_g", c[1]), ("color_b", c[2]), ("roughness", 0.75), ("metallic", 0.0)]);
    }
    set_floats(&mut def, 103, &[("color_r", 0.12), ("color_g", 0.13), ("color_b", 0.16)]);
    let world = node_id_of(&def, 40);
    let boxes = node_id_of(&def, 121);
    let transform = node_id_of(&def, 120);
    let meta = &mut def["presetMetadata"];
    meta["id"] = json!(RUBBLE_PRESET);
    meta["displayName"] = json!("Rubble Pile");
    meta["oscPrefix"] = json!("rubblepile");
    let s = "Rubble";
    meta["params"] = json!([
        param_spec("count", "Count", s, 0.0, 4000.0, 160.0, true, false, "F0"),
        param_spec("size", "Size", s, 0.1, 2.0, 0.55, false, false, "F2"),
        param_spec("spacing", "Spacing", s, 0.3, 4.0, 0.95, false, false, "F2"),
        param_spec("bounce", "Bounce", s, 0.0, 1.0, 0.2, false, false, "F2"),
        param_spec("friction", "Friction", s, 0.0, 1.0, 0.45, false, false, "F2"),
        param_spec("speed", "Speed", s, 0.0, 4.0, 1.0, false, false, "F1"),
        param_spec("reset", "Reset", s, 0.0, 1.0, 0.0, true, true, "F0"),
    ]);
    meta["bindings"] = json!([
        binding("count", "Count", 160.0, &boxes, "copy_count", "IntRound"),
        binding("size", "Size", 0.55, &transform, "scale_x", "Float"),
        binding("size", "Size", 0.55, &transform, "scale_y", "Float"),
        binding("size", "Size", 0.55, &transform, "scale_z", "Float"),
        binding("spacing", "Spacing", 0.95, &boxes, "copy_spacing", "Float"),
        binding("bounce", "Bounce", 0.2, &boxes, "bounce", "Float"),
        binding("friction", "Friction", 0.45, &boxes, "friction", "Float"),
        binding("speed", "Speed", 1.0, &world, "speed", "Float"),
        binding("reset", "Reset", 0.0, &world, "reset", "Trigger"),
    ]);
    node_mut(&mut def, 30)["params"]["objects"] = f(4.0);
    def
}

/// Every starter and the file it ships as.
pub fn starters() -> Vec<(&'static str, Value)> {
    vec![(WATER_SCAN_PRESET, water_in_scan()), (RUBBLE_PRESET, rubble_pile())]
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
}
