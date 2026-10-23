//! Copies: the section on an Object or Group with Copies on. Every row
//! reaches the copies body, the `node.rigid_body` that lays the copies out
//! and publishes their instances (SCENE_DOCUMENT_DESIGN.md section 7.9
//! (Copies)). Count, Layout, Spacing, Columns and Turn are live params: the
//! body reads them every frame.
//!
//! On Surface sits the copies on another item's surface (`node.scatter_on_mesh`)
//! in place of a Layout. The copy fields are Modifier types aimed at items
//! with Copies ([`COPY_FIELD_TYPES`]): each compiles a stock recipe that acts
//! on the targets' copies (the `instances` endpoint).

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{FieldControl, FieldRow, FieldValue, enabled, is_object_or_group, wrong_value};
use super::modifier::{ModifierTypeEntry, ON_IS_THE_TOGGLE, entry, held, live};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::copies_body::SPACING_MIN;
use crate::scene_doc::{Item, ItemFields, ModifierType, Shape};

/// The role of the copies body inside its item.
pub const COPIES_ROLE: &str = "copies";
const BODY: Reach = Reach::Own(COPIES_ROLE);
const C: ControlGroup = ControlGroup::Copies;

/// A new Copies section starts with this many copies.
pub const START_COUNT: f32 = 4.0;

fn on(fields: &ItemFields) -> bool {
    fields.copies()
}

/// Copies laid out by Layout: on, and not on a surface.
fn laid_out(fields: &ItemFields) -> bool {
    fields.copies() && fields.copies_surface().is_none()
}

/// Copies sitting on another item's surface.
fn on_surface(fields: &ItemFields) -> bool {
    fields.copies_surface().is_some()
}

/// The role of the scatter that sits copies on a surface.
pub const SURFACE_ROLE: &str = "surface";
const SURFACE: Reach = Reach::Own(SURFACE_ROLE);

const fn surface(property: &'static str, param: &'static str, exposure: Exposure) -> PropertyRow {
    row(property, SURFACE, C, exposure).param(param).when(on_surface)
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

pub static COPIES_ROWS: [PropertyRow; 32] = [
    card("count", "copy_count", perform(CardBand::Scene, 0)).default(START_COUNT),
    card("layout", "copy_layout", perform(CardBand::Scene, 1)).when(laid_out),
    card("spacing", "copy_size_spacing", perform(CardBand::Scene, 2)).unit(Unit::SizeMultiple).limit(HardLimit::AtLeast(SPACING_MIN)).when(laid_out),
    card("columns", "copy_columns", setup()).when(laid_out),
    card("turn_x", "copy_turn_x", setup()).limit(HardLimit::Unlimited).when(laid_out),
    card("turn_y", "copy_turn_y", setup()).limit(HardLimit::Unlimited).when(laid_out),
    card("turn_z", "copy_turn_z", setup()).limit(HardLimit::Unlimited).when(laid_out),
    surface("surface_seed", "seed", perform(CardBand::Scene, 3)).name("Seed"),
    surface("surface_align", "align_to_normal", perform(CardBand::Scene, 4)).name("Align to Surface"),
    surface("surface_scale_min", "scale_min", perform(CardBand::Scene, 5)).name("Size Min"),
    surface("surface_scale_max", "scale_max", perform(CardBand::Scene, 6)).name("Size Max"),
    surface("surface_count", "count", Exposure::Automatic("the Copies section's Count")),
    surface("surface_capacity", "max_capacity", Exposure::Internal("the most copies one world makes")),
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
    auto("copies_parts", "parts", "the objects inside a copied group"),
];

/// Whether an Object of this shape can be copied: every solid can (built-in,
/// imported or curve shape), an Ocean surface cannot.
pub fn can_copy(shape: &Shape) -> bool {
    *shape != Shape::Ocean
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
        ItemFields::Object(object) if !can_copy(&object.shape) => Some("An Ocean surface cannot be copied"),
        ItemFields::Group(_) => match copied_parts(&item.children) {
            0 => Some("Nothing in this group has a shape to copy"),
            _ => None,
        },
        _ => None,
    }
}

/// Why physics cannot move an item's copies: they sit on a surface.
pub fn surface_holds_copies(item: &Item) -> Option<&'static str> {
    item.fields.copies_surface().map(|_| "Copies on a surface stay on it: physics does not move them")
}

/// On Surface: the item the copies sit on, in place of a Layout. None lays
/// them out by Layout.
pub(super) const COPIES_SURFACE: FieldRow = FieldRow {
    id: "copies_surface",
    label: "On Surface",
    section: C,
    control: FieldControl::Item { items: is_object_or_group, none: "None" },
    applies: on,
    disabled: enabled,
    read: |fields| {
        FieldValue::Item(match fields {
            ItemFields::Object(o) => o.copies_surface.clone(),
            ItemFields::Group(g) => g.copies_surface.clone(),
            _ => None,
        })
    },
    write: |fields, value| {
        let FieldValue::Item(surface) = value else {
            return Err(wrong_value("copies_surface", value));
        };
        match fields {
            ItemFields::Object(o) => o.copies_surface.clone_from(surface),
            ItemFields::Group(g) => g.copies_surface.clone_from(surface),
            _ => return Err(wrong_value("copies_surface", value)),
        }
        Ok(())
    },
};

/// Copy Wave: a travelling wave pushes each copy by the wave's value at its
/// position (`wave_field_3d` over `copy_positions`, then `displace_copies`).
pub static COPY_WAVE_ROWS: [PropertyRow; 10] = [
    live("amount", 1),
    live("frequency", 2),
    live("phase", 3),
    live("wave_x", 4),
    live("wave_y", 5),
    live("wave_z", 6),
    live("push_x", 7),
    live("push_y", 8),
    live("push_z", 9),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// Copy Jitter: noise moves each copy and a fixed random turn rotates it.
pub static COPY_JITTER_ROWS: [PropertyRow; 6] = [
    live("amount", 1),
    live("turn", 2),
    live("scale", 3),
    live("drift", 4),
    live("evolve", 5),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// Copy Noise: each copy pushed by its own noise value; Detail 1 is plain
/// simplex noise, more is fractal noise.
pub static COPY_NOISE_ROWS: [PropertyRow; 8] = [
    live("amount", 1),
    live("scale", 2),
    live("detail", 3),
    live("evolve", 4),
    live("push_x", 5),
    live("push_y", 6),
    live("push_z", 7),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// Copy Blend: the copies move toward a second layout as Mix rises.
pub static COPY_BLEND_ROWS: [PropertyRow; 9] = [
    live("mix", 1),
    live("layout", 2),
    live("count", 3),
    live("width", 4),
    live("height", 5),
    live("depth", 6),
    live("size", 7),
    live("seed", 8),
    held("enabled", ON_IS_THE_TOGGLE),
];

/// Whether a Modifier type acts on an item's copies rather than its mesh.
pub fn acts_on_copies(modifier: ModifierType) -> bool {
    COPY_FIELD_TYPES.iter().any(|entry| entry.modifier == modifier)
}

/// The copy field Modifier types, in menu order.
pub(super) const COPY_FIELD_TYPES: [ModifierTypeEntry; 4] = [
    entry(ModifierType::CopyWave, "copy_wave", "Copy Wave", "CopyWave", false, &COPY_WAVE_ROWS),
    entry(ModifierType::CopyJitter, "copy_jitter", "Copy Jitter", "CopyJitter", false, &COPY_JITTER_ROWS),
    entry(ModifierType::CopyNoise, "copy_noise", "Copy Noise", "CopyNoise", false, &COPY_NOISE_ROWS),
    entry(ModifierType::CopyBlend, "copy_blend", "Copy Blend", "CopyBlend", false, &COPY_BLEND_ROWS),
];
