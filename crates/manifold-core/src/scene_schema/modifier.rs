//! Mesh modifiers: the rows of each modifier an Object can list
//! (`ObjectFields::modifiers`). A modifier compiles to one scene modifier
//! instance, so a row reaches a param of that instance's recipe: the row's
//! property is the recipe's own param id. On the Object these appear as
//! `<object id>.<modifier key>.<param>` ([`modifier_param_id`]), so reordering
//! the list never moves a mapping.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::{PropertyRow, Reach, row, setup};
use crate::scene_doc::{ItemId, MeshModifierKind};

const M: ControlGroup = ControlGroup::Modifier;

const fn m(property: &'static str) -> PropertyRow {
    row(property, Reach::Modifier, M, setup())
}

const fn centre(property: &'static str) -> PropertyRow {
    m(property).unit(Unit::Metres).limit(HardLimit::Unlimited)
}

pub static SURFACE_PEEL_ROWS: [PropertyRow; 21] = [
    m("enabled"),
    m("lift"),
    m("curl"),
    m("spread"),
    m("phase"),
    m("detail"),
    m("yaw"),
    m("pitch"),
    m("clip_trigger"),
    m("burst_strength"),
    m("burst_duration"),
    m("mask_amount"),
    m("mask_shape"),
    m("mask_width"),
    m("mask_feather"),
    m("mask_invert"),
    centre("mask_center_x"),
    centre("mask_center_y"),
    centre("mask_center_z"),
    m("mask_yaw"),
    m("mask_pitch"),
];

pub static ORDERED_RECON_ROWS: [PropertyRow; 26] = [
    m("enabled"),
    m("progress"),
    m("bands"),
    m("separation"),
    m("rotation"),
    m("spread"),
    m("direction_x"),
    m("direction_y"),
    m("direction_z"),
    m("clip_trigger"),
    m("attack_beats").unit(Unit::Beats),
    m("hold_beats").unit(Unit::Beats),
    m("return_beats").unit(Unit::Beats),
    m("stagger_beats").unit(Unit::Beats),
    m("stagger_yaw"),
    m("stagger_pitch"),
    m("mask_amount"),
    m("mask_shape"),
    m("mask_width"),
    m("mask_feather"),
    m("mask_invert"),
    centre("mask_center_x"),
    centre("mask_center_y"),
    centre("mask_center_z"),
    m("mask_yaw"),
    m("mask_pitch"),
];

pub static SURFACE_WAVES_ROWS: [PropertyRow; 16] = [
    m("enabled"),
    m("amplitude"),
    m("frequency"),
    m("phase"),
    m("yaw"),
    m("pitch"),
    m("mask_amount"),
    m("mask_shape"),
    m("mask_width"),
    m("mask_feather"),
    m("mask_invert"),
    centre("mask_center_x"),
    centre("mask_center_y"),
    centre("mask_center_z"),
    m("mask_yaw"),
    m("mask_pitch"),
];

pub static SPATIAL_ECHOES_ROWS: [PropertyRow; 7] =
    [m("enabled"), m("count"), m("radius"), m("rise"), m("phase"), m("arc"), m("taper")];

/// The rows of a modifier kind.
pub fn modifier_rows(kind: MeshModifierKind) -> &'static [PropertyRow] {
    match kind {
        MeshModifierKind::SurfacePeel => &SURFACE_PEEL_ROWS,
        MeshModifierKind::OrderedRecon => &ORDERED_RECON_ROWS,
        MeshModifierKind::SurfaceWaves => &SURFACE_WAVES_ROWS,
        MeshModifierKind::SpatialEchoes => &SPATIAL_ECHOES_ROWS,
    }
}

/// The property of one modifier's param on its Object: `<key>.<param>`.
pub fn modifier_property(key: &str, param: &str) -> String {
    format!("{key}.{param}")
}

/// The manifest id of one modifier's param: `<object id>.<key>.<param>`.
pub fn modifier_param_id(object: &ItemId, key: &str, param: &str) -> String {
    crate::scene_doc::param_id(object, &modifier_property(key, param))
}

/// Why an Object cannot take a mesh modifier, for the disabled Add control's
/// hover text. The stock recipes measure the mesh they act on, which only a
/// mesh from a file can give them.
pub fn modifiers_unavailable(shape: &crate::scene_doc::Shape) -> Option<&'static str> {
    match shape {
        crate::scene_doc::Shape::Mesh { .. } => None,
        _ => Some("Mesh modifiers measure an imported mesh; a built-in solid has none"),
    }
}
