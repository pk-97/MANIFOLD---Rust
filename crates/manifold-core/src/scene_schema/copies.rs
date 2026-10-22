//! Copies: the section on an Object or Group with Copies on. Every row
//! reaches the copies body, the `node.rigid_body` that lays the copies out
//! and publishes their instances (SCENE_DOCUMENT_DESIGN.md section 7.9
//! (Copies)). Count, Layout, Spacing, Columns and Turn are live params: the
//! body reads them every frame.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use super::MAX_GROUP_PARTS;
use crate::scene_doc::{Item, ItemFields, Shape};

/// The role of the copies body inside its item.
pub const COPIES_ROLE: &str = "copies";
const BODY: Reach = Reach::Own(COPIES_ROLE);
const C: ControlGroup = ControlGroup::Copies;

/// A new Copies section starts with this many copies.
pub const START_COUNT: f32 = 4.0;

fn on(fields: &ItemFields) -> bool {
    fields.copies()
}

const FROM_MESH: &str = "the shape comes from the item's mesh";

const fn card(property: &'static str, param: &'static str, exposure: Exposure) -> PropertyRow {
    row(property, BODY, C, exposure).param(param).when(on)
}

const fn auto(property: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    card(property, param, Exposure::Automatic(why))
}

const fn internal(property: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    card(property, param, Exposure::Internal(why))
}

pub static COPIES_ROWS: [PropertyRow; 25] = [
    card("count", "copy_count", perform(CardBand::Scene, 0)).default(START_COUNT),
    card("layout", "copy_layout", perform(CardBand::Scene, 1)),
    card("spacing", "copy_size_spacing", perform(CardBand::Scene, 2)).unit(Unit::SizeMultiple).limit(AT_LEAST_ZERO),
    card("columns", "copy_columns", setup()),
    card("turn_x", "copy_turn_x", setup()).limit(HardLimit::Unlimited),
    card("turn_y", "copy_turn_y", setup()).limit(HardLimit::Unlimited),
    card("turn_z", "copy_turn_z", setup()).limit(HardLimit::Unlimited),
    auto("copies_enabled", "enabled", "the physics setting: off while it is None, so the copies rest in their layout"),
    auto("copies_motion", "motion", "the physics setting: Fixed for a Collider, Moving for a Body"),
    auto("copies_mass", "mass", "the Physics section's Mass"),
    auto("copies_friction", "friction", "the Physics section's Friction"),
    auto("copies_bounce", "bounce", "the Physics section's Bounce"),
    internal("copies_release_count", "release_count", "Break Apart is not a Copies control"),
    internal("copies_path", "path", FROM_MESH),
    internal("copies_mesh_index", "mesh_index", FROM_MESH),
    internal("copies_primitive_index", "primitive_index", FROM_MESH),
    internal("copies_material_index", "material_index", FROM_MESH),
    internal("copies_fit", "fit", FROM_MESH),
    internal("copies_recenter", "recenter", FROM_MESH),
    internal("copies_translate_x", "translate_x", FROM_MESH),
    internal("copies_translate_y", "translate_y", FROM_MESH),
    internal("copies_translate_z", "translate_z", FROM_MESH),
    internal("copies_fragment_count", "fragment_count", "copies are whole solids"),
    internal("copies_fragment_index", "fragment_index", "copies are whole solids"),
    internal("copies_fragment_parent", "fragment_parent", "copies are whole solids"),
];

/// Whether an Object of this shape can be copied: every built-in solid and
/// imported part can; an Ocean surface and a curve shape cannot.
pub fn can_copy(shape: &Shape) -> bool {
    *shape != Shape::Ocean && shape.curve().is_none()
}

/// Objects below `items` that are one part of a copied group's shape.
fn copied_parts(items: &[Item]) -> usize {
    items
        .iter()
        .map(|item| match &item.fields {
            ItemFields::Object(object) => usize::from(can_copy(&object.shape)),
            _ => copied_parts(&item.children),
        })
        .sum()
}

/// Why `item` cannot take Copies, for the disabled switch's hover text.
pub fn copies_unavailable(item: &Item) -> Option<&'static str> {
    match &item.fields {
        ItemFields::Object(object) if object.shape.curve().is_some() => Some(super::physics::CURVE_HAS_NO_SOLID),
        ItemFields::Object(object) if !can_copy(&object.shape) => Some("An Ocean surface cannot be copied"),
        ItemFields::Group(_) => match copied_parts(&item.children) {
            0 => Some("Nothing in this group has a shape to copy"),
            count if count > MAX_GROUP_PARTS => Some("Copies of a group take at most 64 objects"),
            _ => None,
        },
        _ => None,
    }
}

const COPY_GROUPS_FULL: &str = "The physics world is full: it takes 32 copy groups";
const _: () = assert!(crate::scene_impulse::RigidImpulseTargets::GROUP_CAPACITY == 32, "the full reason names the capacity");

/// Why no more items can take Copies: the world holds as many copy groups as
/// it takes.
pub fn copy_groups_full(doc: &crate::scene_doc::SceneDoc) -> Option<&'static str> {
    let mut groups = 0;
    doc.walk(|item, ancestors| {
        if item.fields.copies() && !ancestors.iter().any(|above| above.fields.copies()) {
            groups += 1;
        }
    });
    (groups >= crate::scene_impulse::RigidImpulseTargets::GROUP_CAPACITY).then_some(COPY_GROUPS_FULL)
}
