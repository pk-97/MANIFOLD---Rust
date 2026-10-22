//! Modifier: one card of the scene's modifier chain (SCENE_DOCUMENT_DESIGN.md
//! section 7.19 (Modifiers)). Each type is one entry of [`MODIFIER_TYPES`]:
//! its key, label, the stock recipe it compiles (`scene-modifier-presets`)
//! and its rows. A card compiles to scene modifier instances of that recipe,
//! so a row reaches a param of the recipe: the row's property is the
//! recipe's own param id, and the manifest param is `<modifier id>.<param>`,
//! so reordering the chain never moves a mapping.
//!
//! Every card row is a performance row (section 7.4 (Stage versus setup)).
//! A recipe's Enabled is the card's On field; its clip-trigger and hit
//! rows stay internal (ruling 22).

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, is_on, on_off, parse_on, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{
    DEFAULT_PIECES, Item, ItemFields, ModifierFields, ModifierTargets, ModifierType, PhysicsSetting, SceneDoc,
};

const M: ControlGroup = ControlGroup::Modifier;
const MASK: ControlGroup = ControlGroup::Mask;
const BAND: CardBand = CardBand::Modifiers;

/// A performance row of a modifier, at `order` on its card.
const fn live(property: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Modifier, M, perform(BAND, order))
}

/// A recipe param the card never shows.
const fn held(property: &'static str, why: &'static str) -> PropertyRow {
    row(property, Reach::Modifier, M, Exposure::Internal(why))
}

const ON_IS_THE_TOGGLE: &str = "The card's On toggle turns the modifier on and off";
const NO_HITS: &str = "Modifiers have no clip trigger or hit rows (ruling 22)";

/// A mask row, in the Mask section.
const fn mask(property: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Modifier, MASK, perform(BAND, order))
}

/// A mask row measured in the target's radius: 1 is the target's radius.
const fn mask_radius(property: &'static str, name: &'static str, order: u8) -> PropertyRow {
    mask(property, order).name(name).unit(Unit::Radius)
}

/// A mask centre: where the region sits, from the target's centre.
const fn mask_centre(property: &'static str, name: &'static str, order: u8) -> PropertyRow {
    mask_radius(property, name, order).range(-2.0, 2.0).limit(HardLimit::Unlimited)
}

/// The mask rows main carried on every masked modifier
/// (`node.mesh_spatial_mask`), from `order` on.
macro_rules! mask_rows {
    ($o:expr) => {
        [
            mask("mask_amount", $o),
            mask("mask_shape", $o + 1),
            mask_radius("mask_width", "Mask Width", $o + 2),
            mask_radius("mask_feather", "Mask Feather", $o + 3),
            mask("mask_invert", $o + 4),
            mask_centre("mask_center_x", "Mask Centre X", $o + 5),
            mask_centre("mask_center_y", "Mask Centre Y", $o + 6),
            mask_centre("mask_center_z", "Mask Centre Z", $o + 7),
            mask("mask_yaw", $o + 8),
            mask("mask_pitch", $o + 9),
        ]
    };
}

const fn join<const A: usize, const B: usize, const N: usize>(a: [PropertyRow; A], b: [PropertyRow; B]) -> [PropertyRow; N] {
    assert!(A + B == N);
    let mut out = [a[0]; N];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    while i < N {
        out[i] = b[i - A];
        i += 1;
    }
    out
}

pub static SURFACE_PEEL_ROWS: [PropertyRow; 21] = join(
    [
        live("lift", 1),
        live("curl", 2),
        live("spread", 3),
        live("phase", 4),
        live("detail", 5),
        live("yaw", 6),
        live("pitch", 7),
        held("enabled", ON_IS_THE_TOGGLE),
        held("clip_trigger", NO_HITS),
        held("burst_strength", NO_HITS),
        held("burst_duration", NO_HITS),
    ],
    mask_rows!(20),
);

pub static ORDERED_RECON_ROWS: [PropertyRow; 26] = join(
    [
        live("progress", 1),
        live("bands", 2),
        live("separation", 3),
        live("rotation", 4),
        live("spread", 5),
        live("direction_x", 6),
        live("direction_y", 7),
        live("direction_z", 8),
        held("enabled", ON_IS_THE_TOGGLE),
        held("clip_trigger", NO_HITS),
        held("attack_beats", NO_HITS),
        held("hold_beats", NO_HITS),
        held("return_beats", NO_HITS),
        // Stagger spreads the hit envelope across the bands; with no hit it
        // does nothing.
        held("stagger_beats", NO_HITS),
        held("stagger_yaw", NO_HITS),
        held("stagger_pitch", NO_HITS),
    ],
    mask_rows!(20),
);

pub static SURFACE_WAVES_ROWS: [PropertyRow; 16] = join(
    [
        live("amplitude", 1),
        live("frequency", 2),
        live("phase", 3),
        live("yaw", 4),
        live("pitch", 5),
        held("enabled", ON_IS_THE_TOGGLE),
    ],
    mask_rows!(20),
);

pub static SPATIAL_ECHOES_ROWS: [PropertyRow; 7] = [
    live("count", 1),
    live("radius", 2),
    live("rise", 3),
    live("phase", 4),
    live("arc", 5),
    live("taper", 6),
    held("enabled", ON_IS_THE_TOGGLE),
];

pub static ELASTIC_SCULPTURE_ROWS: [PropertyRow; 17] = join(
    [
        live("bend", 1),
        live("cross_bend", 2),
        live("detail", 3),
        live("phase", 4),
        live("yaw", 5),
        live("pitch", 6),
        held("enabled", ON_IS_THE_TOGGLE),
    ],
    mask_rows!(20),
);

pub static VORTEX_FRAGMENTS_ROWS: [PropertyRow; 18] = join(
    [
        live("orbit", 1),
        live("rise", 2),
        live("separation", 3),
        live("phase", 4),
        live("detail", 5),
        live("yaw", 6),
        live("pitch", 7),
        held("enabled", ON_IS_THE_TOGGLE),
    ],
    mask_rows!(20),
);

/// Shatter's Pieces and Enabled are the recipe's preparation params: the
/// card's Pieces and On fields set them.
pub static SHATTER_ROWS: [PropertyRow; 2] = [live("shatter", 1).name("Break"), held("clip_trigger", NO_HITS)];

/// One modifier type: everything the card, the Add menu and the compiler
/// know about it. A later lane adds a type with one entry here and one
/// `ModifierType` variant.
#[derive(Debug, Clone, Copy)]
pub struct ModifierTypeEntry {
    pub modifier: ModifierType,
    /// What a Type pick sends.
    pub key: &'static str,
    pub label: &'static str,
    /// The stock scene modifier recipe it compiles.
    pub recipe: &'static str,
    /// It moves vertices, so a built-in shape it targets gets mesh detail.
    pub deforms: bool,
    pub rows: &'static [PropertyRow],
}

const fn entry(
    modifier: ModifierType,
    key: &'static str,
    label: &'static str,
    recipe: &'static str,
    deforms: bool,
    rows: &'static [PropertyRow],
) -> ModifierTypeEntry {
    ModifierTypeEntry { modifier, key, label, recipe, deforms, rows }
}

/// Every modifier type, in menu order.
pub static MODIFIER_TYPES: [ModifierTypeEntry; 7] = [
    entry(ModifierType::SurfacePeel, "surface_peel", "Surface Peel", "SurfacePeel", true, &SURFACE_PEEL_ROWS),
    entry(ModifierType::OrderedRecon, "ordered_recon", "Ordered Recon", "OrderedRecon", true, &ORDERED_RECON_ROWS),
    entry(ModifierType::SurfaceWaves, "surface_waves", "Surface Waves", "SurfaceWaves", true, &SURFACE_WAVES_ROWS),
    entry(ModifierType::SpatialEchoes, "spatial_echoes", "Spatial Echoes", "SpatialEchoes", false, &SPATIAL_ECHOES_ROWS),
    entry(
        ModifierType::ElasticSculpture,
        "elastic_sculpture",
        "Elastic Sculpture",
        "ElasticSculpture",
        true,
        &ELASTIC_SCULPTURE_ROWS,
    ),
    entry(
        ModifierType::VortexFragments,
        "vortex_fragments",
        "Vortex Fragments",
        "VortexFragments",
        true,
        &VORTEX_FRAGMENTS_ROWS,
    ),
    entry(ModifierType::Shatter, "shatter", "Shatter", "Shatter", false, &SHATTER_ROWS),
];

/// The entry of a modifier type.
pub fn modifier_type(modifier: ModifierType) -> &'static ModifierTypeEntry {
    MODIFIER_TYPES.iter().find(|entry| entry.modifier == modifier).expect("every modifier type has an entry")
}

/// The rows of the modifier type an item with `fields` has; none for any
/// other kind.
pub fn modifier_rows(fields: &ItemFields) -> &'static [PropertyRow] {
    match fields {
        ItemFields::Modifier(m) => modifier_type(m.modifier).rows,
        _ => &[],
    }
}

/// Why Shatter cannot break `target`: it breaks rigid bodies only. The
/// Targets menu shows it after the item's name, so it does not name Shatter.
pub fn shatter_refuses(target: &Item) -> Option<&'static str> {
    (target.fields.physics() != PhysicsSetting::Body).then_some("Not a Body: set its Physics to Body")
}

/// Why a Shatter card's Break does nothing: no target is a Body.
pub fn break_unavailable(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let ItemFields::Modifier(card) = &item.fields else {
        return None;
    };
    if card.modifier != ModifierType::Shatter {
        return None;
    }
    let mut any_body = false;
    doc.walk(|other, ancestors| {
        let aimed = match &card.targets {
            ModifierTargets::Scene => true,
            ModifierTargets::Items(ids) => ids.contains(&other.id) || ancestors.iter().any(|above| ids.contains(&above.id)),
        };
        any_body |= aimed && is_object_or_group(other) && shatter_refuses(other).is_none();
    });
    (!any_body).then_some("None of this card's targets is a Body: set a target's Physics to Body")
}

/// The Type row's choices, one per entry of [`MODIFIER_TYPES`].
static TYPE_CHOICES: [Choice; MODIFIER_TYPES.len()] = {
    let mut out = [Choice { key: "", label: "" }; MODIFIER_TYPES.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = Choice { key: MODIFIER_TYPES[i].key, label: MODIFIER_TYPES[i].label };
        i += 1;
    }
    out
};

fn card(fields: &ItemFields) -> Option<&ModifierFields> {
    match fields {
        ItemFields::Modifier(m) => Some(m),
        _ => None,
    }
}

fn card_mut<'f>(fields: &'f mut ItemFields, row: &str, value: &FieldValue) -> Result<&'f mut ModifierFields, String> {
    match fields {
        ItemFields::Modifier(m) => Ok(m),
        _ => Err(wrong_value(row, value)),
    }
}

fn is_shatter(fields: &ItemFields) -> bool {
    card(fields).is_some_and(|m| m.modifier == ModifierType::Shatter)
}

/// Shatter's Pieces: a closed set, since a piece is a body the world makes.
const PIECES: &[Choice] = &[
    Choice { key: "2", label: "2" },
    Choice { key: "3", label: "3" },
    Choice { key: "4", label: "4" },
    Choice { key: "6", label: "6" },
    Choice { key: "8", label: "8" },
    Choice { key: "12", label: "12" },
    Choice { key: "16", label: "16" },
    Choice { key: "24", label: "24" },
    Choice { key: "32", label: "32" },
];

/// A Modifier's target picks: Objects and Groups; Shatter refuses a target
/// that is not a Body.
fn target_refused(card_fields: &ItemFields, target: &Item) -> Option<&'static str> {
    is_shatter(card_fields).then(|| shatter_refuses(target)).flatten()
}

pub static MODIFIER_FIELDS: [FieldRow; 4] = [
    FieldRow {
        id: "modifier_type",
        label: "Type",
        section: M,
        control: FieldControl::Closed(&TYPE_CHOICES),
        applies: always,
        disabled: enabled,
        read: |fields| FieldValue::Choice(card(fields).map_or("", |m| modifier_type(m.modifier).key).to_owned()),
        // A swap keeps the card's place, toggle and targets; its rows start
        // at the new type's defaults.
        write: |fields, value| {
            let FieldValue::Choice(key) = value else {
                return Err(wrong_value("modifier_type", value));
            };
            let entry = MODIFIER_TYPES.iter().find(|entry| entry.key == key).ok_or_else(|| wrong_value("modifier_type", value))?;
            let m = card_mut(fields, "modifier_type", value)?;
            if m.modifier != entry.modifier {
                m.modifier = entry.modifier;
                m.pieces = DEFAULT_PIECES;
            }
            Ok(())
        },
    },
    FieldRow {
        id: "on",
        label: "On",
        section: M,
        control: on_off(),
        applies: always,
        disabled: enabled,
        read: |fields| is_on(card(fields).is_some_and(|m| m.on)),
        write: |fields, value| {
            let on = parse_on("on", value)?;
            card_mut(fields, "on", value)?.on = on;
            Ok(())
        },
    },
    FieldRow {
        id: "targets",
        label: "Targets",
        section: M,
        control: FieldControl::ItemSet { items: is_object_or_group, all: "Whole scene", refused: target_refused },
        applies: always,
        disabled: enabled,
        read: |fields| match card(fields).map(|m| &m.targets) {
            Some(ModifierTargets::Items(ids)) => FieldValue::Items(ids.clone()),
            _ => FieldValue::Everything,
        },
        write: |fields, value| {
            let targets = match value {
                FieldValue::Everything => ModifierTargets::Scene,
                // Unchoosing the last target leaves a card that acts on nothing.
                FieldValue::Items(ids) => ModifierTargets::Items(ids.clone()),
                _ => return Err(wrong_value("targets", value)),
            };
            card_mut(fields, "targets", value)?.targets = targets;
            Ok(())
        },
    },
    FieldRow {
        id: "pieces",
        label: "Pieces",
        section: M,
        control: FieldControl::Closed(PIECES),
        applies: is_shatter,
        disabled: enabled,
        read: |fields| FieldValue::Choice(card(fields).map_or(DEFAULT_PIECES, |m| m.pieces).to_string()),
        write: |fields, value| {
            let FieldValue::Choice(key) = value else {
                return Err(wrong_value("pieces", value));
            };
            if !PIECES.iter().any(|choice| choice.key == key) {
                return Err(wrong_value("pieces", value));
            }
            let pieces = key.parse().map_err(|_| wrong_value("pieces", value))?;
            card_mut(fields, "pieces", value)?.pieces = pieces;
            Ok(())
        },
    },
];
