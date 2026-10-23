//! The structural field rows of the (kind, property) table: the rows a card
//! draws for an item's fields (Shape, Physics, Light type, ...), as opposed
//! to the manifest rows in [`super::PropertyRow`]. A field is never a
//! manifest param, so it is never mapped or modulated (ruling 3); a pick
//! sets the item's `ItemFields` through `SetSceneItemFieldsCommand`.
//!
//! One list per kind, in that kind's file. The panel draws every list the
//! same way: a closed set of values, an item reference with a filter, or a
//! file picker. Nothing in the panel knows a kind's fields.

use manifold_foundation::scene_surface::ControlGroup;

use crate::LayerId;
use crate::scene_doc::{Item, ItemFields, ItemId, ItemKind, SceneDoc};

/// One value of a closed set. `key` is what a pick sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub key: &'static str,
    pub label: &'static str,
}

/// How a field row picks its value.
#[derive(Debug, Clone, Copy)]
pub enum FieldControl {
    /// On or off: drawn as a switch. Its value is [`is_on`].
    OnOff,
    /// A closed set of values.
    Closed(&'static [Choice]),
    /// A closed set, then every item `items` passes.
    ClosedOrItem { closed: &'static [Choice], items: fn(&Item) -> bool },
    /// One item reference, or none (`none` names the empty value).
    Item { items: fn(&Item) -> bool, none: &'static str },
    /// Any number of items, or everything (`all`). A pick toggles the item in
    /// the set. `refused` gives why the item holding the row (second) cannot
    /// take a candidate (third): the choice is drawn disabled.
    ItemSet { items: fn(&Item) -> bool, all: &'static str, refused: fn(&SceneDoc, &Item, &Item) -> Option<&'static str> },
    /// A closed set, then every layer in the project.
    ClosedOrLayer(&'static [Choice]),
    /// A file picker.
    File { extensions: &'static [&'static str] },
    /// One of a list the item itself holds (an import's clips): the key is
    /// the position, the label the name.
    Listed { names: fn(&ItemFields) -> &[String] },
}

/// What a field holds, or what a pick sets it to.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Choice(String),
    Item(Option<ItemId>),
    /// What an `ItemSet` holds when items are chosen; empty is nothing.
    Items(Vec<ItemId>),
    /// What an `ItemSet` holds when none is chosen: everything.
    Everything,
    File(String),
    Layer(LayerId),
}

/// One field row of a kind's card.
#[derive(Clone, Copy)]
pub struct FieldRow {
    pub id: &'static str,
    pub label: &'static str,
    pub section: ControlGroup,
    pub control: FieldControl,
    /// Whether the row exists for an item with these fields.
    pub applies: fn(&ItemFields) -> bool,
    /// Why the control cannot be used right now, or `None`. The row is drawn
    /// disabled with the reason on hover, never as standing text.
    pub disabled: fn(&SceneDoc, &Item) -> Option<&'static str>,
    pub read: fn(&ItemFields) -> FieldValue,
    pub write: fn(&mut ItemFields, &FieldValue) -> Result<(), String>,
}

impl std::fmt::Debug for FieldRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldRow").field("id", &self.id).field("section", &self.section).finish()
    }
}

pub(super) fn always(_: &ItemFields) -> bool {
    true
}

pub(super) fn enabled(_: &SceneDoc, _: &Item) -> Option<&'static str> {
    None
}

/// The error a write gives for a value its row does not take.
pub(super) fn wrong_value(row: &str, value: &FieldValue) -> String {
    format!("{row} does not take {value:?}")
}

/// Whether `item` is a kind placed in the world and given a physics setting
/// or drawn per particle: an Object or a Group.
pub fn is_object_or_group(item: &Item) -> bool {
    matches!(item.kind(), ItemKind::Object | ItemKind::Group)
}

pub fn is_material(item: &Item) -> bool {
    item.kind() == ItemKind::Material
}

/// Every field row of `kind`.
pub fn field_rows(kind: ItemKind) -> &'static [FieldRow] {
    match kind {
        ItemKind::Group => &super::group::GROUP_FIELDS,
        ItemKind::Object => &super::object::OBJECT_FIELDS,
        ItemKind::Camera => &super::camera::CAMERA_FIELDS,
        ItemKind::Light => &super::light::LIGHT_FIELDS,
        ItemKind::Material => &super::material::MATERIAL_FIELDS,
        ItemKind::Water => &super::water::WATER_FIELDS,
        ItemKind::WaterSource => &super::water_source::WATER_SOURCE_FIELDS,
        ItemKind::Swarm => &super::swarm::SWARM_FIELDS,
        ItemKind::Force => &super::force::FORCE_FIELDS,
        ItemKind::Modifier => &super::modifier::MODIFIER_FIELDS,
        ItemKind::World => &super::world::WORLD_FIELDS,
    }
}

/// Why `item` cannot take the choice `key` of its field `field` right now,
/// or `None`. The choice is drawn disabled with the reason, and a pick of it
/// is refused.
pub fn choice_disabled(doc: &SceneDoc, item: &Item, field: &str, key: &str) -> Option<&'static str> {
    match item.kind() {
        ItemKind::Water => super::water::choice_unavailable(item, field, key),
        ItemKind::WaterSource => super::water_source::choice_unavailable(doc, item, field, key),
        ItemKind::Camera => super::camera::choice_unavailable(doc, field, key),
        ItemKind::Modifier => super::scene_wide::type_unavailable(doc, item, field, key).or_else(|| super::copies::type_moves_no_copies(doc, item, field, key)),
        ItemKind::Object | ItemKind::Group if field == "physics" && key != "none" => physics_refused(doc, item),
        _ => None,
    }
}

/// Why `item` cannot take `pick` as the item of its field `field` right now,
/// or `None`: the item picker's sibling of [`choice_disabled`].
pub fn item_disabled(doc: &SceneDoc, item: &Item, field: &str, pick: &Item) -> Option<&'static str> {
    match item.kind() {
        ItemKind::Swarm => super::swarm::pick_unavailable(doc, item, field, pick),
        _ => None,
    }
}

/// The field rows that exist for an item with `fields`.
pub fn applicable_fields(fields: &ItemFields) -> impl Iterator<Item = &'static FieldRow> + '_ {
    field_rows(fields.kind()).iter().filter(move |row| (row.applies)(fields))
}

/// The field row `id` of an item with `fields`, if it exists there.
pub fn field_for(fields: &ItemFields, id: &str) -> Option<&'static FieldRow> {
    applicable_fields(fields).find(|row| row.id == id)
}

/// The fields `item` would have after `row` is set to `value`. The item's
/// kind is kept; a refused pick (a disabled row, a value the row does not
/// take) is an error and changes nothing.
pub fn set_field(doc: &SceneDoc, item: &Item, row: &FieldRow, value: &FieldValue) -> Result<ItemFields, String> {
    if let Some(reason) = (row.disabled)(doc, item) {
        return Err(reason.to_owned());
    }
    if let FieldValue::Choice(key) = value
        && let Some(reason) = choice_disabled(doc, item, row.id, key)
    {
        return Err(reason.to_owned());
    }
    if let FieldValue::Item(Some(id)) = value
        && let Some(pick) = doc.find(id)
        && let Some(reason) = item_disabled(doc, item, row.id, pick)
    {
        return Err(reason.to_owned());
    }
    let mut fields = item.fields.clone();
    (row.write)(&mut fields, value)?;
    Ok(fields)
}

pub(super) const fn on_off() -> FieldControl {
    FieldControl::OnOff
}

/// The choice key of an on/off field's value.
pub fn on_off_key(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// What an on/off field holds, or what a pick sets it to.
pub fn is_on(value: bool) -> FieldValue {
    FieldValue::Choice(on_off_key(value).to_owned())
}

/// `true` for an "on" pick, `false` for "off".
pub(super) fn parse_on(row: &str, value: &FieldValue) -> Result<bool, String> {
    match value {
        FieldValue::Choice(key) if key == "on" => Ok(true),
        FieldValue::Choice(key) if key == "off" => Ok(false),
        other => Err(wrong_value(row, other)),
    }
}

const PHYSICS_CHOICES: &[Choice] = &[
    Choice { key: "none", label: "None" },
    Choice { key: "collider", label: "Collider" },
    Choice { key: "body", label: "Body" },
];

fn physics_of(fields: &ItemFields) -> crate::scene_doc::PhysicsSetting {
    fields.physics()
}

fn has_body_below(item: &Item) -> bool {
    item.children.iter().any(|child| child.fields.physics() == crate::scene_doc::PhysicsSetting::Body || has_body_below(child))
}

/// A Body inside a Body is refused (section 7.8): the row is disabled with
/// the reason. A group's Body is one compound body of everything inside.
fn physics_blocked(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let mut inside_body = false;
    doc.walk(|other, ancestors| {
        if other.id == item.id {
            inside_body = ancestors.iter().any(|a| a.fields.physics() == crate::scene_doc::PhysicsSetting::Body);
        }
    });
    if inside_body {
        return Some("A group above is already a Body, and everything inside it moves with it");
    }
    if item.fields.physics() != crate::scene_doc::PhysicsSetting::Body && has_body_below(item) {
        return Some("A part inside is already a Body, so this cannot be one too");
    }
    None
}

/// The Physics row: while the item has none, refused inside a Body, above a
/// Body, for copies on a surface, or when it cannot take a body. None always
/// stays pickable once a setting is on, so a Collider inside a Body group can
/// be switched off.
fn physics_disabled(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    (item.fields.physics() == crate::scene_doc::PhysicsSetting::None).then(|| physics_refused(doc, item)).flatten()
}

/// Why this item can take no Collider or Body; with a setting already on,
/// only those two choices are refused.
fn physics_refused(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    physics_blocked(doc, item)
        .or_else(|| super::copies::surface_holds_copies(item))
        .or_else(|| super::physics_unavailable(doc, &item.id))
}

/// The Copies switch: refused while off when the item cannot take copies.
fn copies_disabled(_doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    if item.fields.copies() {
        return None;
    }
    super::copies_unavailable(item)
}

fn write_physics(fields: &mut ItemFields, value: &FieldValue) -> Result<(), String> {
    use crate::scene_doc::PhysicsSetting;
    let FieldValue::Choice(key) = value else {
        return Err(wrong_value("physics", value));
    };
    let setting = match key.as_str() {
        "none" => PhysicsSetting::None,
        "collider" => PhysicsSetting::Collider,
        "body" => PhysicsSetting::Body,
        _ => return Err(wrong_value("physics", value)),
    };
    match fields {
        ItemFields::Object(o) => o.physics = setting,
        ItemFields::Group(g) => g.physics = setting,
        _ => return Err("only an Object or a Group has physics".to_owned()),
    }
    Ok(())
}

/// The physics setting of an Object or a Group.
pub(super) const PHYSICS: FieldRow = FieldRow {
    id: "physics",
    label: "Physics",
    section: ControlGroup::Body,
    control: FieldControl::Closed(PHYSICS_CHOICES),
    applies: always,
    disabled: physics_disabled,
    read: |fields| {
        FieldValue::Choice(
            match physics_of(fields) {
                crate::scene_doc::PhysicsSetting::None => "none",
                crate::scene_doc::PhysicsSetting::Collider => "collider",
                crate::scene_doc::PhysicsSetting::Body => "body",
            }
            .to_owned(),
        )
    },
    write: write_physics,
};

/// Copies on or off; the numbers are live params.
pub(super) const COPIES: FieldRow = FieldRow {
    id: "copies",
    label: "Copies",
    section: ControlGroup::Copies,
    control: on_off(),
    applies: always,
    disabled: copies_disabled,
    read: |fields| {
        is_on(match fields {
            ItemFields::Object(o) => o.copies,
            ItemFields::Group(g) => g.copies,
            _ => false,
        })
    },
    write: |fields, value| {
        let on = parse_on("copies", value)?;
        match fields {
            ItemFields::Object(o) => o.copies = on,
            ItemFields::Group(g) => g.copies = on,
            _ => return Err("only an Object or a Group has copies".to_owned()),
        }
        Ok(())
    },
};

/// The material an Object or Group draws with (a Group's reaches every child
/// with none of its own).
pub(super) const MATERIAL_BINDING: FieldRow = FieldRow {
    id: "material",
    label: "Material",
    section: ControlGroup::Material,
    control: FieldControl::Item { items: is_material, none: "Default" },
    applies: always,
    disabled: enabled,
    read: |fields| FieldValue::Item(fields.material().cloned()),
    write: |fields, value| {
        let FieldValue::Item(material) = value else {
            return Err(wrong_value("material", value));
        };
        match fields {
            ItemFields::Object(o) => o.material = material.clone(),
            ItemFields::Group(g) => g.material = material.clone(),
            ItemFields::Swarm(s) => s.material = material.clone(),
            ItemFields::Water(w) => w.material = material.clone(),
            _ => return Err("only an Object, a Group, a Swarm or a Water binds a material".to_owned()),
        }
        Ok(())
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_doc::{GroupFields, ObjectFields, PhysicsSetting};

    /// A Collider inside a Body group can always be switched back to None;
    /// only Collider and Body stay refused there, with the reason.
    #[test]
    fn none_stays_pickable_inside_a_body_group() {
        let part = Item {
            id: ItemId::new("part"),
            name: "Part".into(),
            fields: ItemFields::Object(ObjectFields { physics: PhysicsSetting::Collider, ..ObjectFields::default() }),
            children: Vec::new(),
        };
        let group = Item {
            id: ItemId::new("group"),
            name: "Group".into(),
            fields: ItemFields::Group(GroupFields { physics: PhysicsSetting::Body, ..GroupFields::default() }),
            children: vec![part.clone()],
        };
        let doc = SceneDoc { items: vec![group], ..SceneDoc::default() };
        let none = FieldValue::Choice("none".into());
        let fields = set_field(&doc, &part, &PHYSICS, &none).expect("None is pickable");
        assert_eq!(fields.physics(), PhysicsSetting::None);
        assert!(set_field(&doc, &part, &PHYSICS, &FieldValue::Choice("body".into())).is_err());
        assert!(choice_disabled(&doc, &part, "physics", "collider").is_some());
    }
}
