//! The Add menu's scene templates, as data: one group per kind plus the card
//! rows of its own nodes, in template-local ids (`manifold_core::scene_template`
//! gives them their identity on insert). Scene kinds live here; Water lives in
//! `manifold-nodes-water`.

use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, PresetMetadata, RENDER_SCENE_TYPE_ID, SerializedParamValue,
};
use manifold_core::scene_object_model::scene_object_models;
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;
use manifold_core::scene_template::{HANDLE_TOKEN, SceneTemplate, TemplatePhysics};

use super::scene_modifier_authoring::stock_force;

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
    SceneTemplate { group, metadata, physics: Default::default() }
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
/// under it. Placed inside the water's `domain` box: an inflow at the top
/// centre, a drain on the floor by the +X wall so the inflow does not pour
/// straight into it, a collider in the middle.
pub fn role_source_template(kind: RoleSourceKind, domain: &FluidDomainLayout) -> SceneTemplate {
    let [min_x, min_y, min_z] = domain.min;
    let [sx, sy, sz] = domain.size;
    let (cx, cz) = (min_x + sx * 0.5, min_z + sz * 0.5);
    let inflow = 0.15 * sx.min(sz);
    let drain = [0.2 * sx, 0.1 * sy, 0.2 * sz];
    let (display_name, role, pos, scale) = match kind {
        RoleSourceKind::Inflow => ("Inflow", 1, [cx, min_y + sy - inflow, cz], [inflow; 3]),
        RoleSourceKind::Drain => (
            "Drain",
            2,
            [min_x + sx - drain[0], min_y + drain[1] * 0.5, cz],
            drain,
        ),
        RoleSourceKind::Collider => ("Collider", 3, [cx, min_y + sy * 0.5, cz], [0.25 * sx, 0.25 * sy, 0.25 * sz]),
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
    SceneTemplate { group, metadata, physics: Default::default() }
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
    SceneTemplate { group: light, metadata, physics: Default::default() }
}


/// Add Swarm: thousands of GPU instances of a small shape, pushed by the
/// scene's forces. `slot` picks its tint; `centroid` its graph editor
/// position. The same group, inserted into the preset's dressing, is the
/// shipped `Swarm` preset ([`swarm_preset_def`]).
pub fn swarm_template(slot: u32, centroid: (f32, f32)) -> Result<SceneTemplate, String> {
    let mut template = swarm_body(tint(slot), centroid);
    template.physics = swarm_physics()?;
    Ok(template)
}

/// What a swarm brings (D-114): an Attractor and a Radial Force at the world
/// origin, where the swarm starts, and drifting gusts. Together the two
/// radial forces are a goal spring (Houdini's POP Attract in spring mode):
/// beyond the Radial Force's radius the Attractor pulls at full strength,
/// inside it the push cancels the pull in proportion, so the pull grows with
/// distance from the centre. Gravity sags the cloud about a metre below the
/// centre; the gusts' Air Resistance relaxes every particle toward the gusts,
/// so the cloud breathes and swirls but never leaves, at stock World gravity
/// with no still air. The swarm never sets the Physics World.
fn swarm_physics() -> Result<TemplatePhysics, String> {
    Ok(TemplatePhysics {
        forces: vec![
            stock_force("Attractor", &SWARM_ATTRACTOR)?,
            stock_force("RadialForce", &SWARM_CORE)?,
            stock_force("Wind", &SWARM_GUSTS)?,
        ],
    })
}

/// Drifting gusts that stir the cloud so it lives on: the stock Wind with
/// Turbulence on (Houdini's POP Attract with wind noise). Swirls smaller than
/// the cloud keep it from collapsing to a point; a slow Speed lets them
/// drift; Air Resistance 3 is how strongly a particle follows them.
const SWARM_GUSTS: [(&str, f32); 4] = [("speed", 0.5), ("turbulence", 5.0), ("turbulence_size", 0.8), ("air_resistance", 3.0)];

/// The Attractor's full pull, stronger than gravity plus the gusts' push,
/// so nothing escapes. Tuned on the CPU twin (`swarm_hold`) for a 60 s clip.
const SWARM_ATTRACTOR: [(&str, f32); 4] = [("strength", 20.0), ("radius", 2.5), ("falloff", 1.0), ("curve", 1.0)];

/// The soft core: an outward push equal to the Attractor at the centre,
/// fading linearly to nothing at 1.5 m, so the pull inside is a spring.
const SWARM_CORE: [(&str, f32); 4] = [("strength", 20.0), ("radius", 1.5), ("falloff", 1.0), ("curve", 0.0)];

/// The Swarm preset's warm orange.
#[cfg(any(test, feature = "testkit"))]
const SWARM_PRESET_TINT: [f32; 3] = [0.9, 0.55, 0.2];

/// One swarm: a clock and force lattice over the `bounds` box, a seeded
/// particle loop the forces move, copies placed at the particles, ending in
/// one scene object, so the row, its card and its box gizmo work as for any
/// object. Its Speed and Reset come from the Physics World on insert (D-107).
fn swarm_body(tint: manifold_core::Color, centroid: (f32, f32)) -> SceneTemplate {
    const BOUNDS: u32 = 1;
    const FORCES: u32 = 2;
    const START: u32 = 3;
    const SEED: u32 = 4;
    const STATE: u32 = 5;
    const MOVE: u32 = 6;
    const COPIES: u32 = 7;
    const SHAPE: u32 = 8;
    const MATERIAL_NODE: u32 = 9;
    const OBJECT_NODE: u32 = 10;
    const OUTPUT_NODE: u32 = 11;
    const GROUP_NODE: u32 = 12;
    let named = |id: u32, name: &str, type_id: &str, handle: Option<String>, params: BTreeMap<String, SerializedParamValue>| {
        let mut built = node(id, type_id, handle.as_deref(), params);
        built.node_id = NodeId::new(name);
        built
    };
    let suffixed = |suffix: &str| Some(format!("{HANDLE_TOKEN} {suffix}"));
    // The force lattice samples the forces inside the bounds box and holds its
    // edge value outside it, so the box is wider than the 4 m start cube: the
    // Attractor and the gusts keep their shape over the whole stirred cloud.
    let nodes = vec![
        named(
            BOUNDS,
            "swarm_bounds",
            "node.transform_3d",
            suffixed("Bounds"),
            BTreeMap::from([
                ("scale_x".to_owned(), float(6.0)),
                ("scale_y".to_owned(), float(6.0)),
                ("scale_z".to_owned(), float(6.0)),
            ]),
        ),
        named(FORCES, "swarm_forces", "node.swarm_forces", suffixed("Forces"), BTreeMap::new()),
        named(START, "swarm_start", "node.cube_mesh", None, BTreeMap::from([("size".to_owned(), float(4.0))])),
        named(
            SEED,
            "swarm_seed",
            "node.spawn_from_mesh",
            suffixed("Seed"),
            BTreeMap::from([
                ("mode".to_owned(), SerializedParamValue::Enum { value: 1 }),
                ("max_capacity".to_owned(), float(100_000.0)),
                ("active_count".to_owned(), float(20_000.0)),
            ]),
        ),
        named(STATE, "swarm_state", "node.array_feedback", None, BTreeMap::new()),
        named(MOVE, "swarm_move", "node.move_swarm", None, BTreeMap::from([("active_count".to_owned(), float(100_000.0))])),
        named(COPIES, "swarm_copies", "node.swarm_to_copies", suffixed("Copies"), BTreeMap::from([("size".to_owned(), float(0.05))])),
        named(SHAPE, "swarm_shape", "node.platonic_solid_mesh", None, BTreeMap::new()),
        named(
            MATERIAL_NODE,
            "swarm_material",
            "node.pbr_material",
            None,
            BTreeMap::from([
                ("color_r".to_owned(), float(tint.r)),
                ("color_g".to_owned(), float(tint.g)),
                ("color_b".to_owned(), float(tint.b)),
                ("metallic".to_owned(), float(0.1)),
                ("roughness".to_owned(), float(0.5)),
            ]),
        ),
        named(OBJECT_NODE, "swarm_object", "node.scene_object", Some(HANDLE_TOKEN.to_owned()), BTreeMap::new()),
        node(OUTPUT_NODE, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new()),
    ];
    // The forces node's clock and field scalars go to the mover port for
    // port; `epoch` also restarts the seed and the particle loop.
    const CLOCK_PORTS: [&str; 14] = [
        "forces", "ticks", "first_tick", "force_lattices", "field_nodes_x", "field_nodes_y", "field_nodes_z",
        "field_spacing", "field_min_x", "field_min_y", "field_min_z", "tick_seconds", "impulses", "impulse_tick",
    ];
    let mut wires = vec![
        wire(BOUNDS, "transform", FORCES, "bounds"),
        wire(START, "vertices", SEED, "vertices"),
        wire(FORCES, "epoch", SEED, "reset_trigger"),
        wire(FORCES, "epoch", STATE, "reset_trigger"),
        wire(SEED, "particles", STATE, "seed"),
        wire(STATE, "out", MOVE, "in"),
        wire(MOVE, "out", STATE, "in"),
        wire(MOVE, "out", COPIES, "particles"),
        wire(COPIES, "copies", OBJECT_NODE, "instances"),
        wire(SHAPE, "vertices", OBJECT_NODE, "vertices"),
        wire(MATERIAL_NODE, "out", OBJECT_NODE, "material"),
        wire(OBJECT_NODE, "object", OUTPUT_NODE, "object"),
    ];
    wires.extend(CLOCK_PORTS.iter().map(|port| wire(FORCES, port, MOVE, port)));
    let mut group = node(GROUP_NODE, GROUP_TYPE_ID, Some(HANDLE_TOKEN), BTreeMap::new());
    group.editor_pos = Some(centroid);
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "object".into(), port_type: "Object".into() }],
            params: Vec::new(),
        },
        nodes,
        wires,
        tint: Some([tint.r, tint.g, tint.b, 1.0]),
    }));
    let metadata = owned_card("Swarm", &group);
    SceneTemplate { group, metadata, physics: Default::default() }
}

/// The card rows of a one-object group's own nodes, read off the one scene
/// object model: every node its row owns controls on, then its material and
/// the object. A control the Physics World drives on insert is the World's
/// row, never the node's own (it would be dead).
fn owned_card(display_name: &str, group: &EffectGraphNode) -> PresetMetadata {
    const SCENE: u32 = 0;
    let body = group.group.as_deref().expect("a template group has a body");
    let mut render = node(SCENE, RENDER_SCENE_TYPE_ID, None, BTreeMap::from([("objects".to_owned(), float(1.0))]));
    render.node_id = NodeId::new("scene");
    let output = &body.interface.outputs.first().expect("the group places one object").name;
    let mut probe: EffectGraphDef =
        serde_json::from_value(serde_json::json!({"version": 2, "nodes": [], "wires": []})).expect("an empty graph");
    probe.nodes = vec![render, group.clone()];
    probe.wires = vec![wire(group.id, output, SCENE, "object_0")];
    let scene = manifold_core::SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
    let rows = scene_object_models(&probe, &scene).expect("the template's object resolves");
    let [row] = rows.as_slice() else { panic!("a one-object template has one row: {rows:?}") };
    let local = |id: &NodeId| body.nodes.iter().find(|candidate| candidate.node_id == *id).expect("the row's node is in the template");
    let object = local(&row.object.node);
    let section = |suffix: &str| format!("{HANDLE_TOKEN} — {suffix}");
    let mut owned: Vec<(&EffectGraphNode, String)> =
        row.controls.iter().map(|control| (local(&control.node), section(display_name))).collect();
    if let Some(material) = body.wires.iter().find(|wire| wire.to_node == object.id && wire.to_port == "material") {
        let material = body.nodes.iter().find(|candidate| candidate.id == material.from_node).expect("material node");
        owned.push((material, section("Material")));
    }
    owned.push((object, HANDLE_TOKEN.to_owned()));
    let mut metadata = empty_metadata(display_name);
    for (found, label) in owned {
        let taken = manifold_core::param_lookup::world_control_inputs(&found.type_id);
        let descriptors: Vec<_> = metadata_for_node_type(&found.type_id)
            .into_iter()
            .filter(|meta| !taken.iter().any(|input| input.input == meta.name))
            .collect();
        stamp_scene_node_exposures_into(
            &mut metadata.params,
            &mut metadata.bindings,
            found.id,
            &found.node_id,
            &label,
            &descriptors,
            &found.params,
        );
    }
    metadata
}

/// The shipped `Swarm` generator preset: [`swarm_template`]'s group inserted
/// into a camera, light and environment by the one template insertion, so
/// the preset and an Add menu swarm are the same graph. The manifold-nodes
/// freshness test rewrites `Swarm.json` from this with `UPDATE_SWARM_PRESET=1`.
#[cfg(any(test, feature = "testkit"))]
pub fn swarm_preset_def() -> EffectGraphDef {
    let dressing = serde_json::json!({
        "name": "Swarm",
        "version": 2,
        "nodes": [
            {"id": 0, "nodeId": "input", "typeId": "system.generator_input", "handle": "input"},
            {"id": 1, "nodeId": "camera", "typeId": "node.orbit_camera", "handle": "camera", "params": {
                "distance": {"type": "Float", "value": 9.0}, "fov_y": {"type": "Float", "value": 0.9},
                "look_y": {"type": "Float", "value": 0.0}, "orbit": {"type": "Float", "value": 0.55},
                "tilt": {"type": "Float", "value": 0.4}}},
            {"id": 3, "nodeId": "environment", "typeId": "node.bake_environment", "handle": "environment", "params": {
                "fill": {"type": "Float", "value": 0.3}, "intensity": {"type": "Float", "value": 1.0},
                "mode": {"type": "Enum", "value": 1}}},
            {"id": 4, "nodeId": "sun", "typeId": "node.light", "handle": "sun", "params": {
                "intensity": {"type": "Float", "value": 1.5}, "mode": {"type": "Enum", "value": 0},
                "pos_x": {"type": "Float", "value": -6.0}, "pos_y": {"type": "Float", "value": 10.0},
                "pos_z": {"type": "Float", "value": 6.0}}},
            {"id": 30, "nodeId": "scene", "typeId": "node.render_scene", "handle": "scene", "params": {
                "lights": {"type": "Float", "value": 1.0}, "objects": {"type": "Float", "value": 0.0},
                "rt_enabled": {"type": "Bool", "value": false}, "temporal_upscale": {"type": "Bool", "value": false}}},
            {"id": 31, "nodeId": "final_output", "typeId": "system.final_output", "handle": "final_output"}
        ],
        "wires": [
            {"fromNode": 1, "fromPort": "out", "toNode": 30, "toPort": "camera"},
            {"fromNode": 3, "fromPort": "envmap", "toNode": 30, "toPort": "envmap"},
            {"fromNode": 4, "fromPort": "out", "toNode": 30, "toPort": "light_0"},
            {"fromNode": 4, "fromPort": "out", "toNode": 3, "toPort": "sun"},
            {"fromNode": 30, "fromPort": "color", "toNode": 31, "toPort": "in"}
        ],
        "presetMetadata": {
            "id": "Swarm", "displayName": "Swarm", "category": "Geometry", "oscPrefix": "swarm",
            "available": true, "isLineBased": false, "params": [], "bindings": []
        }
    });
    let def: EffectGraphDef = serde_json::from_value(dressing).expect("the Swarm dressing parses");
    let scene = manifold_core::SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") };
    let [r, g, b] = SWARM_PRESET_TINT;
    let mut template = swarm_body(manifold_core::Color::new(r, g, b, 1.0), (900.0, 200.0));
    template.physics = swarm_physics().expect("the stock swarm physics reads");
    manifold_core::scene_template::insert_scene_template_preparing(&def, &scene, None, &template, &super::scene_modifier_authoring::prepare_template_force)
        .expect("the swarm inserts into its dressing")
        .graph
}
