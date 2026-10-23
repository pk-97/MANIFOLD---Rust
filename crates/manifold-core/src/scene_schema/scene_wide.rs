//! The scene-wide modifier types (SCENE_DOCUMENT_DESIGN.md ruling 21 and
//! section 7.19 (Modifiers)): Scene Loop, Math View and Scene Mirror. They
//! always act on the whole scene, so their card has no Targets row, and a
//! scene holds at most one Scene Loop and one Math View
//! (`ModifierType::once_per_scene`).
//!
//! Every row is a performance row reaching the type's stock recipe, the
//! row's property being the recipe's own param id, with one exception:
//! Scene Loop also drives every camera on the Loop rig, so its Beats, Stride,
//! Home, Near and Far reach those cameras' nodes, and Pattern and Spacing
//! reach both the corridor and the cameras (camera travel per loop is a whole
//! number of patterns, SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D3).

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{CameraRig, Item, ItemFields, ModifierType, SceneDoc};

const M: ControlGroup = ControlGroup::Modifier;

const fn live(property: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Modifier, M, perform(CardBand::Modifiers, order))
}

const ON_IS_THE_TOGGLE: &str = "The card's On toggle turns the modifier on and off";

/// The Scene Loop rows that reach the cameras on the Loop rig, by the node
/// role on the camera and the node param.
pub const LOOP_CAMERA_ROWS: [(&str, &str); 7] = [
    ("bars", "phase"),
    ("patterns_per_loop", "rig"),
    ("home", "rig"),
    ("near", "rig"),
    ("far", "rig"),
    ("pattern_length", "rig"),
    ("cell_size", "rig"),
];

pub static SCENE_LOOP_ROWS: [PropertyRow; 9] = [
    live("bars", 0).name("Beats"),
    live("pattern_length", 1),
    live("patterns_per_loop", 2).name("Stride"),
    live("cell_size", 3).unit(Unit::Metres),
    live("jitter_amount", 4),
    live("home", 5).name("Home").unit(Unit::Metres),
    live("near", 6).name("Near").unit(Unit::Metres),
    live("far", 7).name("Far").unit(Unit::Metres),
    row("enabled", Reach::Modifier, M, Exposure::Internal(ON_IS_THE_TOGGLE)),
];

pub static MATH_VIEW_ROWS: [PropertyRow; 18] = [
    live("math_view_mode", 0),
    live("math_view_occlusion", 1),
    live("math_view_grid", 2),
    live("math_view_fragments", 3),
    live("math_view_ghosts", 4),
    live("math_view_vectors", 5),
    live("math_view_axes", 6),
    live("math_view_trails", 7),
    live("math_view_grid_brightness", 8),
    live("math_view_fragments_brightness", 9),
    live("math_view_ghosts_brightness", 10),
    live("math_view_vectors_brightness", 11),
    live("math_view_trails_brightness", 12),
    live("math_view_density", 13),
    live("math_view_line_width", 14),
    live("math_view_geometry_hue", 15),
    live("math_view_path_hue", 16),
    row("enabled", Reach::Modifier, M, Exposure::Internal(ON_IS_THE_TOGGLE)),
];

pub static SCENE_MIRROR_ROWS: [PropertyRow; 3] = [
    live("axis", 0),
    live("plane_offset", 1).unit(Unit::Metres),
    row("enabled", Reach::Modifier, M, Exposure::Internal(ON_IS_THE_TOGGLE)),
];

/// Why the Loop rig cannot be picked: it rides the scene's Scene Loop.
pub const NO_SCENE_LOOP: &str = "Loop rides the scene's Scene Loop: add one from Add, Modifier";

/// The scene's Scene Loop card, on or off.
pub fn scene_loop(doc: &SceneDoc) -> Option<&Item> {
    doc.items.iter().find(|item| matches!(&item.fields, ItemFields::Modifier(card) if card.modifier == ModifierType::SceneLoop))
}

/// The Scene Loop rows only a Loop camera reads: the corridor recipe has no
/// Beats, Stride, Home, Near or Far of its own.
pub const LOOP_CAMERA_ONLY: [&str; 5] = ["bars", "patterns_per_loop", "home", "near", "far"];

/// Whether some camera in `doc` rides the Loop rig.
pub fn has_loop_camera(doc: &SceneDoc) -> bool {
    let mut found = false;
    doc.walk(|item, _| found |= matches!(&item.fields, ItemFields::Camera(camera) if camera.rig == CameraRig::Loop));
    found
}

/// Whether `item`'s row `property` stays hidden: a Scene Loop row only a
/// Loop camera reads, while no camera rides the loop.
pub fn waits_for_loop_camera(doc: &SceneDoc, item: &Item, property: &str) -> bool {
    matches!(&item.fields, ItemFields::Modifier(card) if card.modifier == ModifierType::SceneLoop)
        && LOOP_CAMERA_ONLY.contains(&property)
        && !has_loop_camera(doc)
}

/// Why the Add menu cannot add an item with `fields` to `doc` right now:
/// a second card of a type the scene holds one of.
pub fn add_refused(doc: &SceneDoc, fields: &ItemFields) -> Option<&'static str> {
    match fields {
        ItemFields::Modifier(card) => doc.second_modifier(card.modifier, None),
        _ => None,
    }
}

/// Why a modifier card cannot swap to the type `key`: the scene already
/// holds that type's one card.
pub(super) fn type_unavailable(doc: &SceneDoc, item: &Item, field: &str, key: &str) -> Option<&'static str> {
    if field != "modifier_type" {
        return None;
    }
    let entry = super::MODIFIER_TYPES.iter().find(|entry| entry.key == key)?;
    doc.second_modifier(entry.modifier, Some(&item.id))
}
