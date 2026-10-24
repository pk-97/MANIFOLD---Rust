//! Physics (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision
//! shapes)): each body an item's physics setting makes, as a
//! `node.rigid_body` playing role `body` for the item, joined to the World's
//! Physics World. Which item is which body is
//! `manifold_core::scene_schema::physics_parts`; the card rows are its table.
//!
//! - An Object with Collider or Body is its own body. Its mesh's `source` is
//!   the body's shape, its transform (in the enclosing group's frame) the
//!   body's start, and it draws at the body's solved pose.
//! - A group with Collider or Body is one body of everything inside it: each
//!   Object inside is one mesh part (`mesh_k`, `part_k`), placed by its
//!   transform inside the group. A nested group inside gets a second
//!   transform (`body_local`) bound to the same card rows, so parts sit in the
//!   body's frame while the drawn objects sit in the solved pose.
//! - A Fixed body never moves, so a Collider draws where it is authored.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemId, PhysicsSetting, SceneDoc};
use manifold_core::scene_schema::{BODY_ROLE, PHYSICS_ROWS, PhysicsPart, physics_parts};
use std::collections::HashMap;

use super::builder::Builder;
use super::group::Frame;

const BODY_TYPE: &str = "node.rigid_body";
const TRANSFORM_TYPE: &str = "node.transform_3d";
/// `node.rigid_body`'s Body Type: Fixed and Moving.
const FIXED: u32 = 0;
const MOVING: u32 = 1;

/// What the compiler knows about physics while it walks the tree.
#[derive(Default)]
pub(crate) struct State {
    parts: HashMap<ItemId, PhysicsPart>,
    /// Every body, in tree order: the World's `body_k`.
    bodies: Vec<u32>,
    /// Each group body's parts, (mesh node, transform node), in tree order.
    group_parts: Vec<(ItemId, Vec<(u32, u32)>)>,
}

impl State {
    pub(crate) fn new(doc: &SceneDoc) -> Self {
        Self {
            parts: physics_parts(doc),
            ..Self::default()
        }
    }

    fn part(&self, item: &Item) -> PhysicsPart {
        self.parts
            .get(&item.id)
            .cloned()
            .unwrap_or(PhysicsPart::Off)
    }
}

fn add_body(b: &mut Builder<'_>, item: &Item, setting: PhysicsSetting, transform: u32) -> u32 {
    let body = b.add(item.id.as_str(), BODY_ROLE, BODY_TYPE);
    let motion = if setting == PhysicsSetting::Body {
        MOVING
    } else {
        FIXED
    };
    b.set(body, "motion", SerializedParamValue::Enum { value: motion });
    b.wire(transform, "transform", body, "transform");
    b.physics.bodies.push(body);
    body
}

/// A frame at `body`'s solved pose, for what draws with the body.
fn pose_frame(b: &mut Builder<'_>, item: &Item, body: u32) -> u32 {
    let frame = b.add(item.id.as_str(), "pose_frame", TRANSFORM_TYPE);
    b.wire(body, "pose", frame, "parent");
    frame
}

/// An Object's physics, after its mesh (with a `source` output) and its
/// transform are emitted. Returns the frame the object draws in when it is
/// its own body: its transform is then the body's start, not its draw.
pub(crate) fn object(
    b: &mut Builder<'_>,
    item: &Item,
    frame: Option<&Frame>,
    mesh: u32,
    transform: u32,
) -> Option<u32> {
    match b.physics.part(item) {
        PhysicsPart::Solo(setting) => {
            if let Some(parent) = frame.and_then(|frame| frame.transform) {
                b.wire(parent, "transform", transform, "parent");
            }
            let body = add_body(b, item, setting, transform);
            b.wire(mesh, "source", body, "source");
            Some(pose_frame(b, item, body))
        }
        PhysicsPart::Member { root, local } => {
            // The draw ignores its transform's parent, so the parent here
            // only places the part inside the body.
            if let Some(local) = local.and_then(|local| b.id(local.as_str(), "body_local")) {
                b.wire(local, "transform", transform, "parent");
            }
            add_part(b, root, (mesh, transform));
            None
        }
        PhysicsPart::Off | PhysicsPart::Group(_) => None,
    }
}

/// A group drawn as one combined mesh (Combine Meshes on) is one part: of
/// its own body, or of the group body it sits in.
pub(crate) fn combined(b: &mut Builder<'_>, group: &Item, mesh: u32, transform: u32) {
    match b.physics.part(group) {
        PhysicsPart::Group(_) => add_part(b, group.id.clone(), (mesh, transform)),
        PhysicsPart::Member { root, .. } => {
            if let Some(local) = b.id(group.id.as_str(), "body_local") {
                b.wire(local, "transform", transform, "parent");
            }
            add_part(b, root, (mesh, transform));
        }
        PhysicsPart::Off | PhysicsPart::Solo(_) => {}
    }
}

fn add_part(b: &mut Builder<'_>, root: ItemId, part: (u32, u32)) {
    match b
        .physics
        .group_parts
        .iter_mut()
        .find(|(known, _)| *known == root)
    {
        Some((_, parts)) => parts.push(part),
        None => b.physics.group_parts.push((root, vec![part])),
    }
}

/// A Group's physics, after its transform and rows are emitted. Returns the
/// frame what is inside it sits in: a Body group's solved pose.
pub(crate) fn group(b: &mut Builder<'_>, item: &Item, inner: Frame) -> Frame {
    match b.physics.part(item) {
        PhysicsPart::Group(setting) => {
            // A group under copies is the Copies section's, so a group body
            // always has its own transform.
            let Some(transform) = inner.transform else {
                return inner;
            };
            let body = add_body(b, item, setting, transform);
            // The group's rows ran before its body existed.
            b.apply_rows(
                item,
                PHYSICS_ROWS
                    .iter()
                    .filter(|row| (row.applies)(&item.fields)),
            );
            if setting == PhysicsSetting::Body {
                return Frame {
                    transform: Some(pose_frame(b, item, body)),
                    ..inner
                };
            }
            inner
        }
        PhysicsPart::Member { local, .. } => {
            let body_local = b.add(item.id.as_str(), "body_local", TRANSFORM_TYPE);
            if let Some(transform) = inner.transform {
                b.mirror(transform, body_local);
            }
            if let Some(parent) = local.and_then(|local| b.id(local.as_str(), "body_local")) {
                b.wire(parent, "transform", body_local, "parent");
            }
            inner
        }
        PhysicsPart::Off | PhysicsPart::Solo(_) => inner,
    }
}

/// Join every body to the World's `world` and give each group body its parts.
pub(crate) fn join_world(b: &mut Builder<'_>, world: u32) {
    for (root, parts) in std::mem::take(&mut b.physics.group_parts) {
        let Some(body) = b.id(root.as_str(), BODY_ROLE) else {
            continue;
        };
        // The body grows a mesh and transform input per part.
        b.set(
            body,
            "parts",
            SerializedParamValue::Int {
                value: parts.len() as i32,
            },
        );
        for (k, (mesh, transform)) in parts.into_iter().enumerate() {
            b.wire(mesh, "source", body, &format!("mesh_{k}"));
            b.wire(transform, "transform", body, &format!("part_{k}"));
        }
    }
    let bodies = std::mem::take(&mut b.physics.bodies);
    // The world sizes its ports to its bodies. A body added mid-show grows
    // them and joins the running world; the bodies already moving keep going
    // (`RigidSimulation` follows each body by identity, not by port).
    b.set(
        world,
        "bodies",
        SerializedParamValue::Int {
            value: bodies.len().max(1) as i32,
        },
    );
    for (k, body) in bodies.into_iter().enumerate() {
        b.wire(body, "body", world, &format!("body_{k}"));
    }
}
