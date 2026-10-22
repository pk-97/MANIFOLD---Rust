//! The deformer Modifier types (SCENE_DOCUMENT_DESIGN.md section 7.19
//! (Modifiers), lane L18): Bend, Twist, Taper, Inflate, Rotate, Displace by
//! Texture, Morph, Boil, Ripple, Melt, Glitch Jitter, Shatter Mesh, Slice,
//! Voxelize, Fold, Facet Normals, Wave Shear, Cut Out Box and Shake. Each is
//! one entry here wiring one stock recipe in `scene-modifier-presets`; its
//! rows are the recipe's own param ids, every one a performance row.
//!
//! Lengths on a card are in the target's radius units (the recipe multiplies
//! them by the frame's radius), so a deformer reads a small prop and a whole
//! scene the same way. Normal Wave is Surface Waves (`node.normal_wave_mesh`)
//! and is not repeated here.

use manifold_foundation::scene_surface::Unit;

use super::modifier::{ModifierTypeEntry, ON_IS_THE_TOGGLE, entry, held, join, live, mask, mask_centre, mask_radius, mask_rows};
use super::PropertyRow;
use crate::scene_doc::ModifierType;

/// A performance row measured in the target's radius.
const fn radii(property: &'static str, order: u8) -> PropertyRow {
    live(property, order).unit(Unit::Radius)
}

pub static BEND_ROWS: [PropertyRow; 14] =
    join([live("angle", 1), live("axis", 2), radii("center", 3), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static TWIST_ROWS: [PropertyRow; 14] =
    join([live("angle", 1), live("axis", 2), radii("center", 3), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static TAPER_ROWS: [PropertyRow; 15] = join(
    [live("taper", 1), live("axis", 2), radii("center", 3), radii("length", 4), held("enabled", ON_IS_THE_TOGGLE)],
    mask_rows!(20),
);

pub static INFLATE_ROWS: [PropertyRow; 12] = join([radii("amount", 1), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static ROTATE_ROWS: [PropertyRow; 14] = join(
    [live("angle_x", 1), live("angle_y", 2), live("angle_z", 3), held("enabled", ON_IS_THE_TOGGLE)],
    mask_rows!(20),
);

pub static DISPLACE_TEXTURE_ROWS: [PropertyRow; 16] = join(
    [
        radii("amount", 1),
        live("scale", 2),
        live("offset_x", 3),
        live("offset_y", 4),
        live("bias", 5),
        held("enabled", ON_IS_THE_TOGGLE),
    ],
    mask_rows!(20),
);

pub static MORPH_ROWS: [PropertyRow; 12] = join([live("t", 1), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static BOIL_ROWS: [PropertyRow; 14] =
    join([radii("amount", 1), live("frequency", 2), live("speed", 3), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static RIPPLE_ROWS: [PropertyRow; 15] = join(
    [radii("amplitude", 1), live("frequency", 2), live("speed", 3), live("axis", 4), held("enabled", ON_IS_THE_TOGGLE)],
    mask_rows!(20),
);

pub static MELT_ROWS: [PropertyRow; 14] =
    join([radii("amount", 1), live("frequency", 2), live("seed", 3), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static GLITCH_JITTER_ROWS: [PropertyRow; 14] =
    join([radii("amount", 1), live("rate", 2), live("seed", 3), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static SHATTER_MESH_ROWS: [PropertyRow; 13] =
    join([radii("amount", 1), live("seed", 2), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static SLICE_ROWS: [PropertyRow; 13] =
    join([live("axis", 1), radii("cut", 2), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static VOXELIZE_ROWS: [PropertyRow; 13] =
    join([live("amount", 1), radii("cell_size", 2), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static FOLD_ROWS: [PropertyRow; 13] = join([live("axis", 1), live("amount", 2), held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static FACET_NORMALS_ROWS: [PropertyRow; 11] = join([held("enabled", ON_IS_THE_TOGGLE)], mask_rows!(20));

pub static WAVE_SHEAR_ROWS: [PropertyRow; 17] = join(
    [
        live("amplitude", 1),
        live("frequency", 2),
        live("phase", 3),
        live("yaw", 4),
        live("pitch", 5),
        live("axis", 6),
        held("enabled", ON_IS_THE_TOGGLE),
    ],
    mask_rows!(20),
);

/// Cut Out Box is its own region: it carries no mask.
pub static CUT_OUT_BOX_ROWS: [PropertyRow; 8] = [
    radii("center_x", 1),
    radii("center_y", 2),
    radii("center_z", 3),
    radii("size_x", 4),
    radii("size_y", 5),
    radii("size_z", 6),
    radii("feather", 7),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// Shake moves the target's transform, not its vertices, so it takes no mask.
pub static SHAKE_ROWS: [PropertyRow; 5] = [
    live("amount", 1),
    live("frequency", 2),
    live("position_strength", 3),
    live("rotation_strength", 4),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// The deformer types, in menu order.
pub static DEFORMER_TYPES: [ModifierTypeEntry; 19] = [
    entry(ModifierType::Bend, "bend", "Bend", "Bend", true, &BEND_ROWS),
    entry(ModifierType::Twist, "twist", "Twist", "Twist", true, &TWIST_ROWS),
    entry(ModifierType::Taper, "taper", "Taper", "Taper", true, &TAPER_ROWS),
    entry(ModifierType::Inflate, "inflate", "Inflate", "Inflate", true, &INFLATE_ROWS),
    entry(ModifierType::Rotate, "rotate", "Rotate", "Rotate", false, &ROTATE_ROWS),
    entry(
        ModifierType::DisplaceByTexture,
        "displace_by_texture",
        "Displace by Texture",
        "DisplaceByTexture",
        true,
        &DISPLACE_TEXTURE_ROWS,
    ),
    entry(ModifierType::Morph, "morph", "Morph", "Morph", false, &MORPH_ROWS),
    entry(ModifierType::Boil, "boil", "Boil", "Boil", true, &BOIL_ROWS),
    entry(ModifierType::Ripple, "ripple", "Ripple", "Ripple", true, &RIPPLE_ROWS),
    entry(ModifierType::Melt, "melt", "Melt", "Melt", true, &MELT_ROWS),
    entry(ModifierType::GlitchJitter, "glitch_jitter", "Glitch Jitter", "GlitchJitter", true, &GLITCH_JITTER_ROWS),
    entry(ModifierType::ShatterMesh, "shatter_mesh", "Shatter Mesh", "ShatterMesh", true, &SHATTER_MESH_ROWS),
    entry(ModifierType::Slice, "slice", "Slice", "Slice", true, &SLICE_ROWS),
    entry(ModifierType::Voxelize, "voxelize", "Voxelize", "Voxelize", true, &VOXELIZE_ROWS),
    entry(ModifierType::Fold, "fold", "Fold", "Fold", true, &FOLD_ROWS),
    entry(ModifierType::FacetNormals, "facet_normals", "Facet Normals", "FacetNormals", false, &FACET_NORMALS_ROWS),
    entry(ModifierType::WaveShear, "wave_shear", "Wave Shear", "WaveShear", true, &WAVE_SHEAR_ROWS),
    entry(ModifierType::CutOutBox, "cut_out_box", "Cut Out Box", "CutOutBox", true, &CUT_OUT_BOX_ROWS),
    entry(ModifierType::Shake, "shake", "Shake", "Shake", false, &SHAKE_ROWS),
];
