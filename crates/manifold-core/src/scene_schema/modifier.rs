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

use super::copies::COPY_FIELD_TYPES;
use super::deformers::DEFORMER_TYPES;
use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, is_on, on_off,
    parse_on, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{
    DEFAULT_PIECES, Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets,
    ModifierTexture, ModifierType, PhysicsSetting, SceneDoc, Shape,
};

const M: ControlGroup = ControlGroup::Modifier;
const MASK: ControlGroup = ControlGroup::Mask;
const BAND: CardBand = CardBand::Modifiers;

/// A performance row of a modifier, at `order` on its card.
pub(super) const fn live(property: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Modifier, M, perform(BAND, order))
}

/// A recipe param the card never shows.
pub(super) const fn held(property: &'static str, why: &'static str) -> PropertyRow {
    row(property, Reach::Modifier, M, Exposure::Internal(why))
}

pub(super) const ON_IS_THE_TOGGLE: &str = "The card's On toggle turns the modifier on and off";
const NO_HITS: &str = "Modifiers have no clip trigger or hit rows (ruling 22)";

/// A mask row, in the Mask section.
pub(super) const fn mask(property: &'static str, order: u8) -> PropertyRow {
    row(property, Reach::Modifier, MASK, perform(BAND, order))
}

/// A mask row measured in the target's radius: 1 is the target's radius.
pub(super) const fn mask_radius(
    property: &'static str,
    name: &'static str,
    order: u8,
) -> PropertyRow {
    mask(property, order).name(name).unit(Unit::Radius)
}

/// A mask centre: where the region sits, from the target's centre.
pub(super) const fn mask_centre(
    property: &'static str,
    name: &'static str,
    order: u8,
) -> PropertyRow {
    mask_radius(property, name, order)
        .range(-2.0, 2.0)
        .limit(HardLimit::Unlimited)
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
pub(super) use mask_rows;

pub(super) const fn join<const A: usize, const B: usize, const N: usize>(
    a: [PropertyRow; A],
    b: [PropertyRow; B],
) -> [PropertyRow; N] {
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
pub static SHATTER_ROWS: [PropertyRow; 2] = [
    live("shatter", 1).name("Break"),
    held("clip_trigger", NO_HITS),
];

/// One modifier type: everything the card, the Add menu and the compiler
/// know about it. A new type is one entry here and one `ModifierType`
/// variant.
#[derive(Debug, Clone, Copy)]
pub struct ModifierTypeEntry {
    pub modifier: ModifierType,
    /// What a Type pick sends.
    pub key: &'static str,
    pub label: &'static str,
    /// The stock scene modifier recipe it compiles.
    pub recipe: &'static str,
    /// It moves vertices, so Shatter leaves a shape it reaches whole
    /// ([`shatter_parts`]).
    pub deforms: bool,
    /// It bends surfaces smoothly, so a built-in shape it reaches is split
    /// fine enough to curve. Cuts, snaps and breaks keep the shape as it is.
    pub smooth_detail: bool,
    pub rows: &'static [PropertyRow],
}

pub(super) const fn entry(
    modifier: ModifierType,
    key: &'static str,
    label: &'static str,
    recipe: &'static str,
    deforms: bool,
    rows: &'static [PropertyRow],
) -> ModifierTypeEntry {
    ModifierTypeEntry {
        modifier,
        key,
        label,
        recipe,
        deforms,
        smooth_detail: false,
        rows,
    }
}

impl ModifierTypeEntry {
    /// Marks a type whose bends need a smooth surface.
    pub(super) const fn smooth(self) -> Self {
        Self {
            smooth_detail: true,
            ..self
        }
    }
}

/// The mesh and scene modifiers.
const CORE_TYPES: [ModifierTypeEntry; 7] = [
    entry(
        ModifierType::SurfacePeel,
        "surface_peel",
        "Surface Peel",
        "SurfacePeel",
        true,
        &SURFACE_PEEL_ROWS,
    ),
    entry(
        ModifierType::OrderedRecon,
        "ordered_recon",
        "Ordered Recon",
        "OrderedRecon",
        true,
        &ORDERED_RECON_ROWS,
    ),
    entry(
        ModifierType::SurfaceWaves,
        "surface_waves",
        "Surface Waves",
        "SurfaceWaves",
        true,
        &SURFACE_WAVES_ROWS,
    )
    .smooth(),
    entry(
        ModifierType::SpatialEchoes,
        "spatial_echoes",
        "Spatial Echoes",
        "SpatialEchoes",
        false,
        &SPATIAL_ECHOES_ROWS,
    ),
    entry(
        ModifierType::ElasticSculpture,
        "elastic_sculpture",
        "Elastic Sculpture",
        "ElasticSculpture",
        true,
        &ELASTIC_SCULPTURE_ROWS,
    )
    .smooth(),
    entry(
        ModifierType::VortexFragments,
        "vortex_fragments",
        "Vortex Fragments",
        "VortexFragments",
        true,
        &VORTEX_FRAGMENTS_ROWS,
    ),
    entry(
        ModifierType::Shatter,
        "shatter",
        "Shatter",
        "Shatter",
        false,
        &SHATTER_ROWS,
    ),
];

/// The scene-wide types: they act on the whole scene and have no Targets row.
const SCENE_WIDE_TYPES: [ModifierTypeEntry; 3] = [
    entry(
        ModifierType::SceneLoop,
        "scene_loop",
        "Scene Loop",
        "SceneLoop",
        false,
        &super::scene_wide::SCENE_LOOP_ROWS,
    ),
    entry(
        ModifierType::MathView,
        "math_view",
        "Math View",
        "MathView",
        false,
        &super::scene_wide::MATH_VIEW_ROWS,
    ),
    entry(
        ModifierType::SceneMirror,
        "scene_mirror",
        "Scene Mirror",
        "SceneMirror",
        false,
        &super::scene_wide::SCENE_MIRROR_ROWS,
    ),
];

const fn concat<const A: usize, const B: usize, const N: usize>(
    a: [ModifierTypeEntry; A],
    b: [ModifierTypeEntry; B],
) -> [ModifierTypeEntry; N] {
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

/// Every modifier type, in menu order.
pub static MODIFIER_TYPES: [ModifierTypeEntry; 33] = concat::<30, 3, 33>(
    concat::<26, 4, 30>(
        concat::<7, 19, 26>(CORE_TYPES, DEFORMER_TYPES),
        COPY_FIELD_TYPES,
    ),
    SCENE_WIDE_TYPES,
);

/// The entry of a modifier type.
pub fn modifier_type(modifier: ModifierType) -> &'static ModifierTypeEntry {
    MODIFIER_TYPES
        .iter()
        .find(|entry| entry.modifier == modifier)
        .expect("every modifier type has an entry")
}

/// The rows of the modifier type an item with `fields` has; none for any
/// other kind.
pub fn modifier_rows(fields: &ItemFields) -> &'static [PropertyRow] {
    match fields {
        ItemFields::Modifier(m) => modifier_type(m.modifier).rows,
        _ => &[],
    }
}

/// Whether a card aimed at `targets` reaches `item`, whose ancestors are
/// `ancestors`: directly, through a Group target, or as the whole scene.
fn reaches(targets: &ModifierTargets, item: &Item, ancestors: &[&Item]) -> bool {
    match targets {
        ModifierTargets::Scene => true,
        ModifierTargets::Items(ids) => {
            ids.contains(&item.id) || ancestors.iter().any(|above| ids.contains(&above.id))
        }
    }
}

/// Why the Shatter card `shatter` cannot break `target`. The Targets menu
/// shows it after the item's name, so it does not name Shatter.
pub fn shatter_refuses(doc: &SceneDoc, shatter: &Item, target: &Item) -> Option<&'static str> {
    shatter_parts(doc, &shatter.id, target).err()
}

/// The parts the Shatter card `shatter` breaks of the Body `body`: the
/// Objects in it no deforming card bends, or why it breaks none. The picker
/// and the compiler (`shatterable` in `scene_compile/modifiers.rs`) both
/// read this, so the panel says what the compile does.
pub fn shatter_parts(
    doc: &SceneDoc,
    shatter: &ItemId,
    body: &Item,
) -> Result<Vec<ItemId>, &'static str> {
    // The parts reason below names this limit.
    const _: () = assert!(manifold_foundation::mesh_partition::MAX_PIECES == 64);
    if body.fields.physics() != PhysicsSetting::Body {
        return Err("Not a Body: set its Physics to Body");
    }
    // Copies are a group of bodies the world lays out, not one body.
    if body.fields.copies() {
        return Err("Copies on: each copy is a body of the copies, not one body to break");
    }
    let on = |item: &Item| card(&item.fields).is_some_and(|m| m.on);
    let bending: Vec<&ModifierTargets> = doc
        .items
        .iter()
        .filter(|item| on(item))
        .filter_map(|item| card(&item.fields))
        .filter(|m| modifier_type(m.modifier).deforms)
        .map(|m| &m.targets)
        .collect();
    let (mut reached_by, mut parts, mut bent) = (Vec::new(), Vec::new(), false);
    doc.walk(|item, ancestors| {
        if item.id == body.id {
            // Shatter cards before this one in the chain that break this
            // body break it first.
            reached_by = doc
                .items
                .iter()
                .take_while(|card_item| &card_item.id != shatter)
                .filter(|card_item| on(card_item) && is_shatter(&card_item.fields))
                .filter(|card_item| {
                    card(&card_item.fields).is_some_and(|m| reaches(&m.targets, item, ancestors))
                })
                .map(|card_item| &card_item.id)
                .collect();
        }
        let inside = item.id == body.id || ancestors.iter().any(|above| above.id == body.id);
        if inside && item.kind() == ItemKind::Object {
            if bending
                .iter()
                .any(|targets| reaches(targets, item, ancestors))
            {
                bent = true;
            } else {
                parts.push(item.id.clone());
            }
        }
    });
    if reached_by
        .into_iter()
        .any(|earlier| shatter_parts(doc, earlier, body).is_ok())
    {
        Err("An earlier Shatter card already breaks this body")
    } else if parts.is_empty() && bent {
        Err("A modifier card bends every part: Shatter breaks only unbent shapes")
    } else if parts.is_empty() {
        Err("Nothing inside to break: add an Object")
    } else if parts.len() > 64 {
        Err(
            "More than 64 unbent parts: each piece loads the whole mesh twice, so 64 is the memory limit",
        )
    } else {
        Ok(parts)
    }
}

/// Why a Shatter card's Break does nothing: no target can break. Names the
/// first Body target's reason, else that none is a Body.
pub fn break_unavailable(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let ItemFields::Modifier(card) = &item.fields else {
        return None;
    };
    if card.modifier != ModifierType::Shatter {
        return None;
    }
    let (mut any_body, mut reason) = (false, None);
    doc.walk(|other, ancestors| {
        if !(reaches(&card.targets, other, ancestors) && is_object_or_group(other)) {
            return;
        }
        match shatter_refuses(doc, item, other) {
            None => any_body = true,
            Some(why) if other.fields.physics() == PhysicsSetting::Body => {
                reason.get_or_insert(why);
            }
            Some(_) => {}
        }
    });
    (!any_body).then(|| {
        reason.unwrap_or("None of this card's targets is a Body: set a target's Physics to Body")
    })
}

/// The Type row's choices, one per entry of [`MODIFIER_TYPES`].
static TYPE_CHOICES: [Choice; MODIFIER_TYPES.len()] = {
    let mut out = [Choice { key: "", label: "" }; MODIFIER_TYPES.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = Choice {
            key: MODIFIER_TYPES[i].key,
            label: MODIFIER_TYPES[i].label,
        };
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

fn card_mut<'f>(
    fields: &'f mut ItemFields,
    row: &str,
    value: &FieldValue,
) -> Result<&'f mut ModifierFields, String> {
    match fields {
        ItemFields::Modifier(m) => Ok(m),
        _ => Err(wrong_value(row, value)),
    }
}

/// A card that names its targets: every type but the scene-wide ones.
fn aimed(fields: &ItemFields) -> bool {
    card(fields).is_some_and(|m| !m.modifier.scene_wide())
}

fn is_shatter(fields: &ItemFields) -> bool {
    card(fields).is_some_and(|m| m.modifier == ModifierType::Shatter)
}

fn reads_texture(fields: &ItemFields) -> bool {
    card(fields).is_some_and(|m| m.modifier == ModifierType::DisplaceByTexture)
}

fn reads_file(fields: &ItemFields) -> bool {
    reads_texture(fields)
        && card(fields).is_some_and(|m| matches!(m.texture, ModifierTexture::File { .. }))
}

/// Displace by Texture's sources: the built-in noise or a file, then every
/// layer.
const TEXTURE_SOURCES: &[Choice] = &[
    Choice {
        key: "noise",
        label: "Noise",
    },
    Choice {
        key: "file",
        label: "File",
    },
];

/// Shatter's Pieces: a closed set, since a piece is a body the world makes.
const PIECES: &[Choice] = &[
    Choice {
        key: "2",
        label: "2",
    },
    Choice {
        key: "3",
        label: "3",
    },
    Choice {
        key: "4",
        label: "4",
    },
    Choice {
        key: "6",
        label: "6",
    },
    Choice {
        key: "8",
        label: "8",
    },
    Choice {
        key: "12",
        label: "12",
    },
    Choice {
        key: "16",
        label: "16",
    },
    Choice {
        key: "24",
        label: "24",
    },
    Choice {
        key: "32",
        label: "32",
    },
    Choice {
        key: "48",
        label: "48",
    },
    Choice {
        key: "64",
        label: "64",
    },
];

/// Why no card can aim at `target`: an Ocean is an endless sea cast from
/// the camera each frame, so it has no bounds for a modifier's frame. A
/// Water moves by its own simulation, which no card's frame reaches. A
/// card aimed at the whole scene or at a Group passes over both.
pub fn modifier_refuses(target: &Item) -> Option<&'static str> {
    match &target.fields {
        ItemFields::Object(object) if object.shape == Shape::Ocean => {
            Some("An Ocean is endless: it has no bounds for a modifier to bend in")
        }
        ItemFields::Water(_) => {
            Some("Water moves by its own simulation: a modifier cannot bend it")
        }
        _ => None,
    }
}

/// What the Targets menu lists: Objects and Groups, and Water, so it shows
/// with the reason no card aims at it.
fn is_target_kind(item: &Item) -> bool {
    is_object_or_group(item) || item.kind() == ItemKind::Water
}

/// What a new card aims at: the Objects and Groups in `selection` a card can
/// aim at, or the whole scene when none is selected.
pub fn new_card_targets(doc: &SceneDoc, selection: &[ItemId]) -> ModifierTargets {
    let aimed: Vec<ItemId> = selection
        .iter()
        .filter(|id| {
            doc.find(id).is_some_and(|item| {
                matches!(item.kind(), ItemKind::Object | ItemKind::Group)
                    && modifier_refuses(item).is_none()
            })
        })
        .cloned()
        .collect();
    if aimed.is_empty() {
        ModifierTargets::Scene
    } else {
        ModifierTargets::Items(aimed)
    }
}

/// A Modifier's target picks: Objects and Groups; never an Ocean or a Water; Shatter
/// refuses a target it cannot break, a copy type one without Copies.
fn target_refused(doc: &SceneDoc, modifier: &Item, target: &Item) -> Option<&'static str> {
    let moves_copies = matches!(&modifier.fields, ItemFields::Modifier(card) if super::copies::acts_on_copies(card.modifier));
    modifier_refuses(target)
        .or_else(|| {
            is_shatter(&modifier.fields)
                .then(|| shatter_refuses(doc, modifier, target))
                .flatten()
        })
        .or_else(|| {
            (moves_copies && !super::copies::holds_copies(target))
                .then_some(super::copies::TURN_ON_COPIES)
        })
}

/// The Modifier fields its card header carries instead of rows: Type and On
/// (SCENE_DOCUMENT_DESIGN.md section 8).
pub const HEADER_FIELDS: [&str; 2] = ["modifier_type", "on"];

pub static MODIFIER_FIELDS: [FieldRow; 6] = [
    FieldRow {
        id: "modifier_type",
        label: "Type",
        section: M,
        control: FieldControl::Closed(&TYPE_CHOICES),
        applies: always,
        disabled: enabled,
        read: |fields| {
            FieldValue::Choice(
                card(fields)
                    .map_or("", |m| modifier_type(m.modifier).key)
                    .to_owned(),
            )
        },
        // A swap keeps the card's place, toggle and targets; its rows start
        // at the new type's defaults.
        write: |fields, value| {
            let FieldValue::Choice(key) = value else {
                return Err(wrong_value("modifier_type", value));
            };
            let entry = MODIFIER_TYPES
                .iter()
                .find(|entry| entry.key == key)
                .ok_or_else(|| wrong_value("modifier_type", value))?;
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
        control: FieldControl::ItemSet {
            items: is_target_kind,
            all: "Whole scene",
            refused: target_refused,
        },
        applies: aimed,
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
        read: |fields| {
            FieldValue::Choice(
                card(fields)
                    .map_or(DEFAULT_PIECES, |m| m.pieces)
                    .to_string(),
            )
        },
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
    FieldRow {
        id: "texture",
        label: "Texture",
        section: M,
        control: FieldControl::ClosedOrLayer(TEXTURE_SOURCES),
        applies: reads_texture,
        disabled: enabled,
        read: |fields| match card(fields).map(|m| &m.texture) {
            Some(ModifierTexture::File { .. }) => FieldValue::Choice("file".to_owned()),
            Some(ModifierTexture::Layer { layer }) => FieldValue::Layer(layer.clone()),
            _ => FieldValue::Choice("noise".to_owned()),
        },
        write: |fields, value| {
            let m = card_mut(fields, "texture", value)?;
            m.texture = match value {
                FieldValue::Choice(key) if key == "noise" => ModifierTexture::Noise,
                FieldValue::Choice(key) if key == "file" => match &m.texture {
                    ModifierTexture::File { .. } => m.texture.clone(),
                    _ => ModifierTexture::File {
                        path: String::new(),
                    },
                },
                FieldValue::Layer(layer) => ModifierTexture::Layer {
                    layer: layer.clone(),
                },
                other => return Err(wrong_value("texture", other)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "texture_file",
        label: "Texture File",
        section: M,
        control: FieldControl::File {
            extensions: crate::file_loader::PICTURE_EXTENSIONS,
        },
        applies: reads_file,
        disabled: enabled,
        read: |fields| match card(fields).map(|m| &m.texture) {
            Some(ModifierTexture::File { path }) => FieldValue::File(path.clone()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| {
            let FieldValue::File(path) = value else {
                return Err(wrong_value("texture_file", value));
            };
            card_mut(fields, "texture_file", value)?.texture =
                ModifierTexture::File { path: path.clone() };
            Ok(())
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_doc::ObjectFields;

    /// The Targets menu lists Objects, Groups and Water; a Camera is never a
    /// target. Only an Ocean and a Water carry a reason no card can aim at them.
    #[test]
    fn targets_list_water_and_ocean_with_their_reason() {
        let object = Item::new(
            ItemId::new("object"),
            "Object",
            ItemFields::default_for(ItemKind::Object),
        );
        let ocean = Item::new(
            ItemId::new("ocean"),
            "Ocean",
            ItemFields::Object(ObjectFields {
                shape: Shape::Ocean,
                ..ObjectFields::default()
            }),
        );
        let water = Item::new(
            ItemId::new("water"),
            "Water",
            ItemFields::default_for(ItemKind::Water),
        );
        let group = Item::new(
            ItemId::new("group"),
            "Group",
            ItemFields::default_for(ItemKind::Group),
        );
        let camera = Item::new(
            ItemId::new("camera"),
            "Camera",
            ItemFields::default_for(ItemKind::Camera),
        );

        assert!(is_target_kind(&object));
        assert!(is_target_kind(&ocean));
        assert!(is_target_kind(&water));
        assert!(is_target_kind(&group));
        assert!(!is_target_kind(&camera));

        assert_eq!(modifier_refuses(&object), None);
        assert_eq!(modifier_refuses(&group), None);
        assert!(modifier_refuses(&ocean).is_some());
        assert!(modifier_refuses(&water).is_some());
    }
}
