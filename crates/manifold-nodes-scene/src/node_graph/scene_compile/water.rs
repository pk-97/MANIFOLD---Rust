//! Water and Water Source (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water,
//! Water Source, Ocean)): each Water splices one copy of the water family
//! template for its Solver and Draw As (`scene_schema::water_family`), its
//! node ids `<water id>:<template node id>`, and draws the template's
//! objects. Its Transform node is the domain box. Each Water Source child
//! compiles to a `node.fluid_role_source` placed by its own Transform (a
//! Model's by its Object's), wired into the domain's role inputs in tree
//! order; the domain has a role input per source.
//!
//! Inside a Group, the domain box and the sources sit in the group's frame
//! and the water follows the group's visibility. The domain stays
//! axis-aligned: a turned group moves the box but does not turn it.
//!
//! The water draws with its Material, as any object does; with none it
//! keeps the template's water look. Solids reach the water through the
//! World (section 7.8): the coupling install pairs every Water with the
//! World's Colliders and Bodies.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_core::scene_doc::{Item, ItemFields, ItemKind, SceneDoc, SourceShape};
use manifold_core::scene_schema::{
    DOMAIN_ROLE, PHYSICS_VIEW_SWITCHES, SOLID, SURFACE_DETAIL_NODES, SURFACE_DETAIL_OFFSET,
    WATER_MATERIAL_ROLE, WATER_OBJECT_ROLE, WORLD_FOLLOWS, WaterFamily, role_index, template_roles,
    water_family,
};
use manifold_core::scene_world_controls::WORLD_INPUTS;
use manifold_foundation::ItemId;
use manifold_node_engine::scene::render_mode::RENDER_MODE_PHYSICS_VIEW;

use super::builder::Builder;
use super::material::surface_for;

/// Every Water in tree order, with the Group it sits in, if any.
pub(crate) fn waters(doc: &SceneDoc) -> Vec<(Item, Option<ItemId>)> {
    let mut found = Vec::new();
    doc.walk(|item, ancestors| {
        if matches!(item.fields, ItemFields::Water(_)) {
            let group = ancestors
                .iter()
                .rev()
                .find(|ancestor| ancestor.kind() == ItemKind::Group)
                .map(|group| group.id.clone());
            found.push((item.clone(), group));
        }
    });
    found
}

/// Emit every Water and its sources. With no water solver linked, a Water
/// draws nothing.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    for (item, group) in waters(b.doc) {
        let ItemFields::Water(fields) = &item.fields else {
            continue;
        };
        if let Some(family) = water_family(fields.solver, fields.draw) {
            emit(b, &item, group.as_ref(), &family);
        }
    }
}

fn emit(b: &mut Builder<'_>, item: &Item, group: Option<&ItemId>, family: &WaterFamily) {
    let owner = item.id.as_str();
    let ids = splice(b, owner, family);
    let node = |name: &str| ids.get(name).copied();
    for role in template_roles()
        .chain(PHYSICS_VIEW_SWITCHES)
        .chain(SURFACE_DETAIL_NODES)
    {
        if let Some(id) = node(role) {
            b.adopt(owner, role, id);
        }
    }
    let Some(domain) = node(DOMAIN_ROLE) else {
        return;
    };
    let frame = group.and_then(|group| b.group_frames.get(group).copied());
    let visible = group.and_then(|group| b.group_visible.get(group).copied());
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(frame) = frame {
        b.wire(frame, "transform", transform, "parent");
    }
    b.wire(transform, "transform", domain, "domain");

    let cell_size = b.add(owner, "cell_size", "node.value");
    b.wire(cell_size, "out", domain, "cell_metres");
    if let ItemFields::Water(water) = &item.fields {
        b.set(
            domain,
            "bounds",
            SerializedParamValue::Bool {
                value: water.bounds,
            },
        );
    }

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

    let sources: Vec<&Item> = item
        .children
        .iter()
        .filter(|child| matches!(child.fields, ItemFields::WaterSource(_)))
        .collect();
    b.set(
        domain,
        "roles",
        SerializedParamValue::Int {
            value: sources.len() as i32,
        },
    );
    for (port, source) in sources.into_iter().enumerate() {
        emit_source(b, source, domain, port, frame);
    }

    // The water's look is its Material's, as for any object.
    if b.doc.effective_material(&item.id).is_some()
        && let (Some(look), Some(water)) = (node(WATER_MATERIAL_ROLE), node(WATER_OBJECT_ROLE))
    {
        let (surface, maps) = surface_for(b, &item.id);
        b.splice(look, "out", surface, "out");
        for (source, port) in maps {
            b.wire(source, "out", water, port);
        }
    }

    for object in &family.objects {
        if let Some(id) = node(object) {
            if let Some(visible) = visible {
                b.wire(visible, "out", id, "parent_visible");
            }
            b.objects.push(id);
            b.object_items.push(item.id.clone());
        }
    }
    // Surface Detail drives every surface node from one value, shown from 0.
    if family.holds(SURFACE_DETAIL_NODES[0]) {
        b.add(owner, "surface_detail", "node.value");
    }
    b.rows(item);
    if let Some(detail) = b.id(owner, "surface_detail") {
        for target in SURFACE_DETAIL_NODES {
            if let Some(target) = b.id(owner, target) {
                b.follow_offset(
                    detail,
                    "value",
                    target,
                    "resolution_scale",
                    SURFACE_DETAIL_OFFSET,
                );
            }
        }
    }
}

/// One source: its Transform places a `node.fluid_role_source` (a unit box,
/// or a ball one metre across) wired into the domain's `role_<port>`. A
/// Model takes its Object's mesh and stands where the Object does, in the
/// frame of the groups round it; with no Object picked it is off.
fn emit_source(b: &mut Builder<'_>, item: &Item, domain: u32, port: usize, frame: Option<u32>) {
    let ItemFields::WaterSource(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(frame) = frame {
        b.wire(frame, "transform", transform, "parent");
    }
    let role = b.add(owner, "role", "node.fluid_role_source");
    b.set(
        role,
        "role",
        SerializedParamValue::Enum {
            value: role_index(fields.role),
        },
    );
    match fields.shape {
        SourceShape::Box => b.wire(transform, "transform", role, "transform"),
        SourceShape::Sphere => {
            b.wire(transform, "transform", role, "transform");
            let sphere = b.add(owner, "sphere", "node.sphere_mesh");
            b.wire(sphere, "source", role, "mesh_0");
        }
        SourceShape::Model => {
            let target = fields.model.as_ref().and_then(|model| b.doc.find(model));
            let mesh = target.and_then(|target| {
                let id = target.id.as_str();
                b.id(id, "mesh").or_else(|| b.id(id, SOLID))
            });
            let placed = target.zip(mesh).and_then(|(target, mesh)| {
                Some((super::swarm::emit_frame(b, owner, target)?, mesh))
            });
            match placed {
                Some((frame, mesh)) => {
                    b.wire(frame, "transform", role, "transform");
                    b.wire(mesh, "source", role, "mesh_0");
                }
                None => {
                    b.wire(transform, "transform", role, "transform");
                    b.set(role, "enabled", SerializedParamValue::Bool { value: false });
                }
            }
        }
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
        wire.from_node = level
            .get(&wire.from_node)
            .copied()
            .unwrap_or(wire.from_node);
        wire.to_node = level.get(&wire.to_node).copied().unwrap_or(wire.to_node);
    }
}

/// Make every Water follow the World: gravity, still air, speed and reset
/// through its value nodes, and the World's Render Mode node, whose Physics View draws
/// the particle dots instead of the surface. The switches sit at the mode
/// less Physics View's index plus one, so only Physics View reaches 1; the
/// side that is off is never computed.
pub(crate) fn follow_world(b: &mut Builder<'_>, world: Option<u32>, mode: u32) {
    for (item, _) in waters(b.doc) {
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
                b.follow_offset(
                    mode,
                    "mode",
                    switch,
                    "selector",
                    1.0 - RENDER_MODE_PHYSICS_VIEW as f32,
                );
            }
        }
    }
}
