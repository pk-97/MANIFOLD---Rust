//! The Add menu's scene templates, as data: one group per kind plus the card
//! rows of its own nodes, in template-local ids (`manifold_core::scene_template`
//! gives them their identity on insert). Scene kinds live here; Water lives in
//! `manifold-nodes-water`.

use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;
use manifold_core::scene_template::{HANDLE_TOKEN, SceneTemplate};

use super::scene_exposure::metadata_for_node_type;

const MESH: u32 = 1;
const MATERIAL: u32 = 2;
const TRANSFORM: u32 = 3;
const OBJECT: u32 = 4;
const OUTPUT: u32 = 5;
const GROUP: u32 = 6;

fn node(id: u32, type_id: &str, handle: Option<&str>, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(type_id.rsplit('.').next().unwrap_or(type_id)),
        type_id: type_id.to_owned(),
        handle: handle.map(str::to_owned),
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
}

fn float(value: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value }
}

/// A distinct tint for object slot `slot`, spread round the hue wheel by the
/// golden ratio, as imported objects are.
fn tint(slot: u32) -> manifold_core::Color {
    manifold_core::Color::hsv_to_rgb((slot as f32 * 0.618_034) % 1.0, 0.7, 0.85)
}

fn empty_metadata(display_name: &str) -> PresetMetadata {
    PresetMetadata {
        shape: None,
        id: manifold_core::PresetTypeId::from_string(format!("__template_{display_name}")),
        display_name: display_name.to_owned(),
        category: "Geometry".into(),
        osc_prefix: "scene".into(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: Vec::new(),
        bindings: Vec::new(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_modifier: None,
        scene_bounds: None,
    }
}

/// One placed object: a mesh, a material and a transform bound by one scene
/// object, grouped, with its material, transform and object rows on the card.
fn object_group(
    display_name: &str,
    mesh: (&str, BTreeMap<String, SerializedParamValue>),
    material: BTreeMap<String, SerializedParamValue>,
    slot: u32,
    centroid: (f32, f32),
) -> SceneTemplate {
    let tint = tint(slot);
    let mut material = material;
    for (name, value) in [("color_r", tint.r), ("color_g", tint.g), ("color_b", tint.b)] {
        material.insert(name.into(), float(value));
    }
    let nodes = vec![
        node(MESH, mesh.0, Some("mesh"), mesh.1),
        node(MATERIAL, "node.pbr_material", Some("material"), material),
        node(TRANSFORM, "node.transform_3d", Some("transform"), BTreeMap::new()),
        node(OBJECT, "node.scene_object", Some(HANDLE_TOKEN), BTreeMap::new()),
        node(OUTPUT, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new()),
    ];
    let mut metadata = empty_metadata(display_name);
    for (id, section, label) in [
        (MATERIAL, "node.pbr_material", format!("{HANDLE_TOKEN} — Material")),
        (TRANSFORM, "node.transform_3d", format!("{HANDLE_TOKEN} — Transform")),
        (OBJECT, "node.scene_object", HANDLE_TOKEN.to_owned()),
    ] {
        let found = nodes.iter().find(|candidate| candidate.id == id).expect("template node");
        stamp_scene_node_exposures_into(
            &mut metadata.params,
            &mut metadata.bindings,
            id,
            &found.node_id,
            &label,
            &metadata_for_node_type(section),
            &found.params,
        );
    }
    let mut group = node(GROUP, GROUP_TYPE_ID, Some(HANDLE_TOKEN), BTreeMap::new());
    group.editor_pos = Some(centroid);
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "object".into(), port_type: "Object".into() }],
            params: Vec::new(),
        },
        nodes,
        wires: vec![
            wire(MESH, "vertices", OBJECT, "vertices"),
            wire(MATERIAL, "out", OBJECT, "material"),
            wire(TRANSFORM, "transform", OBJECT, "transform"),
            wire(OBJECT, "object", OUTPUT, "object"),
        ],
        tint: Some([tint.r, tint.g, tint.b, 1.0]),
    }));
    SceneTemplate { group, metadata }
}

/// Add Object: a default cube. `slot` picks its tint; `centroid` its graph
/// editor position.
pub fn object_template(slot: u32, centroid: (f32, f32)) -> SceneTemplate {
    object_group("Object", ("node.cube_mesh", BTreeMap::new()), BTreeMap::new(), slot, centroid)
}

/// Add Plane: a sheet `width` by `height` world units, unlit and cut out, so
/// a layer composite skinned onto it keeps its own look.
pub fn plane_template(slot: u32, centroid: (f32, f32), width: f32, height: f32) -> SceneTemplate {
    let material = BTreeMap::from([
        ("baked_look".to_owned(), SerializedParamValue::Bool { value: true }),
        ("alpha_mode".to_owned(), SerializedParamValue::Enum { value: 1 }),
    ]);
    object_group(
        "Layer Plane",
        ("node.plane_mesh", BTreeMap::from([("width".to_owned(), float(width)), ("height".to_owned(), float(height))])),
        material,
        slot,
        centroid,
    )
}

/// The placed fluid sources the Add menu offers inside a water.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleSourceKind {
    Inflow,
    Drain,
    Collider,
}

/// Add Inflow, Drain or Collider: a box, placed by its transform, whose
/// fluid role output the insertion wires into the target water's next free
/// role input. The source group sits inside that water, so its row lists
/// under it. Placed where the Dam Break tank holds water: an inflow up in
/// the air, a drain on the floor, a collider where the preset's obstacle
/// stands.
pub fn role_source_template(kind: RoleSourceKind) -> SceneTemplate {
    let (display_name, role, pos, scale) = match kind {
        RoleSourceKind::Inflow => ("Inflow", 1, [0.0, 1.6, 0.0], [0.3, 0.3, 0.3]),
        RoleSourceKind::Drain => ("Drain", 2, [1.4, 0.15, 0.0], [0.4, 0.3, 0.4]),
        RoleSourceKind::Collider => ("Collider", 3, [0.35, 0.58, -0.1], [0.6, 1.16, 0.85]),
    };
    const TRANSFORM_NODE: u32 = 1;
    const ROLE_NODE: u32 = 2;
    const OUTPUT_NODE: u32 = 3;
    const GROUP_NODE: u32 = 4;
    let transform = node(
        TRANSFORM_NODE,
        "node.transform_3d",
        Some(&format!("{HANDLE_TOKEN} Transform")),
        BTreeMap::from([
            ("pos_x".to_owned(), float(pos[0])),
            ("pos_y".to_owned(), float(pos[1])),
            ("pos_z".to_owned(), float(pos[2])),
            ("scale_x".to_owned(), float(scale[0])),
            ("scale_y".to_owned(), float(scale[1])),
            ("scale_z".to_owned(), float(scale[2])),
        ]),
    );
    // A unit cube's circumradius, so the transform's scale is the box.
    let role_node = node(
        ROLE_NODE,
        "node.fluid_role_source",
        Some(HANDLE_TOKEN),
        BTreeMap::from([
            ("role".to_owned(), SerializedParamValue::Enum { value: role }),
            ("shape".to_owned(), SerializedParamValue::Enum { value: 1 }),
            ("radius".to_owned(), float(0.866_025_4)),
        ]),
    );
    let mut metadata = empty_metadata(display_name);
    for (found, label) in [(&role_node, HANDLE_TOKEN.to_owned()), (&transform, format!("{HANDLE_TOKEN} — Transform"))] {
        stamp_scene_node_exposures_into(
            &mut metadata.params,
            &mut metadata.bindings,
            found.id,
            &found.node_id,
            &label,
            &metadata_for_node_type(&found.type_id),
            &found.params,
        );
    }
    let mut group = node(GROUP_NODE, GROUP_TYPE_ID, Some(HANDLE_TOKEN), BTreeMap::new());
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "role".into(), port_type: "FluidRole".into() }],
            params: Vec::new(),
        },
        nodes: vec![transform, role_node, node(OUTPUT_NODE, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new())],
        wires: vec![wire(TRANSFORM_NODE, "transform", ROLE_NODE, "transform"), wire(ROLE_NODE, "role", OUTPUT_NODE, "role")],
        tint: None,
    }));
    SceneTemplate { group, metadata }
}

/// Add Light: a bare Sun light about 45 degrees up, white, casting shadows.
/// It stays a plain node on the scene, named "Light N", `position` its
/// graph editor spot.
pub fn light_template(position: (f32, f32)) -> SceneTemplate {
    let params = BTreeMap::from([
        ("mode".to_owned(), SerializedParamValue::Enum { value: 0 }),
        ("pos_x".to_owned(), float(0.0)),
        ("pos_y".to_owned(), float(7.0)),
        ("pos_z".to_owned(), float(7.0)),
        ("color_r".to_owned(), float(1.0)),
        ("color_g".to_owned(), float(1.0)),
        ("color_b".to_owned(), float(1.0)),
        ("intensity".to_owned(), float(1.0)),
        ("cast_shadows".to_owned(), float(1.0)),
    ]);
    let mut light = node(1, "node.light", Some(HANDLE_TOKEN), params);
    light.editor_pos = Some(position);
    let mut metadata = empty_metadata("Light");
    stamp_scene_node_exposures_into(
        &mut metadata.params,
        &mut metadata.bindings,
        light.id,
        &light.node_id,
        HANDLE_TOKEN,
        &metadata_for_node_type("node.light"),
        &light.params,
    );
    SceneTemplate { group: light, metadata }
}
