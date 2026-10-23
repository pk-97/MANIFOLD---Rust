//! Copies (SCENE_DOCUMENT_DESIGN.md section 7.9 (Copies)): an Object or Group
//! with Copies on gets a copies body, the `node.rigid_body` that lays the
//! copies out and publishes their instances. The body joins the scene's
//! Physics World as copy group `k`; the world publishes the group's
//! `instances` and `active_count`, and the draws read them through the
//! renderer's instance space (an Object draws each copy as itself, a Group
//! draws the whole group at each copy).
//!
//! The layout is the body's own: Count, Layout, Spacing, Columns and Turn are
//! params it reads every frame, so they stay live. The prototype's physics
//! setting applies to every copy: None rests the copies in their layout (the
//! body is disabled and the world draws their rest poses), a Collider holds
//! them fixed, a Body drops them as moving bodies. A copy group needs the
//! world even when nothing has physics, because the world is what places the
//! copies.
//!
//! On Surface replaces the layout: a `node.scatter_on_mesh` over the surface
//! item's mesh places the copies in that item's frame, and the body only counts
//! them (its Count drives the scatter's). Physics does not move copies on a
//! surface, so their body stays disabled.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::copies_body::MAX_COPIES;
use manifold_core::scene_doc::{Item, PhysicsSetting};
use manifold_core::scene_schema::{BODY_ROLE, COPIES_ROLE, COPIES_ROWS, Reach, SURFACE_ROLE};

use super::builder::{Builder, SCENE};

/// `node.scene_object`'s `instances_space` value for a Group.
const GROUP_SPACE: u32 = 1;
/// `node.rigid_body`'s Body Type values.
const FIXED: u32 = 0;
const MOVING: u32 = 1;

fn add_body(b: &mut Builder<'_>, item: &Item, transform: u32, physics: PhysicsSetting) -> u32 {
    let physics = if item.fields.copies_surface().is_some() { PhysicsSetting::None } else { physics };
    let owner = item.id.as_str();
    let body = b.add(owner, COPIES_ROLE, "node.rigid_body");
    b.wire(transform, "transform", body, "transform");
    b.set(body, "enabled", SerializedParamValue::Bool { value: physics != PhysicsSetting::None });
    let motion = if physics == PhysicsSetting::Collider { FIXED } else { MOVING };
    b.set(body, "motion", SerializedParamValue::Enum { value: motion });
    // The Physics section's rows (Mass, Bounce, Friction) set every copy.
    b.adopt(owner, BODY_ROLE, body);
    b.copy_bodies.push(body);
    body
}

/// An Object's copies: the body takes the object's transform and its solid,
/// and the object draws the body's instances in place of one pose. Returns
/// the body.
pub(crate) fn emit_object(
    b: &mut Builder<'_>,
    item: &Item,
    mesh: u32,
    transform: u32,
    object: u32,
    physics: PhysicsSetting,
) -> u32 {
    let body = add_body(b, item, transform, physics);
    b.wire(mesh, "source", body, "source");
    draw_instances(b, body, object, false);
    body
}

/// A Group's copies: the body takes the group's transform, every object
/// inside is one part of its shape ([`add_part`]), so the group's bounds size
/// one copy's spacing, and the group's draws read the body's instances in
/// Group space.
pub(crate) fn emit_group(b: &mut Builder<'_>, item: &Item, transform: u32, physics: PhysicsSetting) -> u32 {
    add_body(b, item, transform, physics)
}

/// A Group's copies laid out at rest about its own Transform, for a swarm
/// that draws the group to carry at every particle.
pub(crate) fn emit_layout(b: &mut Builder<'_>, item: &Item, transform: u32) -> u32 {
    add_body(b, item, transform, PhysicsSetting::None)
}

/// One object inside a copied group: its mesh, placed by its transform inside
/// one copy, is one part of the copies body's shape.
pub(crate) fn add_part(b: &mut Builder<'_>, body: u32, mesh: u32, transform: u32) {
    // A swarm drawing a group with copies sizes the group's own layout.
    let body = b.nests.get(&body).copied().flatten().unwrap_or(body);
    // A swarm's copies node has no shape to size.
    if b.swarm_frames.contains_key(&body) {
        return;
    }
    b.copy_parts.entry(body).or_default().push((mesh, transform));
}

/// Make `object` draw `body`'s copies.
pub(crate) fn draw_instances(b: &mut Builder<'_>, body: u32, object: u32, group_space: bool) {
    if let Some(&(emitter, live)) = b.swarm_frames.get(&body) {
        // A swarm's particles live in its emitter's frame.
        b.wire(body, "copies", object, "instances");
        b.wire(emitter, "transform", object, "parent_transform");
        // Only the live particles draw: Count, or the nest's live product.
        if b.nests.contains_key(&body) {
            b.wire(body, "count", object, "instance_count");
        } else {
            b.wire(live, "out", object, "instance_count");
        }
    } else {
        b.wire(body, "instances", object, "instances");
        b.wire(body, "active_count", object, "instance_count");
    }
    if group_space {
        b.set(object, "instances_space", SerializedParamValue::Enum { value: GROUP_SPACE });
    }
}

/// Wire every copies body into the Physics World as its own copy group, in
/// emission order, and size the world's copy-group family to match.
pub(crate) fn join_world(b: &mut Builder<'_>) {
    place_on_surfaces(b);
    scale_copies(b);
    if b.copy_bodies.is_empty() {
        return;
    }
    let world = b
        .doc
        .world()
        .and_then(|item| b.id(item.id.as_str(), "physics"))
        .or_else(|| b.id(SCENE, "physics"))
        .unwrap_or_else(|| b.add(SCENE, "physics", "node.physics_world"));
    for (body, parts) in std::mem::take(&mut b.copy_parts) {
        b.set(body, "parts", SerializedParamValue::Int { value: parts.len() as i32 });
        for (k, (mesh, transform)) in parts.into_iter().enumerate() {
            b.wire(mesh, "source", body, &format!("mesh_{k}"));
            b.wire(transform, "transform", body, &format!("part_{k}"));
        }
    }
    let bodies = std::mem::take(&mut b.copy_bodies);
    for (group, body) in bodies.iter().enumerate() {
        b.wire(*body, "body", world, &format!("copies_{group}"));
    }
    b.set_number(world, "copy_groups", bodies.len() as f32);
}

/// Draw every laid-out copy at its prototype's per-axis scale: the world's
/// instances carry one uniform scale, and the body's `copy_scale` the rest,
/// applied inside each copy in Group space. Copies on a surface draw by the
/// surface's instances and the item's own transform instead
/// ([`place_on_surfaces`] has already rewired them).
fn scale_copies(b: &mut Builder<'_>) {
    let doc = b.doc;
    let mut copied = Vec::new();
    doc.walk(|item, _| copied.push(item));
    for item in copied {
        let Some(body) = b.id(item.id.as_str(), COPIES_ROLE) else { continue };
        for object in draws_of(b, item, body) {
            b.wire(body, "copy_scale", object, "copy_scale");
            b.set(object, "instances_space", SerializedParamValue::Enum { value: GROUP_SPACE });
        }
    }
}

/// Sit the copies of every item with On Surface set on that item's surface:
/// a scatter over the surface's mesh places them in the surface's frame, in
/// place of the body's layout. A surface with no mesh leaves the copies to
/// their Layout.
fn place_on_surfaces(b: &mut Builder<'_>) {
    let doc = b.doc;
    let mut on_surface = Vec::new();
    doc.walk(|item, _| {
        if let Some(surface) = item.fields.copies_surface() {
            on_surface.push((item, surface));
        }
    });
    for (item, surface) in on_surface {
        let owner = item.id.as_str();
        let (Some(body), Some(target)) = (b.id(owner, COPIES_ROLE), doc.find(surface)) else {
            continue;
        };
        let Some(mesh) = super::swarm::first_mesh(b, target) else {
            continue;
        };
        let Some(frame) = super::swarm::emit_frame(b, owner, target) else {
            continue;
        };
        let draws = draws_of(b, item, body);
        let scatter = b.add(owner, SURFACE_ROLE, "node.scatter_on_mesh");
        b.wire(mesh.0, mesh.1, scatter, "vertices");
        b.set_number(scatter, "max_capacity", MAX_COPIES as f32);
        b.follow(body, "copy_count", scatter, "count");
        b.apply_rows(item, COPIES_ROWS.iter().filter(|row| row.reach == Reach::Own(SURFACE_ROLE) && (row.applies)(&item.fields)));
        b.splice(body, "instances", scatter, "instances");
        // Each copy is the surface's frame, then its point on the surface,
        // then the copied item's own transform.
        let own = b.id(owner, "transform");
        for object in draws {
            b.set(object, "instances_space", SerializedParamValue::Enum { value: GROUP_SPACE });
            b.wire(frame, "transform", object, "parent_transform");
            match b.input(object, "transform") {
                // A part inside a copied group: the group's transform leads
                // its chain.
                Some((first, _)) => {
                    let mut root = first;
                    while let Some((above, _)) = b.input(root, "parent") {
                        root = above;
                    }
                    if let Some(own) = own.filter(|own| *own != root) {
                        b.wire(own, "transform", root, "parent");
                    }
                }
                // A copied Object, which draws no transform of its own.
                None => {
                    if let Some(own) = own {
                        b.wire(own, "transform", object, "transform");
                    }
                }
            }
        }
    }
}

/// The objects that draw `body`'s copies: the copied Object, or every part
/// of the copied Group.
fn draws_of(b: &Builder<'_>, item: &Item, body: u32) -> Vec<u32> {
    let mut draws = Vec::new();
    let mut visit = vec![item];
    while let Some(next) = visit.pop() {
        if let Some(object) = b.id(next.id.as_str(), "object")
            && b.input(object, "instances").is_some_and(|(from, port)| from == body && port == "instances")
        {
            draws.push(object);
        }
        visit.extend(next.children.iter());
    }
    draws
}
