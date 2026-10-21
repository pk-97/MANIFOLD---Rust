//! A GPU water's starting column is an Initial Fill: a
//! `node.fluid_role_source` with role 0 in its own group beside the domain,
//! wired into a free role input and placed by its transform, the same model
//! as an Inflow or a Drain. Graphs saved before wired a transform straight
//! into the domain's `initial_volume`; [`migrate_initial_volume`] rebuilds
//! each as an Initial Fill over the same box, so an old show seeds the same
//! water.

use std::collections::{BTreeMap, BTreeSet};

use crate::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef,
    GroupInterface, InterfacePortDef, SerializedParamValue,
};
use crate::flatten::HANDLE_DELIM;
use crate::fluid_domain::MAX_FLUID_ROLES;
use crate::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};
use crate::NodeId;

/// The domain input the column used to arrive on.
pub const INITIAL_VOLUME_PORT: &str = "initial_volume";
pub const FLUID_ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
pub const TRANSFORM_3D_TYPE_ID: &str = "node.transform_3d";
/// `node.fluid_role_source`'s Initial Fill role and Cube shape.
pub const INITIAL_FILL_ROLE: u32 = 0;
pub const CUBE_SHAPE: u32 = 1;
/// A unit cube's circumradius, so the transform's scale is the box.
pub const UNIT_CUBE_RADIUS: f32 = 0.866_025_4;
/// The name an Initial Fill group and its role source carry.
pub const INITIAL_FILL_NAME: &str = "Initial Fill";

/// The GPU liquids, whose column moved onto an Initial Fill.
fn takes_initial_fill(type_id: &str) -> bool {
    type_id == GPU_FLIP_DOMAIN_TYPE_ID || type_id == MATTER_DOMAIN_TYPE_ID
}

/// Whether any level of `def`, or of a scene modifier's graph, still wires
/// a column into a GPU liquid's `initial_volume`. Lets the common load
/// borrow the document instead of cloning it.
pub fn carries_initial_volume(def: &EffectGraphDef) -> bool {
    fn level(nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> bool {
        wires.iter().any(|wire| column_wire(nodes, wire))
            || nodes.iter().any(|node| node.group.as_deref().is_some_and(|body| level(&body.nodes, &body.wires)))
    }
    level(&def.nodes, &def.wires) || def.scene_modifiers.iter().any(|modifier| carries_initial_volume(&modifier.graph))
}

fn column_wire(nodes: &[EffectGraphNode], wire: &EffectGraphWire) -> bool {
    wire.to_port == INITIAL_VOLUME_PORT && nodes.iter().any(|node| node.id == wire.to_node && takes_initial_fill(&node.type_id))
}

/// Rebuild every column wired into a GPU liquid's `initial_volume` as an
/// Initial Fill group beside the domain, wired into the domain's first free
/// role input. The column's transform moves into the group when the column
/// is all it feeds and no group param names it, so its stable id and every
/// card driving it stay; otherwise the group takes it through an input.
/// Returns whether anything changed; a graph with no column is untouched, so
/// it is idempotent.
pub fn migrate_initial_volume(def: &mut EffectGraphDef) -> bool {
    if !carries_initial_volume(def) {
        return false;
    }
    let mut taken = document_node_ids(def);
    let (mut changed, _) = migrate_level(&mut def.nodes, &mut def.wires, None, &mut taken);
    for modifier in &mut def.scene_modifiers {
        changed |= migrate_initial_volume(&mut modifier.graph);
    }
    changed
}

fn collect_node_ids(def: &EffectGraphDef, out: &mut BTreeSet<String>) {
    fn walk(nodes: &[EffectGraphNode], out: &mut BTreeSet<String>) {
        for node in nodes {
            out.insert(node.node_id.to_string());
            if let Some(body) = node.group.as_deref() {
                walk(&body.nodes, out);
            }
        }
    }
    walk(&def.nodes, out);
}

/// A stable id built from `base` that no node in the document has.
fn fresh_id(base: &str, taken: &mut BTreeSet<String>) -> NodeId {
    let mut candidate = base.to_owned();
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    taken.insert(candidate.clone());
    NodeId::new(candidate)
}

fn migrate_level(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    mut interface: Option<&mut GroupInterface>,
    taken: &mut BTreeSet<String>,
) -> (bool, Vec<String>) {
    let mut changed = false;
    // A group whose column arrived through an input now takes a role there:
    // what feeds that input becomes the Initial Fill.
    let mut retyped = Vec::new();
    for node in nodes.iter_mut() {
        if let Some(body) = node.group.as_deref_mut() {
            let GroupDef { interface, nodes, wires, .. } = body;
            let (inner, inputs) = migrate_level(nodes, wires, Some(interface), taken);
            changed |= inner;
            retyped.extend(inputs.into_iter().map(|input| (node.id, input)));
        }
    }
    let mut columns: Vec<EffectGraphWire> = Vec::new();
    wires.retain(|wire| {
        let column = column_wire(nodes, wire) || retyped.iter().any(|(group, input)| wire.to_node == *group && &wire.to_port == input);
        if column {
            columns.push(wire.clone());
        }
        !column
    });
    let mut inputs = Vec::new();
    let columns_in = columns.clone();
    // Judged before any fill adds wires: a role wired from the input for an
    // earlier water is not another reader.
    let sole_reader: Vec<bool> =
        columns.iter().map(|column| !wires.iter().any(|wire| wire.from_node == column.from_node && wire.from_port == column.from_port)).collect();
    for (column, sole_reader) in columns.into_iter().zip(sole_reader) {
        changed = true;
        // A column arriving through this group's own input: the input
        // carries the role instead, and the level above fills it.
        let through = nodes.iter().any(|node| node.id == column.from_node && node.type_id == GROUP_INPUT_TYPE_ID)
            && sole_reader;
        // An input already retyped for another water in this group carries
        // the same role to this one.
        let input = interface.as_deref_mut().filter(|_| through).and_then(|interface| {
            interface.inputs.iter_mut().find(|input| input.name == column.from_port && matches!(input.port_type.as_str(), "Transform" | "FluidRole"))
        });
        let target = if column.to_port == INITIAL_VOLUME_PORT {
            let Some(port) = free_role(wires, column.to_node) else {
                log::warn!("initial fill: the water has no free role input, so its old column is gone");
                continue;
            };
            port
        } else {
            column.to_port.clone()
        };
        if let Some(input) = input {
            input.port_type = "FluidRole".into();
            if !inputs.contains(&input.name) {
                inputs.push(input.name.clone());
            }
            wires.push(EffectGraphWire { from_node: column.from_node, from_port: column.from_port, to_node: column.to_node, to_port: target });
            continue;
        }
        // The column's transform moves in when it only feeds this one
        // column and nothing drives it: another water's column wire, already
        // taken out of `wires`, would be left reading a node that moved.
        let movable = nodes.iter().find(|node| node.id == column.from_node).is_some_and(|producer| {
            producer.type_id == TRANSFORM_3D_TYPE_ID
                && !wires.iter().any(|wire| wire.from_node == producer.id || wire.to_node == producer.id)
                && columns_in.iter().filter(|other| other.from_node == producer.id).count() == 1
                && producer.handle.as_deref().is_none_or(|handle| {
                    interface.as_deref().is_none_or(|interface| interface.params.iter().all(|param| param.target_handle != handle))
                })
        });
        let fill_box = if movable {
            let index = nodes.iter().position(|node| node.id == column.from_node).expect("found above");
            FillBox::Node(Box::new(nodes.remove(index)))
        } else {
            FillBox::Port(column.from_node, column.from_port)
        };
        place_initial_fill(nodes, wires, (column.to_node, target), fill_box, taken);
    }
    (changed, inputs)
}

/// The first role input of `domain` no wire feeds.
fn free_role(wires: &[EffectGraphWire], domain: u32) -> Option<String> {
    (0..MAX_FLUID_ROLES)
        .map(|k| format!("role_{k}"))
        .find(|port| !wires.iter().any(|wire| wire.to_node == domain && &wire.to_port == port))
}

/// Where an Initial Fill's box comes from.
pub enum FillBox {
    /// A `node.transform_3d`, moved into the group.
    Node(Box<EffectGraphNode>),
    /// A port in the domain's level, read through the group's `transform`
    /// input.
    Port(u32, String),
}

/// Every stable node id in `def`, for [`add_initial_fill`] to mint new
/// ones beside.
pub fn document_node_ids(def: &EffectGraphDef) -> BTreeSet<String> {
    let mut taken = BTreeSet::new();
    collect_node_ids(def, &mut taken);
    taken
}

/// Puts an Initial Fill group over `fill_box` beside `domain` (a doc id in
/// this level) and wires it into the domain's first free role input.
/// Returns the group's doc id, or `None` when every role input is taken.
pub fn add_initial_fill(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    domain: u32,
    fill_box: FillBox,
    taken: &mut BTreeSet<String>,
) -> Option<u32> {
    let port = free_role(wires, domain)?;
    Some(place_initial_fill(nodes, wires, (domain, port), fill_box, taken))
}

/// An Initial Fill group over `fill_box`, its role wired into `to` (a node
/// in this level and its input), placed beside that node.
fn place_initial_fill(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    (to_node, to_port): (u32, String),
    fill_box: FillBox,
    taken: &mut BTreeSet<String>,
) -> u32 {
    let (anchor_id, anchor_pos) =
        nodes.iter().find(|node| node.id == to_node).map(|node| (node.node_id.to_string(), node.editor_pos)).expect("the target is in this level");
    let group_id = nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    let (transform, outer) = match fill_box {
        FillBox::Node(node) => (Some(*node), None),
        FillBox::Port(node, port) => (None, Some((node, port))),
    };
    let body = initial_fill_body(transform, &anchor_id, taken);
    // Handles are unique within a level: the flattener prefixes the body's
    // handles with this one.
    let handle = (1..)
        .map(|n| if n == 1 { INITIAL_FILL_NAME.to_owned() } else { format!("{INITIAL_FILL_NAME} {n}") })
        .find(|handle| {
            let inside = format!("{handle}{HANDLE_DELIM}");
            nodes.iter().all(|node| node.handle.as_deref().is_none_or(|h| h != handle && !h.starts_with(&inside)))
        })
        .expect("an unused handle");
    // A level a flatten already produced carries group paths in its handles,
    // which a second flatten refuses: the fill goes in as the nodes the
    // flatten would have made.
    if nodes.iter().any(|node| node.handle.as_deref().is_some_and(|h| h.contains(HANDLE_DELIM))) {
        return place_flat_initial_fill(nodes, wires, (to_node, to_port), (body, outer), &handle);
    }
    let mut group = bare_node(group_id, GROUP_TYPE_ID, fresh_id(&format!("{anchor_id}_initial_fill"), taken), Some(&handle));
    group.editor_pos = anchor_pos.map(|(x, y)| (x - 260.0, y + 160.0));
    group.group = Some(Box::new(body));
    nodes.push(group);
    if let Some((from_node, from_port)) = outer {
        wires.push(EffectGraphWire { from_node, from_port, to_node: group_id, to_port: "transform".into() });
    }
    wires.push(EffectGraphWire { from_node: group_id, from_port: "role".into(), to_node, to_port });
    group_id
}



/// An Initial Fill as the plain nodes a flatten of its group would make, for a
/// level already flat: the moved transform and the role source under
/// `handle/`, wired as the group's body is. Returns the role source's doc id.
fn place_flat_initial_fill(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    (to_node, to_port): (u32, String),
    (body, outer): (GroupDef, Option<(u32, String)>),
    handle: &str,
) -> u32 {
    let inside = |inner: Option<String>| inner.map(|inner| format!("{handle}{HANDLE_DELIM}{inner}"));
    let mut moved = None;
    let mut role = None;
    for node in body.nodes {
        match node.type_id.as_str() {
            TRANSFORM_3D_TYPE_ID => moved = Some(node),
            FLUID_ROLE_SOURCE_TYPE_ID => role = Some(node),
            _ => {}
        }
    }
    let mut role = role.expect("the fill body holds a role source");
    role.id = nodes.iter().map(|node| node.id).chain(moved.as_ref().map(|node| node.id)).max().unwrap_or(0) + 1;
    role.handle = inside(role.handle.take());
    let role_id = role.id;
    let (from_node, from_port) = match (moved, outer) {
        (Some(mut node), _) => {
            node.handle = inside(node.handle.take());
            let from = node.id;
            nodes.push(node);
            (from, "transform".to_owned())
        }
        (None, Some(outer)) => outer,
        (None, None) => unreachable!("a fill is a moved transform or a port read"),
    };
    nodes.push(role);
    wires.push(EffectGraphWire { from_node, from_port, to_node: role_id, to_port: "transform".into() });
    wires.push(EffectGraphWire { from_node: role_id, from_port: "role".into(), to_node, to_port });
    role_id
}

/// The Initial Fill group's body: the transform (moved in, or read through
/// a `transform` input), the role source over its box, and the output.
fn initial_fill_body(transform: Option<EffectGraphNode>, domain_id: &str, taken: &mut BTreeSet<String>) -> GroupDef {
    let mut inputs = Vec::new();
    // A moved transform keeps its own doc id: card controls stamped from it
    // carry `{doc}_{param}` ids, and only that id marks them as its copies.
    let transform = match transform {
        Some(node) => node,
        None => {
            inputs.push(InterfacePortDef { name: "transform".into(), port_type: "Transform".into() });
            bare_node(1, GROUP_INPUT_TYPE_ID, fresh_id(&format!("{domain_id}_initial_fill_input"), taken), None)
        }
    };
    let (transform_id, role_id, output_id) = (transform.id, transform.id + 1, transform.id + 2);
    let mut role = bare_node(role_id, FLUID_ROLE_SOURCE_TYPE_ID, fresh_id(&format!("{domain_id}_initial_fill_role"), taken), Some(INITIAL_FILL_NAME));
    role.params = BTreeMap::from([
        ("role".to_owned(), SerializedParamValue::Enum { value: INITIAL_FILL_ROLE }),
        ("shape".to_owned(), SerializedParamValue::Enum { value: CUBE_SHAPE }),
        ("radius".to_owned(), SerializedParamValue::Float { value: UNIT_CUBE_RADIUS }),
    ]);
    let output = bare_node(output_id, GROUP_OUTPUT_TYPE_ID, fresh_id(&format!("{domain_id}_initial_fill_output"), taken), None);
    let wire = |from_node, from_port: &str, to_node, to_port: &str| EffectGraphWire {
        from_node,
        from_port: from_port.into(),
        to_node,
        to_port: to_port.into(),
    };
    GroupDef {
        interface: GroupInterface {
            inputs,
            outputs: vec![InterfacePortDef { name: "role".into(), port_type: "FluidRole".into() }],
            params: Vec::new(),
        },
        nodes: vec![transform, role, output],
        wires: vec![wire(transform_id, "transform", role_id, "transform"), wire(role_id, "role", output_id, "role")],
        tint: None,
    }
}

fn bare_node(id: u32, type_id: &str, node_id: NodeId, handle: Option<&str>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id,
        type_id: type_id.to_owned(),
        handle: handle.map(str::to_owned),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

impl crate::project::Project {
    /// Every saved graph's column becomes an Initial Fill
    /// ([`migrate_initial_volume`]). The water seeds the same box, so the
    /// load reports nothing.
    pub(crate) fn migrate_initial_volume_to_fill(&mut self) {
        for preset in &mut self.embedded_presets {
            migrate_initial_volume(&mut preset.def);
        }
        self.for_each_preset_instance_mut(|fx| {
            if let Some(def) = fx.graph.as_mut() {
                migrate_initial_volume(def);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(value: serde_json::Value) -> EffectGraphDef {
        serde_json::from_value(value).expect("graph")
    }

    fn column(id: u32) -> serde_json::Value {
        json!({"id": id, "nodeId": "initial_column", "typeId": TRANSFORM_3D_TYPE_ID, "handle": "Initial Column",
            "params": {"pos_x": {"type": "Float", "value": -1.25}, "scale_y": {"type": "Float", "value": 1.92}}})
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
    }

    /// The fill group's body, its role output wire, and the group node.
    fn fill<'a>(nodes: &'a [EffectGraphNode], wires: &[EffectGraphWire]) -> (&'a EffectGraphNode, EffectGraphWire) {
        let group = nodes.iter().find(|node| node.handle.as_deref() == Some(INITIAL_FILL_NAME)).expect("an Initial Fill group");
        let out = wires.iter().find(|wire| wire.from_node == group.id && wire.from_port == "role").expect("its role is wired").clone();
        (group, out)
    }

    /// A column that only feeds the domain moves into the fill, keeping its
    /// stable id and params, so every card bound to it still drives it. The
    /// fill takes the first free role; a second pass changes nothing.
    #[test]
    fn a_column_moves_into_an_initial_fill_on_the_first_free_role() {
        let mut graph = def(json!({"version": 3,
            "nodes": [{"id": 1, "nodeId": "domain", "typeId": GPU_FLIP_DOMAIN_TYPE_ID}, column(2),
                {"id": 3, "nodeId": "collider", "typeId": FLUID_ROLE_SOURCE_TYPE_ID}],
            "wires": [wire(2, "transform", 1, INITIAL_VOLUME_PORT), wire(3, "role", 1, "role_0")]}));
        let saved = graph.nodes.iter().find(|node| node.id == 2).unwrap().clone();
        assert!(migrate_initial_volume(&mut graph));
        assert!(!carries_initial_volume(&graph));
        assert!(graph.nodes.iter().all(|node| node.node_id.as_str() != "initial_column" || node.group.is_some()), "the column left the level");
        let (group, out) = fill(&graph.nodes, &graph.wires);
        assert_eq!(out, wire(group.id, "role", 1, "role_1"), "the collider keeps role_0");
        let body = group.group.as_deref().unwrap();
        assert!(body.interface.inputs.is_empty());
        let moved = body.nodes.iter().find(|node| node.node_id.as_str() == "initial_column").expect("the column moved in");
        assert_eq!((&moved.params, &moved.handle), (&saved.params, &saved.handle));
        let role = body.nodes.iter().find(|node| node.type_id == FLUID_ROLE_SOURCE_TYPE_ID).unwrap();
        assert_eq!(role.params["role"], SerializedParamValue::Enum { value: INITIAL_FILL_ROLE });
        assert_eq!(role.params["shape"], SerializedParamValue::Enum { value: CUBE_SHAPE });
        assert!(body.wires.contains(&wire(moved.id, "transform", role.id, "transform")));
        let ids = document_node_ids(&graph);
        assert_eq!(ids.len(), graph.nodes.len() + body.nodes.len(), "every stable id is unique");
        let once = graph.clone();
        assert!(!migrate_initial_volume(&mut graph));
        assert_eq!(graph, once);
    }

    /// A column that feeds something else stays where it is: the fill reads
    /// it through an input, so the other reader is untouched.
    #[test]
    fn a_shared_column_stays_and_feeds_the_fill_through_an_input() {
        let mut graph = def(json!({"version": 3,
            "nodes": [{"id": 1, "nodeId": "domain", "typeId": MATTER_DOMAIN_TYPE_ID}, column(2),
                {"id": 3, "nodeId": "box", "typeId": "node.scene_object"}],
            "wires": [wire(2, "transform", 1, INITIAL_VOLUME_PORT), wire(2, "transform", 3, "transform")]}));
        assert!(migrate_initial_volume(&mut graph));
        let (group, out) = fill(&graph.nodes, &graph.wires);
        assert_eq!(out.to_port, "role_0");
        assert!(graph.nodes.iter().any(|node| node.id == 2), "the column stays");
        assert!(graph.wires.contains(&wire(2, "transform", group.id, "transform")));
        assert!(graph.wires.contains(&wire(2, "transform", 3, "transform")));
        let body = group.group.as_deref().unwrap();
        assert_eq!(body.interface.inputs, [InterfacePortDef { name: "transform".into(), port_type: "Transform".into() }]);
    }

    /// A column that reaches the domain through its group's input: the input
    /// carries the role instead, and the column above becomes the fill, so
    /// it stays where the viewport finds it.
    #[test]
    fn a_column_through_a_group_input_becomes_a_fill_above_the_group() {
        let mut graph = def(json!({"version": 3,
            "nodes": [column(2), {"id": 5, "nodeId": "Live Matter", "typeId": "group", "handle": "Live Matter", "group": {
                "interface": {"inputs": [{"name": "initialVolume", "portType": "Transform"}], "outputs": []},
                "nodes": [{"id": 0, "nodeId": "live_input", "typeId": GROUP_INPUT_TYPE_ID},
                    {"id": 1, "nodeId": "matter_domain", "typeId": MATTER_DOMAIN_TYPE_ID}],
                "wires": [wire(0, "initialVolume", 1, INITIAL_VOLUME_PORT)]}}],
            "wires": [wire(2, "transform", 5, "initialVolume")]}));
        assert!(migrate_initial_volume(&mut graph));
        assert!(!carries_initial_volume(&graph));
        let live = graph.nodes.iter().find(|node| node.id == 5).unwrap().group.as_deref().unwrap();
        assert_eq!(live.interface.inputs[0].port_type, "FluidRole");
        assert_eq!(live.wires, [wire(0, "initialVolume", 1, "role_0")]);
        let (group, out) = fill(&graph.nodes, &graph.wires);
        assert_eq!(out, wire(group.id, "role", 5, "initialVolume"));
        assert!(group.group.as_deref().unwrap().nodes.iter().any(|node| node.node_id.as_str() == "initial_column"));
        assert!(!migrate_initial_volume(&mut graph));
    }

    /// One transform that places the column of two waters: the first fill
    /// must not take the node into its group, or the second fill wires to a
    /// node that no longer exists and the layer fails to load.
    #[test]
    fn a_column_shared_by_two_waters_stays_and_feeds_both_fills() {
        let mut graph = def(json!({"version": 3,
            "nodes": [{"id": 1, "nodeId": "domain_a", "typeId": GPU_FLIP_DOMAIN_TYPE_ID}, column(2),
                {"id": 3, "nodeId": "domain_b", "typeId": GPU_FLIP_DOMAIN_TYPE_ID}],
            "wires": [wire(2, "transform", 1, INITIAL_VOLUME_PORT), wire(2, "transform", 3, INITIAL_VOLUME_PORT)]}));
        assert!(migrate_initial_volume(&mut graph));
        assert!(graph.nodes.iter().any(|node| node.id == 2), "the shared column stays");
        let ids: BTreeSet<u32> = graph.nodes.iter().map(|node| node.id).collect();
        let dangling: Vec<_> = graph.wires.iter().filter(|wire| !ids.contains(&wire.from_node) || !ids.contains(&wire.to_node)).collect();
        assert!(dangling.is_empty(), "{dangling:?}");
        assert_eq!(graph.wires.iter().filter(|wire| wire.from_node == 2 && wire.to_port == "transform").count(), 2, "both fills read it");
        crate::flatten::flatten_groups(&graph).expect("the layer flattens");
    }

    /// One group input that placed two waters' columns: both waters read the
    /// role from it, and nothing wires a role into a transform.
    #[test]
    fn a_group_input_shared_by_two_waters_carries_the_role_to_both() {
        let mut graph = def(json!({"version": 3,
            "nodes": [column(2), {"id": 5, "nodeId": "Live Matter", "typeId": "group", "handle": "Live Matter", "group": {
                "interface": {"inputs": [{"name": "initialVolume", "portType": "Transform"}], "outputs": []},
                "nodes": [{"id": 0, "nodeId": "live_input", "typeId": GROUP_INPUT_TYPE_ID},
                    {"id": 1, "nodeId": "domain_a", "typeId": MATTER_DOMAIN_TYPE_ID},
                    {"id": 3, "nodeId": "domain_b", "typeId": MATTER_DOMAIN_TYPE_ID}],
                "wires": [wire(0, "initialVolume", 1, INITIAL_VOLUME_PORT), wire(0, "initialVolume", 3, INITIAL_VOLUME_PORT)]}}],
            "wires": [wire(2, "transform", 5, "initialVolume")]}));
        assert!(migrate_initial_volume(&mut graph));
        assert!(!carries_initial_volume(&graph));
        let live = graph.nodes.iter().find(|node| node.id == 5).unwrap().group.as_deref().unwrap();
        assert_eq!(live.interface.inputs.len(), 1);
        assert_eq!(live.interface.inputs[0].port_type, "FluidRole");
        assert_eq!(live.wires, [wire(0, "initialVolume", 1, "role_0"), wire(0, "initialVolume", 3, "role_0")]);
        assert_eq!(graph.nodes.iter().filter(|node| node.handle.as_deref().is_some_and(|h| h.starts_with(INITIAL_FILL_NAME))).count(), 1);
        crate::flatten::flatten_groups(&graph).expect("the layer flattens");
    }

    /// The moved column keeps its document id, so a card control stamped
    /// `{doc}_{param}` still counts as the node's own copy.
    #[test]
    fn the_moved_column_keeps_its_document_id() {
        let mut graph = def(json!({"version": 3,
            "nodes": [{"id": 1, "nodeId": "domain", "typeId": GPU_FLIP_DOMAIN_TYPE_ID}, column(52)],
            "wires": [wire(52, "transform", 1, INITIAL_VOLUME_PORT)]}));
        assert!(migrate_initial_volume(&mut graph));
        let (group, _) = fill(&graph.nodes, &graph.wires);
        let body = group.group.as_deref().unwrap();
        assert_eq!(body.nodes.iter().find(|node| node.node_id.as_str() == "initial_column").map(|node| node.id), Some(52));
        let ids: BTreeSet<u32> = body.nodes.iter().map(|node| node.id).collect();
        assert_eq!(ids.len(), body.nodes.len(), "the body's doc ids are distinct");
        crate::flatten::flatten_groups(&graph).expect("the layer flattens");
    }

    /// A document a flatten already produced (its handles carry the group
    /// path) takes the fill as plain nodes: a group added there would be
    /// flattened again and refuse the '/' in the handles already built. This
    /// is what a preset sees when it is flattened before it is instantiated.
    #[test]
    fn a_flattened_level_takes_the_fill_without_a_group() {
        for shared in [false, true] {
            let mut nodes = vec![
                json!({"id": 1, "nodeId": "domain", "typeId": GPU_FLIP_DOMAIN_TYPE_ID, "handle": "Water/Domain"}),
                column(2),
                json!({"id": 6, "nodeId": "lattice", "typeId": "node.scene_object", "handle": "Water/Liquid Surface/Lattice Box"}),
            ];
            let mut wires = vec![wire(2, "transform", 1, INITIAL_VOLUME_PORT)];
            if shared {
                nodes.push(json!({"id": 7, "nodeId": "box", "typeId": "node.scene_object"}));
                wires.push(wire(2, "transform", 7, "transform"));
            }
            let mut graph = def(json!({"version": 3, "nodes": nodes, "wires": wires}));
            assert!(migrate_initial_volume(&mut graph));
            assert!(graph.nodes.iter().all(|node| node.group.is_none()), "no group in a flattened level");
            let flat = crate::flatten::flatten_groups(&graph).expect("the migrated level flattens");
            let role = flat.nodes.iter().find(|node| node.type_id == FLUID_ROLE_SOURCE_TYPE_ID).expect("the role source");
            assert_eq!(role.handle.as_deref(), Some("Initial Fill/Initial Fill"));
            assert_eq!(role.params["role"], SerializedParamValue::Enum { value: INITIAL_FILL_ROLE });
            let ids: BTreeSet<u32> = flat.nodes.iter().map(|node| node.id).collect();
            assert_eq!(ids.len(), flat.nodes.len(), "doc ids are unique");
            assert!(flat.wires.contains(&wire(role.id, "role", 1, "role_0")));
            let column = flat.nodes.iter().find(|node| node.node_id.as_str() == "initial_column").expect("the column survives");
            assert!(flat.wires.contains(&wire(column.id, "transform", role.id, "transform")), "the column places the fill");
            assert_eq!(column.handle.as_deref().is_some_and(|h| h.starts_with("Initial Fill/")), !shared, "a lone column moves in, a shared one stays");
            assert!(!migrate_initial_volume(&mut graph), "a second load changes nothing");
        }
    }

    /// Two waters on one level get two fills with their own names, and a
    /// scene modifier's graph migrates too.
    #[test]
    fn every_water_gets_its_own_fill() {
        let level = |base: u32| json!([
            {"id": base, "nodeId": format!("domain_{base}"), "typeId": GPU_FLIP_DOMAIN_TYPE_ID},
            {"id": base + 1, "nodeId": format!("column_{base}"), "typeId": TRANSFORM_3D_TYPE_ID}]);
        let nodes: Vec<serde_json::Value> = level(1).as_array().unwrap().iter().chain(level(3).as_array().unwrap()).cloned().collect();
        let mut graph = def(json!({"version": 3, "nodes": nodes,
            "wires": [wire(2, "transform", 1, INITIAL_VOLUME_PORT), wire(4, "transform", 3, INITIAL_VOLUME_PORT)]}));
        let modifier = graph.clone();
        let force = json!({"id": "force", "scene": {"scope": [], "node": "scene"}, "targets": "allObjects", "graph": modifier});
        graph.scene_modifiers.push(serde_json::from_value(force).expect("modifier"));
        assert!(migrate_initial_volume(&mut graph));
        let handles: BTreeSet<_> = graph.nodes.iter().filter_map(|node| node.handle.clone()).collect();
        assert_eq!(handles, BTreeSet::from(["Initial Fill".to_owned(), "Initial Fill 2".to_owned()]));
        assert!(!carries_initial_volume(&graph), "the modifier's graph migrated");
    }
}
