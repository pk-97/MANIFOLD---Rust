//! The scene-wide modifier types (SCENE_DOCUMENT_DESIGN.md ruling 21 and
//! section 7.19 (Modifiers)): Scene Loop, Math View and Scene Mirror. They
//! always act on the whole scene, so their card has no Targets row, and a
//! scene holds at most one Scene Loop and one Math View
//! (`ModifierType::once_per_scene`).
//!
//! Every row is a performance row reaching the type's stock recipe, the
//! row's property being the recipe's own param id, with two exceptions:
//! Scene Loop also drives every camera on the Loop rig, so its Beats, Stride,
//! Home, Near and Far reach those cameras' nodes and Pattern reaches both
//! the corridor and the cameras (camera travel per loop is a whole number of
//! patterns, SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D3); and Spacing is the
//! gap the scene's `node.loop_cell` adds to the measured length of what it
//! tiles, the one cell both the corridor and the cameras read.

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
pub const LOOP_CAMERA_ROWS: [(&str, &str); 4] = [
    ("bars", "phase"),
    ("patterns_per_loop", "rig"),
    ("home", "rig"),
    ("pattern_length", "rig"),
];

/// The Scene Loop row that sets the gap the scene's `node.loop_cell` adds
/// on top of what it measures: (row property, node param).
pub const LOOP_GAP_ROW: (&str, &str) = ("spacing", "gap");

pub static SCENE_LOOP_ROWS: [PropertyRow; 10] = [
    live("bars", 0).name("Beats").tip("Beats per full loop cycle"),
    live("pattern_length", 1).default(4.0).tip("Number of cells in each repeating pattern"),
    live("patterns_per_loop", 2).name("Stride").tip("Patterns travelled per loop cycle"),
    live("spacing", 3).name("Spacing").unit(Unit::Metres).tip("Extra gap between repeating cells along the corridor"),
    live("twist", 4).name("Twist").tip("Spiral rotation per pattern around the tunnel axis"),
    live("spiral_radius", 5).name("Tunnel Radius").unit(Unit::Metres).tip("Radial offset around the tunnel axis for helical/ring tunnels"),
    live("alternate", 6).name("Alternate Flip").toggle().tip("Flip every odd cell in the pattern by 180 degrees"),
    live("jitter_amount", 7).tip("Random rotation and scale variation across pattern cells"),
    live("home", 8).name("Home").unit(Unit::Metres).tip("Starting camera offset along the corridor"),
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
pub const NO_SCENE_LOOP: &str = "Loop rides the scene's Scene Loop: add one with the Modifiers +";

/// Why a pinned Scene Loop row that only a Loop camera reads does nothing yet.
pub const NO_LOOP_CAMERA: &str = "Add Loop Camera to use it";

/// The scene's Scene Loop card, on or off.
pub fn scene_loop(doc: &SceneDoc) -> Option<&Item> {
    doc.items.iter().find(|item| matches!(&item.fields, ItemFields::Modifier(card) if card.modifier == ModifierType::SceneLoop))
}

/// The Scene Loop rows only a Loop camera reads: the corridor recipe has no
/// Beats, Stride, Home, Near or Far of its own.
pub const LOOP_CAMERA_ONLY: [&str; 3] = [
    "bars", "patterns_per_loop", "home",
];

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
