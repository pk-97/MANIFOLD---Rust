//! Copies: the section on an Object or Group with Copies on. Every row
//! reaches the copies body, the `node.rigid_body` that lays the copies out
//! and publishes their instances (SCENE_DOCUMENT_DESIGN.md section 7.9
//! (Copies)). Count, Layout, Spacing, Columns and Turn are live params: the
//! body reads them every frame.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ItemFields, Shape};

/// The role of the copies body inside its item.
pub const COPIES_ROLE: &str = "copies";
const BODY: Reach = Reach::Own(COPIES_ROLE);
const C: ControlGroup = ControlGroup::Copies;

/// A new Copies section starts with this many copies.
pub const START_COUNT: f32 = 4.0;

fn on(fields: &ItemFields) -> bool {
    fields.copies()
}

const FILE_REASON: &str = "copies draw a built-in solid, not a file";

const fn card(property: &'static str, param: &'static str, exposure: Exposure) -> PropertyRow {
    row(property, BODY, C, exposure).param(param).when(on)
}

const fn auto(property: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    card(property, param, Exposure::Automatic(why))
}

const fn internal(property: &'static str, param: &'static str, why: &'static str) -> PropertyRow {
    card(property, param, Exposure::Internal(why))
}

pub static COPIES_ROWS: [PropertyRow; 28] = [
    card("count", "copy_count", perform(CardBand::Scene, 0)).default(START_COUNT),
    card("layout", "copy_layout", perform(CardBand::Scene, 1)),
    card("spacing", "copy_size_spacing", perform(CardBand::Scene, 2)).unit(Unit::SizeMultiple).limit(AT_LEAST_ZERO),
    card("columns", "copy_columns", setup()),
    card("turn_x", "copy_turn_x", setup()).limit(HardLimit::Unlimited),
    card("turn_y", "copy_turn_y", setup()).limit(HardLimit::Unlimited),
    card("turn_z", "copy_turn_z", setup()).limit(HardLimit::Unlimited),
    auto("copies_enabled", "enabled", "the physics setting: off while it is None, so the copies rest in their layout"),
    auto("copies_motion", "motion", "the physics setting: Fixed for a Collider, Moving for a Body"),
    auto("copies_shape", "shape", "the item's Shape"),
    auto("copies_mass", "mass", "the Physics section's Mass"),
    auto("copies_friction", "friction", "the Physics section's Friction"),
    auto("copies_bounce", "bounce", "the Physics section's Bounce"),
    internal("copies_release_count", "release_count", "Break Apart is not a Copies control"),
    internal("copies_path", "path", FILE_REASON),
    internal("copies_mesh_index", "mesh_index", FILE_REASON),
    internal("copies_primitive_index", "primitive_index", FILE_REASON),
    internal("copies_material_index", "material_index", FILE_REASON),
    internal("copies_fit", "fit", FILE_REASON),
    internal("copies_recenter", "recenter", FILE_REASON),
    internal("copies_translate_x", "translate_x", FILE_REASON),
    internal("copies_translate_y", "translate_y", FILE_REASON),
    internal("copies_translate_z", "translate_z", FILE_REASON),
    internal("copies_fragment_count", "fragment_count", "copies are whole solids"),
    internal("copies_fragment_index", "fragment_index", "copies are whole solids"),
    internal("copies_fragment_parent", "fragment_parent", "copies are whole solids"),
    internal("copies_collider_parts", "collider_parts", "the collision shape is automatic"),
    internal("copies_compound_materials", "compound_materials", FILE_REASON),
];

/// Whether an Object of this shape can be copied: the copies body lays out
/// built-in solids.
pub fn can_copy(shape: &Shape) -> bool {
    !matches!(shape, Shape::Plane | Shape::Ocean | Shape::Mesh { .. })
}

/// Why an item's Copies switch is unavailable, for the disabled control's
/// hover text. A Group can always copy.
pub fn copies_unavailable(fields: &ItemFields) -> Option<&'static str> {
    match fields {
        ItemFields::Object(object) if !can_copy(&object.shape) => {
            Some("Copies draw a built-in solid; this shape cannot be copied yet")
        }
        _ => None,
    }
}
