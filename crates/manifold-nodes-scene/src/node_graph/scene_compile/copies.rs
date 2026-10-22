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

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, PhysicsSetting};
use manifold_core::scene_schema::{BODY_ROLE, COPIES_ROLE};

use super::builder::{Builder, SCENE};

/// `node.scene_object`'s `instances_space` value for a Group.
const GROUP_SPACE: u32 = 1;
/// `node.rigid_body`'s Body Type values.
const FIXED: u32 = 0;
const MOVING: u32 = 1;

fn add_body(b: &mut Builder<'_>, item: &Item, transform: u32, physics: PhysicsSetting) -> u32 {
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

/// One object inside a copied group: its mesh, placed by its transform inside
/// one copy, is one part of the copies body's shape.
pub(crate) fn add_part(b: &mut Builder<'_>, body: u32, mesh: u32, transform: u32) {
    // A swarm's copies node has no shape to size.
    if b.swarm_frames.contains_key(&body) {
        return;
    }
    b.copy_parts.entry(body).or_default().push((mesh, transform));
}

/// Make `object` draw `body`'s copies.
pub(crate) fn draw_instances(b: &mut Builder<'_>, body: u32, object: u32, group_space: bool) {
    if let Some(&emitter) = b.swarm_frames.get(&body) {
        // A swarm's particles live in its emitter's frame.
        b.wire(body, "copies", object, "instances");
        b.wire(emitter, "transform", object, "parent_transform");
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
