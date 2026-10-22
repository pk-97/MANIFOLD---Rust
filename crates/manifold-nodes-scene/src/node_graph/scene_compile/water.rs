//! Water and Water Source (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water,
//! Water Source, Ocean)): each Water splices one copy of the water family
//! template (`scene_schema::water_family`), its node ids
//! `<water id>:<template node id>`, and draws the template's objects. Its
//! Transform node is the domain box. Each Water Source child compiles to a
//! `node.fluid_role_source` placed by its own Transform, wired into the
//! domain's role inputs in tree order.
//!
//! The Water and its sources sit in world space: the domain is an
//! axis-aligned box the solvers take as it is, so a Group around a Water
//! does not move or turn it.
//!
//! Solids reach the water through the World (section 7.8): every Collider
//! and Body in the World couples into the first Water in tree order, which
//! owns the rigid world while it runs. A second Water runs without solids.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_core::fluid_domain::MAX_FLUID_ROLES;
use manifold_core::scene_doc::{Item, ItemFields, SceneDoc, SourceShape};
use manifold_core::scene_schema::{
    DOMAIN_ROLE, PHYSICS_VIEW_SWITCHES, SURFACE_DETAIL_NODES, WORLD_FOLLOWS, WaterFamily, role_index, template_roles,
    water_family,
};
use manifold_core::scene_world_controls::WORLD_INPUTS;
use manifold_node_engine::scene::render_mode::RENDER_MODE_PHYSICS_VIEW;

use super::builder::Builder;

/// Every Water in tree order.
pub(crate) fn waters(doc: &SceneDoc) -> Vec<Item> {
    let mut found = Vec::new();
    doc.walk(|item, _| {
        if matches!(item.fields, ItemFields::Water(_)) {
            found.push(item.clone());
        }
    });
    found
}

/// Emit every Water and its sources. With no water solver linked, a Water
/// draws nothing.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let Some(family) = water_family() else {
        return;
    };
    for item in waters(b.doc) {
        emit(b, &item, &family);
    }
}

fn emit(b: &mut Builder<'_>, item: &Item, family: &WaterFamily) {
    let owner = item.id.as_str();
    let ids = splice(b, owner, family);
    let node = |name: &str| ids.get(name).copied();
    for role in template_roles().chain(PHYSICS_VIEW_SWITCHES).chain(SURFACE_DETAIL_NODES) {
        if let Some(id) = node(role) {
            b.adopt(owner, role, id);
        }
    }
    let (Some(domain), Some(whitewater), Some(state)) = (node(DOMAIN_ROLE), node("whitewater"), node("state")) else {
        return;
    };
    let transform = b.add(owner, "transform", "node.transform_3d");
    b.wire(transform, "transform", domain, "domain");

    let resolution = b.add(owner, "resolution", "node.value");
    b.wire(resolution, "out", domain, "resolution");
    let capacity = b.add(owner, "whitewater_capacity", "node.math");
    b.wire(resolution, "out", capacity, "a");
    b.wire(capacity, "out", whitewater, "capacity");
    b.wire(capacity, "out", state, "whitewater_capacity");

    // Speed is the Water's own times the World's.
    let speed = b.add(owner, "speed", "node.math");
    b.wire(speed, "out", domain, "speed");
    for (control, role) in WORLD_INPUTS.iter().zip(WORLD_FOLLOWS) {
        let value = b.add(owner, role, "node.value");
        if control.input == "speed" {
            b.wire(value, "out", speed, "a");
        } else {
            b.wire(value, "out", domain, control.input);
        }
    }

    let mut port = 0;
    for child in &item.children {
        if !matches!(child.fields, ItemFields::WaterSource(_)) {
            continue;
        }
        if port < MAX_FLUID_ROLES {
            emit_source(b, child, domain, port);
        }
        port += 1;
    }

    for object in &family.objects {
        if let Some(id) = node(object) {
            b.objects.push(id);
            b.object_items.push(item.id.clone());
        }
    }
    b.rows(item);
    if let Some(volume) = b.id(owner, SURFACE_DETAIL_NODES[0]) {
        for other in &SURFACE_DETAIL_NODES[1..] {
            if let Some(other) = b.id(owner, other) {
                b.follow(volume, "resolution_scale", other, "resolution_scale");
            }
        }
    }
}

/// One source: its Transform places a `node.fluid_role_source` (a unit box,
/// or a ball one metre across) wired into the domain's `role_<port>`.
fn emit_source(b: &mut Builder<'_>, item: &Item, domain: u32, port: usize) {
    let ItemFields::WaterSource(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    let role = b.add(owner, "role", "node.fluid_role_source");
    b.set(role, "role", SerializedParamValue::Enum { value: role_index(fields.role) });
    b.wire(transform, "transform", role, "transform");
    if fields.shape == SourceShape::Sphere {
        let sphere = b.add(owner, "sphere", "node.sphere_mesh");
        b.wire(sphere, "source", role, "mesh_0");
    }
    b.wire(role, "role", domain, &format!("role_{port}"));
    b.rows(item);
}

/// Add a copy of the template with fresh doc ids and `<owner>:` node ids;
/// returns each template node id's new doc id, at any depth.
fn splice(b: &mut Builder<'_>, owner: &str, family: &WaterFamily) -> BTreeMap<String, u32> {
    let mut ids = BTreeMap::new();
    let mut nodes = family.nodes.clone();
    let mut wires = family.wires.clone();
    renumber(b, owner, &mut nodes, &mut wires, &mut ids, true);
    for node in nodes {
        b.push_node(node);
    }
    for wire in wires {
        b.wire(wire.from_node, &wire.from_port, wire.to_node, &wire.to_port);
    }
    ids
}

fn renumber(
    b: &mut Builder<'_>,
    owner: &str,
    nodes: &mut [EffectGraphNode],
    wires: &mut [EffectGraphWire],
    ids: &mut BTreeMap<String, u32>,
    top: bool,
) {
    let mut level = BTreeMap::new();
    for node in nodes.iter_mut() {
        let fresh = b.fresh();
        level.insert(node.id, fresh);
        node.id = fresh;
        if !node.node_id.as_str().is_empty() {
            ids.insert(node.node_id.as_str().to_owned(), fresh);
            let scoped = format!("{owner}:{}", node.node_id.as_str());
            // Handles are unique within a level and a group's params route
            // by its inner handles, so only the top level's change.
            if top {
                node.handle = Some(scoped.clone());
            }
            node.node_id = manifold_core::NodeId::new(scoped);
        }
        if let Some(body) = node.group.as_deref_mut() {
            renumber(b, owner, &mut body.nodes, &mut body.wires, ids, false);
        }
    }
    for wire in wires.iter_mut() {
        wire.from_node = level.get(&wire.from_node).copied().unwrap_or(wire.from_node);
        wire.to_node = level.get(&wire.to_node).copied().unwrap_or(wire.to_node);
    }
}

/// Make every Water follow the World: gravity, still air, speed and reset
/// through its value nodes, and the Render Mode, whose Physics View draws
/// the particle dots instead of the surface. The switches sit at the mode
/// less Physics View's index plus one, so only Physics View reaches 1; the
/// side that is off is never computed.
pub(crate) fn follow_world(b: &mut Builder<'_>, world: Option<u32>) {
    let render = b.scene("render");
    for item in waters(b.doc) {
        let owner = item.id.as_str();
        if let Some(world) = world {
            for (control, role) in WORLD_INPUTS.iter().zip(WORLD_FOLLOWS) {
                if let Some(value) = b.id(owner, role) {
                    b.follow(world, control.world, value, "value");
                }
            }
        }
        for switch in PHYSICS_VIEW_SWITCHES {
            if let Some(switch) = b.id(owner, switch) {
                b.follow_offset(render, "render_mode", switch, "selector", 1.0 - RENDER_MODE_PHYSICS_VIEW as f32);
            }
        }
    }
}
