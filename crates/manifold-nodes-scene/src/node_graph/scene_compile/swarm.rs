//! Swarm (SCENE_DOCUMENT_DESIGN.md section 7.11 (Swarm)): a clock and force
//! lattice over a bounds box, a seeded particle loop the scene's forces
//! move, and copies placed at the particles, drawn by one scene object or by
//! the Object or Group the Draw As field names.
//!
//! The swarm simulates in its emitter's frame, the item's Transform, and is
//! drawn through that frame, so moving, turning or scaling the swarm carries
//! the particles, the forces' lattice and the drawn copies together. The
//! bounds box is fixed in that frame, wider than the seed box (the lattice
//! holds its edge value outside it, so the Attractor and the gusts keep their
//! shape over the whole stirred cloud). Forces are ordinary scene forces: the
//! load passes lay every force that targets everything onto the swarm's
//! lattice (`forces.rs` compiles them). They act in the world: the lattice
//! samples them where each node sits in the world and turns them into the
//! emitter's frame, so a Wind blows the same way however the swarm turns
//! (Blender's force fields). The World's gravity, still air and clock reach
//! the forces node through value nodes that follow the World's card rows.
//!
//! Emit From a Box fills its volume; a Sphere, an Object or a Group emits
//! from the surface, a Group from every part at its own place. A Group with
//! Copies on drawn by a swarm keeps them: its layout rides every particle.
//!
//! Two phases: [`emit_all`] before the item tree (a Draw As item must know it
//! is drawn by a swarm), [`finish`] after it (Emit From an item reads that
//! item's compiled mesh and frame).

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{DrawAs, EmitFrom, Item, ItemFields, ItemId};
use manifold_core::scene_schema::SWARM_MAX_COUNT;
use manifold_core::scene_world_controls::WORLD_INPUTS;

use super::builder::Builder;
use super::material::surface_for;

/// `node.spawn_from_mesh`'s Modes: surface scatter, volume fill.
const SURFACE: u32 = 1;
const VOLUME: u32 = 2;
/// `node.platonic_solid_mesh`'s shapes: Tetrahedron 0, Cube 1, Octahedron 2,
/// Icosahedron 3.
const TETRAHEDRON: u32 = 0;
const CUBE: u32 = 1;
const OCTAHEDRON: u32 = 2;
const ICOSAHEDRON: u32 = 3;
/// The seed box, in the emitter's frame.
const SEED_SIZE: f32 = 4.0;
/// The lattice box: 1.5 times the seed box.
const BOUNDS_SIZE: f32 = 6.0;

/// The forces node's clock and field scalars go to the mover port for port;
/// `epoch` also restarts the seed and the particle loop.
const CLOCK_PORTS: [&str; 14] = [
    "forces", "ticks", "first_tick", "force_lattices", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing",
    "field_min_x", "field_min_y", "field_min_z", "tick_seconds", "impulses", "impulse_tick",
];

/// The value node that follows each World control a swarm takes, in
/// `WORLD_INPUTS` order.
const FOLLOWS: [&str; 6] =
    ["world_gravity_x", "world_gravity_y", "world_gravity_z", "world_air_resistance", "world_speed", "world_reset"];

/// Emit every Swarm's particle loop and its draw, in tree order.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let mut found = Vec::new();
    b.doc.walk(|item, _| {
        if matches!(item.fields, ItemFields::Swarm(_)) {
            found.push(item.clone());
        }
    });
    for item in &found {
        emit(b, item);
    }
}

/// The mesh node each particle draws as, and the platonic solid index where
/// it is one. `None` when the swarm draws an item.
fn particle_mesh(draw: &DrawAs) -> Option<(&'static str, Option<u32>)> {
    const SOLID: &str = "node.platonic_solid_mesh";
    Some(match draw {
        DrawAs::Tetrahedron => (SOLID, Some(TETRAHEDRON)),
        DrawAs::Cube => (SOLID, Some(CUBE)),
        DrawAs::Octahedron => (SOLID, Some(OCTAHEDRON)),
        DrawAs::Icosahedron => (SOLID, Some(ICOSAHEDRON)),
        DrawAs::Sphere => ("node.sphere_mesh", None),
        DrawAs::Item(_) => return None,
    })
}

fn emit(b: &mut Builder<'_>, item: &Item) {
    let ItemFields::Swarm(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let transform = b.add(owner, "transform", "node.transform_3d");
    let bounds = b.add(owner, "bounds", "node.transform_3d");
    for axis in ["scale_x", "scale_y", "scale_z"] {
        b.set_number(bounds, axis, BOUNDS_SIZE);
    }
    let forces = b.add(owner, "forces", "node.swarm_forces");
    b.wire(bounds, "transform", forces, "bounds");
    // Forces are laid where the lattice sits in the world and turned into
    // the emitter's frame, so they act in world space.
    b.wire(transform, "transform", forces, "frame");

    let seed = b.add(owner, "seed", "node.spawn_from_mesh");
    // A Box fills its volume; a Sphere and an item emit from their surface.
    let mode = if fields.emit_from == EmitFrom::Box { VOLUME } else { SURFACE };
    b.set(seed, "mode", SerializedParamValue::Enum { value: mode });
    b.set_number(seed, "max_capacity", SWARM_MAX_COUNT);
    match &fields.emit_from {
        EmitFrom::Box => {
            let start = b.add(owner, "start", "node.cube_mesh");
            b.set_number(start, "size", SEED_SIZE);
            b.wire(start, "vertices", seed, "vertices");
        }
        EmitFrom::Sphere => {
            let start = b.add(owner, "start", "node.sphere_mesh");
            b.set_number(start, "radius", SEED_SIZE / 2.0);
            b.wire(start, "vertices", seed, "vertices");
        }
        // The item's mesh is wired once it has compiled ([`finish`]).
        EmitFrom::Item(_) => {}
    }
    let state = b.add(owner, "state", "node.array_feedback");
    let mover = b.add(owner, "move", "node.move_swarm");
    b.set_number(mover, "active_count", SWARM_MAX_COUNT);
    let copies = b.add(owner, "copies", "node.swarm_to_copies");
    b.wire(forces, "epoch", seed, "reset_trigger");
    b.wire(forces, "epoch", state, "reset_trigger");
    b.wire(seed, "particles", state, "seed");
    b.wire(state, "out", mover, "in");
    b.wire(mover, "out", state, "in");
    b.wire(mover, "out", copies, "particles");
    for port in CLOCK_PORTS {
        b.wire(forces, port, mover, port);
    }

    match particle_mesh(&fields.draw_as) {
        Some((mesh_type, solid)) => {
            let mesh = b.add(owner, "shape", mesh_type);
            if let Some(solid) = solid {
                b.set(mesh, "shape", SerializedParamValue::Enum { value: solid });
            }
            let object = b.add(owner, "object", "node.scene_object");
            b.wire(mesh, "vertices", object, "vertices");
            b.wire(copies, "copies", object, "instances");
            b.wire(transform, "transform", object, "parent_transform");
            let (surface, maps) = surface_for(b, &item.id);
            b.wire(surface, "out", object, "material");
            for (source, port) in maps {
                b.wire(source, "out", object, port);
            }
            b.objects.push(object);
            b.object_items.push(item.id.clone());
        }
        None => {
            if let DrawAs::Item(prototype) = &fields.draw_as {
                // A Group with Copies on keeps them: its whole layout rides
                // every particle ([`nest_layout`] wires the layout in).
                let draws = match b.doc.find(prototype).map(|item| &item.fields) {
                    Some(ItemFields::Group(group)) if group.copies => {
                        let nest = b.add(owner, "nest", "node.nest_copies");
                        b.wire(copies, "copies", nest, "outer");
                        b.nests.insert(nest, None);
                        nest
                    }
                    _ => copies,
                };
                b.swarm_draws.insert(prototype.clone(), draws);
                b.swarm_frames.insert(draws, transform);
            }
        }
    }
    b.swarms.push(item.id.clone());
    b.rows(item);
}

/// Place each swarm in its frame and read the mesh an item emits from,
/// once the item tree has compiled.
pub(crate) fn finish(b: &mut Builder<'_>) {
    let doc = b.doc;
    for id in b.swarms.clone() {
        let Some(item) = doc.find(&id) else {
            continue;
        };
        let ItemFields::Swarm(fields) = &item.fields else {
            continue;
        };
        let owner = id.as_str();
        let (Some(transform), Some(seed)) = (b.id(owner, "transform"), b.id(owner, "seed")) else {
            continue;
        };
        let enclosing = nearest_group(doc, &id);
        let mut frame = enclosing.as_ref().and_then(|group| b.group_frames.get(group).copied());
        if let EmitFrom::Item(target) = &fields.emit_from
            && let Some(target) = doc.find(target)
        {
            if let Some(mesh) = emit_mesh(b, owner, target) {
                b.wire(mesh, "vertices", seed, "vertices");
            }
            frame = emit_frame(b, owner, target).or(frame);
        }
        if let Some(frame) = frame {
            b.wire(frame, "transform", transform, "parent");
        }
        if let (Some(group), Some(object)) = (enclosing, b.id(owner, "object"))
            && let Some(visible) = b.group_visible.get(&group).copied()
        {
            b.wire(visible, "out", object, "parent_visible");
        }
    }
}

/// The Group closest above `id`.
fn nearest_group(doc: &manifold_core::scene_doc::SceneDoc, id: &ItemId) -> Option<ItemId> {
    let mut found = None;
    doc.walk(|candidate, ancestors| {
        if candidate.id == *id {
            found = ancestors.iter().rev().find(|holder| matches!(holder.fields, ItemFields::Group(_))).map(|group| group.id.clone());
        }
    });
    found
}

/// The node whose `vertices` a swarm emitting from `target` seeds from: an
/// Object's own mesh, or every part of a Group merged in the group's frame,
/// each where it sits in the group.
fn emit_mesh(b: &mut Builder<'_>, owner: &str, target: &Item) -> Option<u32> {
    if matches!(target.fields, ItemFields::Object(_)) {
        return b.id(target.id.as_str(), "mesh");
    }
    let ItemFields::Group(group) = &target.fields else {
        return None;
    };
    // Where the group's own copies or a swarm place it, everything inside
    // sits in the group's frame already; otherwise each part is drawn
    // inside its nearest group's frame, and the merge brings it into this
    // group's.
    let in_group = group.copies || b.swarm_draws.contains_key(&target.id);
    let frame = (!in_group).then(|| b.group_frames.get(&target.id).copied()).flatten();
    // A group with Combine Meshes on is one shape, placed by its combined
    // transform inside the group.
    let parts = match (b.id(target.id.as_str(), "mesh"), b.id(target.id.as_str(), "combined_transform")) {
        (Some(shape), Some(placed)) => vec![(shape, placed, frame.filter(|_| !b.wired_into(placed, "parent")))],
        _ => {
            let mut parts = Vec::new();
            parts_below(b, target, in_group, &mut parts);
            parts
        }
    };
    if parts.is_empty() {
        return None;
    }
    let merge = b.add(owner, "emit_parts", "node.merge_meshes");
    b.set_number(merge, "parts", parts.len() as f32);
    for (k, (mesh, placed, within)) in parts.into_iter().enumerate() {
        b.wire(mesh, "vertices", merge, &format!("mesh_{k}"));
        b.wire(placed, "transform", merge, &format!("place_{k}"));
        if let Some(within) = within {
            b.wire(within, "transform", merge, &format!("within_{k}"));
        }
    }
    if let Some(frame) = frame {
        b.wire(frame, "transform", merge, "frame");
    }
    // Merge only when the seed reads it.
    if let Some(forces) = b.id(owner, "forces") {
        b.wire(forces, "epoch", merge, "reset_trigger");
    }
    Some(merge)
}

/// Every Object at or below `item` with a mesh: (mesh node, transform node,
/// the frame it is drawn in when its transform does not already carry it).
fn parts_below(b: &Builder<'_>, item: &Item, in_group: bool, out: &mut Vec<(u32, u32, Option<u32>)>) {
    if matches!(item.fields, ItemFields::Object(_))
        && let (Some(mesh), Some(placed)) = (b.id(item.id.as_str(), "mesh"), b.id(item.id.as_str(), "transform"))
    {
        let within = if in_group || b.wired_into(placed, "parent") {
            None
        } else {
            nearest_group(b.doc, &item.id).and_then(|group| b.group_frames.get(&group).copied())
        };
        out.push((mesh, placed, within));
    }
    for child in &item.children {
        parts_below(b, child, in_group, out);
    }
}

/// Lay out a Group's own copies for the swarm that draws it: a copies body
/// at rest about the group's own `transform`, so the group's offset, turn
/// and scale ride with every particle as a Blender instanced collection's
/// objects keep theirs. The particles move the copies, so the body runs no
/// simulation of its own. `nest` places its instances at every particle.
pub(crate) fn nest_layout(b: &mut Builder<'_>, item: &Item, transform: u32, nest: u32) {
    let body = super::copies::emit_layout(b, item, transform);
    b.wire(body, "instances", nest, "inner");
    b.wire(body, "active_count", nest, "inner_count");
    b.nests.insert(nest, Some(body));
}

/// The mesh node of the first Object at or below `item` that compiled one.
pub(crate) fn first_mesh(b: &Builder<'_>, item: &Item) -> Option<u32> {
    if matches!(item.fields, ItemFields::Object(_))
        && let Some(mesh) = b.id(item.id.as_str(), "mesh")
    {
        return Some(mesh);
    }
    item.children.iter().find_map(|child| first_mesh(b, child))
}

/// The frame an emit-from item's mesh sits in: an Object's own Transform
/// inside the frame of the groups around it, a Group's frame.
pub(crate) fn emit_frame(b: &mut Builder<'_>, owner: &str, target: &Item) -> Option<u32> {
    if !matches!(target.fields, ItemFields::Object(_)) {
        return b.group_frames.get(&target.id).copied();
    }
    let transform = b.id(target.id.as_str(), "transform")?;
    let frame = b.add(owner, "emit_frame", "node.transform_3d");
    b.mirror(transform, frame);
    let around = nearest_group(b.doc, &target.id);
    if let Some(parent) = around.and_then(|group| b.group_frames.get(&group).copied()) {
        b.wire(parent, "transform", frame, "parent");
    }
    Some(frame)
}

/// Make every swarm follow the World's gravity, still air and clock: a value
/// node per control that the World's card row also drives.
pub(crate) fn follow_world(b: &mut Builder<'_>, world: u32) {
    for id in b.swarms.clone() {
        let owner = id.as_str();
        let Some(forces) = b.id(owner, "forces") else {
            continue;
        };
        for (control, role) in WORLD_INPUTS.iter().zip(FOLLOWS) {
            let value = b.add(owner, role, "node.value");
            b.follow(world, control.world, value, "value");
            b.wire(value, "out", forces, control.input);
        }
    }
}
