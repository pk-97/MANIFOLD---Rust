//! Physics on Object and Group (SCENE_DOCUMENT_DESIGN.md section 7.8
//! (Physics and collision shapes)): the card rows of the body node an item
//! with physics compiles to, and which body every item belongs to.
//!
//! The card shows Mass (Body only), then Bounce and Friction. The shape comes
//! from the item's mesh, so no shape row exists; everything else on the body
//! node is automatic or internal.
//!
//! A group with Collider or Body is one body of everything inside it: a
//! Body group moves as one compound body, a Collider group is one static
//! solid. Inside a Body group every setting is absorbed, so a Body inside a
//! Body never compiles as two bodies. Inside a Collider group a Body item
//! stays its own body, and everything else is part of the collider. Items
//! with Copies on are the Copies section's (section 7.9 (Copies)).

use std::collections::HashMap;

use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::{Item, ItemFields, ItemId, PhysicsSetting, SceneDoc, Shape};

/// The role of the `node.rigid_body` an item with physics compiles to.
pub const BODY_ROLE: &str = "body";

const BODY: Reach = Reach::Own(BODY_ROLE);
const FROM_MESH: &str = "the shape comes from the item's mesh";
const COPIES: &str = "the Copies section sets copies";

fn any_physics(fields: &ItemFields) -> bool {
    fields.physics() != PhysicsSetting::None
}

fn body(fields: &ItemFields) -> bool {
    fields.physics() == PhysicsSetting::Body
}

pub static PHYSICS_ROWS: [PropertyRow; 26] = [
    row("mass", BODY, ControlGroup::Body, setup())
        .unit(Unit::Kilograms)
        .when(body),
    row("bounce", BODY, ControlGroup::Body, setup()).when(any_physics),
    row("friction", BODY, ControlGroup::Body, setup()).when(any_physics),
    row(
        "motion",
        BODY,
        ControlGroup::Body,
        Exposure::Automatic("the Physics setting: Collider is Fixed, Body is Moving"),
    ),
    row(
        "enabled",
        BODY,
        ControlGroup::Body,
        Exposure::Internal("an item with physics is always in the world"),
    ),
    row(
        "release_count",
        BODY,
        ControlGroup::Body,
        Exposure::Internal("breaking apart is not a scene setting"),
    ),
    row(
        "fragment_parent",
        BODY,
        ControlGroup::Body,
        Exposure::Internal("breaking apart is not a scene setting"),
    ),
    row(
        "path",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "mesh_index",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "primitive_index",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "material_index",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "fit",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "recenter",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "translate_x",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "translate_y",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "translate_z",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "fragment_count",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "fragment_index",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(FROM_MESH),
    ),
    row(
        "copy_count",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_size_spacing",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_columns",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_layout",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_turn_x",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_turn_y",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "copy_turn_z",
        BODY,
        ControlGroup::Body,
        Exposure::Internal(COPIES),
    ),
    row(
        "parts",
        BODY,
        ControlGroup::Body,
        Exposure::Automatic("the objects inside a group body"),
    ),
];

/// What one item is to physics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhysicsPart {
    /// Not in the world.
    Off,
    /// An Object that is its own body or collider.
    Solo(PhysicsSetting),
    /// A group that is one body of everything inside it.
    Group(PhysicsSetting),
    /// Part of the group body `root`; `local` is the nearest group between
    /// the item and the root, whose body-local transform it sits in.
    Member { root: ItemId, local: Option<ItemId> },
}

/// Which body every Object and Group in `doc` belongs to.
pub fn physics_parts(doc: &SceneDoc) -> HashMap<ItemId, PhysicsPart> {
    let mut parts = HashMap::new();
    classify(&doc.items, &Within::Free, None, &mut parts);
    parts
}

/// Why `item` cannot take Collider or Body, shown on its disabled Physics
/// control. None always stays available. `None` when both are available.
pub fn physics_unavailable(doc: &SceneDoc, item: &ItemId) -> Option<&'static str> {
    let found = doc.find(item)?;
    match &found.fields {
        ItemFields::Object(object) if object.shape == Shape::Ocean => {
            return Some("An Ocean surface cannot collide; use Water for water");
        }
        ItemFields::Object(object) if matches!(object.shape, Shape::Terrain { .. }) => {
            return Some("A Terrain is shaped on the GPU each frame and has no collision mesh");
        }
        ItemFields::Object(_) | ItemFields::Group(_) => {}
        _ => return Some("Only Objects and Groups have physics"),
    }
    let mut inside_copies = false;
    doc.walk(|other, ancestors| {
        if &other.id == item {
            inside_copies = ancestors.iter().any(|above| copies(&above.fields));
        }
    });
    if inside_copies {
        return Some("The group's Copies place this; set physics on the copied group");
    }
    let parts = physics_parts(doc);
    let inside_body = matches!(
        parts.get(item),
        Some(PhysicsPart::Member { root, .. }) if parts.get(root) == Some(&PhysicsPart::Group(PhysicsSetting::Body))
    );
    if inside_body {
        return Some("It moves as part of the Body group it is in");
    }
    if matches!(found.fields, ItemFields::Group(_)) {
        let count = solid_objects(&found.children, false);
        if count == 0 {
            return Some("Nothing in this group can collide");
        }
    }
    None
}

enum Within {
    Free,
    /// Copies place everything here (the Copies section's).
    Copies,
    Body(ItemId),
    Collider(ItemId),
}

fn copies(fields: &ItemFields) -> bool {
    match fields {
        ItemFields::Object(object) => object.copies,
        ItemFields::Group(group) => group.copies,
        _ => false,
    }
}

fn solid(shape: &Shape) -> bool {
    !matches!(shape, Shape::Ocean | Shape::Terrain { .. })
}

/// Objects in `items` a group body takes: not under copies, and for a
/// Collider group not the Body items that stay their own bodies.
fn solid_objects(items: &[Item], collider: bool) -> usize {
    items
        .iter()
        .map(|item| {
            if copies(&item.fields) || (collider && item.fields.physics() == PhysicsSetting::Body) {
                return 0;
            }
            match &item.fields {
                ItemFields::Object(object) => usize::from(solid(&object.shape)),
                ItemFields::Group(_) => solid_objects(&item.children, collider),
                _ => 0,
            }
        })
        .sum()
}

/// A group with `setting` that can be one body of everything inside it.
fn takes_group_body(item: &Item, setting: PhysicsSetting) -> bool {
    let count = solid_objects(&item.children, setting == PhysicsSetting::Collider);
    count > 0
}

fn classify(
    items: &[Item],
    within: &Within,
    local: Option<&ItemId>,
    parts: &mut HashMap<ItemId, PhysicsPart>,
) {
    for item in items {
        let setting = item.fields.physics();
        let member = |root: &ItemId| PhysicsPart::Member {
            root: root.clone(),
            local: local.cloned(),
        };
        match &item.fields {
            ItemFields::Group(_) => {
                let (part, inner, inner_local) = match within {
                    Within::Copies => (PhysicsPart::Off, Within::Copies, None),
                    _ if copies(&item.fields) => (PhysicsPart::Off, Within::Copies, None),
                    Within::Body(root) => {
                        (member(root), Within::Body(root.clone()), Some(&item.id))
                    }
                    Within::Collider(_)
                        if setting == PhysicsSetting::Body && takes_group_body(item, setting) =>
                    {
                        (
                            PhysicsPart::Group(setting),
                            Within::Body(item.id.clone()),
                            None,
                        )
                    }
                    Within::Collider(root) => {
                        (member(root), Within::Collider(root.clone()), Some(&item.id))
                    }
                    Within::Free
                        if setting != PhysicsSetting::None && takes_group_body(item, setting) =>
                    {
                        let inner = if setting == PhysicsSetting::Body {
                            Within::Body(item.id.clone())
                        } else {
                            Within::Collider(item.id.clone())
                        };
                        (PhysicsPart::Group(setting), inner, None)
                    }
                    Within::Free => (PhysicsPart::Off, Within::Free, None),
                };
                parts.insert(item.id.clone(), part);
                classify(&item.children, &inner, inner_local, parts);
            }
            ItemFields::Object(object) => {
                let part = match within {
                    _ if !solid(&object.shape) => PhysicsPart::Off,
                    Within::Copies => PhysicsPart::Off,
                    _ if object.copies => PhysicsPart::Off,
                    Within::Collider(_) if setting == PhysicsSetting::Body => {
                        PhysicsPart::Solo(setting)
                    }
                    Within::Body(root) | Within::Collider(root) => member(root),
                    Within::Free if setting != PhysicsSetting::None => PhysicsPart::Solo(setting),
                    Within::Free => PhysicsPart::Off,
                };
                parts.insert(item.id.clone(), part);
            }
            _ => {}
        }
    }
}
