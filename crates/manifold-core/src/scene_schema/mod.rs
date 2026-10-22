//! The (kind, property) table (SCENE_DOCUMENT_DESIGN.md section 8 (Cards by
//! kind)): what every param of every node the scene compiler emits is, per
//! item kind. One file per kind, so lanes add files instead of editing each
//! other's.
//!
//! Each row names the node it reaches (by the node's role inside the item),
//! declares its section (there is no fallback section), and marks the param
//! as on the card, automatic (with what sets it) or internal (with why it is
//! never reached). Only card rows become manifest params, with id
//! `<item id>.<property>`. A row that does not apply to an item's fields
//! (a cone angle on a Sun) is not emitted, so it never shows.
//!
//! The table never carries a label: the card shows the node param's own
//! label, unless the row names the property (a group's Visible is a
//! `node.value`). Limits are in the param's storage unit (radians for angles).

mod add_menu;
mod camera;
mod deformers;
mod fields;
mod copies;
mod curves;
mod force;
mod group;
mod light;
mod material;
mod modifier;
mod object;
mod ocean;
mod physics;
mod scene_level;
mod swarm;
mod water;
mod water_source;
mod world;

pub use add_menu::{ADD_ENTRIES, AddEntry, add_entry, unique_name};
pub use camera::{CAMERA_ROWS, FAR_CLIP, NEAR_CLIP};
pub use fields::{
    Choice, FieldControl, FieldRow, FieldValue, applicable_fields, choice_disabled, field_for, field_rows, is_material,
    is_object_or_group, set_field,
};
pub use copies::{COPIES_ROLE, COPIES_ROWS, START_COUNT, SURFACE_ROLE, can_copy, copies_unavailable};
pub use curves::{curve_rows, CURVE_AXIS, CURVE_MATH, CURVE_POLYGON, CURVE_RATIO, CURVE_T, CURVE_XY, SWEEP, TRIANGLES};
pub use force::{FORCE_ROWS, impulse_unavailable};
pub use group::GROUP_ROWS;
pub use light::LIGHT_ROWS;
pub use material::{MATERIAL_FAMILY, MATERIAL_ROWS, MaterialFamily, node_type as material_node_type};
pub use modifier::{
    MODIFIER_TYPES, ModifierTypeEntry, break_unavailable, modifier_rows, modifier_type, shatter_refuses,
};
pub use object::{OBJECT_ROWS, has_shape_name, shape_name};
use ocean::OCEAN_TABLES;
pub use physics::{BODY_ROLE, PHYSICS_ROWS, PhysicsPart, physics_parts, physics_unavailable};
pub use swarm::{MAX_COUNT as SWARM_MAX_COUNT, SWARM_ROWS};
pub use scene_level::{DEFAULT_MATERIAL_REASON, SCENE_LEVEL_ROWS};
pub use water::{
    DOMAIN_ROLE, PHYSICS_VIEW_SWITCHES, SURFACE_DETAIL_NODES, SURFACE_DETAIL_OFFSET, WATER_FAMILY_REASON, WATER_MATERIAL_ROLE,
    WATER_OBJECT_ROLE, WATER_ROWS, WHITEWATER_PER_CELL, WORLD_FOLLOWS, water_look,
    WaterFamily, WaterTemplate, template_roles, water_family,
};
pub use water_source::{DRAIN_ROLE, FILL_ROLE, INFLOW_ROLE, WATER_SOURCE_ROWS, role_index};
pub use world::{SPEED_RANGE, WORLD_ROWS};

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{
    CardBand, CardLabel, ControlGroup, Feature, Gate, RgbChannel, SceneParamDesc, SurfaceRole, Unit,
};

use crate::scene_doc::{ItemFields, ItemKind};

/// Which node a row reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// A node the compiler emits for this item, by role.
    Own(&'static str),
    /// The lens of every camera in the scene (the Environment's Exposure).
    EveryCameraLens,
    /// A node the compiler emits once for the whole scene, by role.
    Scene(&'static str),
    /// A node the compiler emits for this item and one it emits once for
    /// the scene, the same param on both (HDRI Rotation turns the sky
    /// behind the scene and the render's lighting together).
    OwnAndScene(&'static str, &'static str),
    /// The animation clock of every imported part below an import Group
    /// (its skeleton pose and morph weights), so one row plays them all.
    Clocks,
    /// A param of the scene modifier instances the item compiles to (a
    /// Force or a Modifier). The row's property is the recipe's own param id.
    Modifier,
}

/// What a row is to the performer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Exposure {
    /// A row on the item's card: a manifest param `<item id>.<property>`.
    Card(CardFacts),
    /// Set by the compiler; the text says from what.
    Automatic(&'static str),
    /// Left at its default; the text says why no one reaches it.
    Internal(&'static str),
}

/// Where a card row sits and how it edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardFacts {
    pub role: SurfaceRole,
    pub band: Option<CardBand>,
    pub order: u8,
    pub label: CardLabel,
    pub gate: Option<Gate>,
    pub switch: Option<Feature>,
    pub colour: Option<RgbChannel>,
    pub unit: Option<Unit>,
    pub limit: HardLimit,
    /// Draw the node param as an on/off switch (a float 0 or 1).
    pub toggle: bool,
    /// The slider range where the node param's own is not the right one.
    pub range: Option<(f32, f32)>,
}

/// One (kind, property) row.
#[derive(Debug, Clone, Copy)]
pub struct PropertyRow {
    pub property: &'static str,
    pub reach: Reach,
    /// The node param the row binds.
    pub param: &'static str,
    pub section: ControlGroup,
    pub exposure: Exposure,
    /// The kind's own default, where it differs from the node's.
    pub default: Option<f32>,
    /// The card name, where the node param's label does not name it.
    pub name: Option<&'static str>,
    /// Whether the row exists for an item with these fields.
    pub applies: fn(&ItemFields) -> bool,
}

impl CardFacts {
    /// The scene placement the param surface reads.
    pub fn desc(&self, section: ControlGroup) -> SceneParamDesc {
        SceneParamDesc {
            role: self.role,
            group: section,
            band: self.band,
            card_order: self.order,
            card_label: self.label,
            gate: self.gate,
            switch: self.switch,
            colour: self.colour,
        }
    }
}

fn always(_: &ItemFields) -> bool {
    true
}

const fn facts(role: SurfaceRole) -> CardFacts {
    CardFacts {
        role,
        band: None,
        order: 0,
        label: CardLabel::Param,
        gate: None,
        switch: None,
        colour: None,
        unit: None,
        limit: HardLimit::Slider,
        toggle: false,
        range: None,
    }
}

/// A row builder, so each kind's file reads as a table.
const fn row(property: &'static str, reach: Reach, section: ControlGroup, exposure: Exposure) -> PropertyRow {
    PropertyRow { property, reach, param: property, section, exposure, default: None, name: None, applies: always }
}

/// A card row set up in the Scene tab.
const fn setup() -> Exposure {
    Exposure::Card(facts(SurfaceRole::Setup))
}

/// A performance row in `band`, at `order` within its item.
const fn perform(band: CardBand, order: u8) -> Exposure {
    let mut f = facts(SurfaceRole::Performance);
    f.band = Some(band);
    f.order = order;
    Exposure::Card(f)
}

impl PropertyRow {
    const fn param(mut self, param: &'static str) -> Self {
        self.param = param;
        self
    }

    const fn default(mut self, value: f32) -> Self {
        self.default = Some(value);
        self
    }

    const fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    const fn when(mut self, applies: fn(&ItemFields) -> bool) -> Self {
        self.applies = applies;
        self
    }

    const fn unit(mut self, unit: Unit) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.unit = Some(unit);
        }
        self
    }

    const fn limit(mut self, limit: HardLimit) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.limit = limit;
        }
        self
    }

    const fn label(mut self, label: CardLabel) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.label = label;
        }
        self
    }

    const fn gate(mut self, gate: Gate) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.gate = Some(gate);
        }
        self
    }

    const fn switch(mut self, feature: Feature) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.switch = Some(feature);
        }
        self
    }

    const fn toggle(mut self) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.toggle = true;
        }
        self
    }

    const fn range(mut self, min: f32, max: f32) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.range = Some((min, max));
        }
        self
    }

    const fn colour(mut self, channel: RgbChannel) -> Self {
        if let Exposure::Card(f) = &mut self.exposure {
            f.colour = Some(channel);
        }
        self
    }

    /// Whether this row is a card row.
    pub fn is_card(&self) -> bool {
        matches!(self.exposure, Exposure::Card(_))
    }
}

const AT_LEAST_ZERO: HardLimit = HardLimit::AtLeast(0.0);

/// Every row a kind declares, applicable or not.
pub fn rows(kind: ItemKind) -> &'static [PropertyRow] {
    match kind {
        ItemKind::Group => &GROUP_ROWS,
        ItemKind::Object => &OBJECT_ROWS,
        ItemKind::Camera => &CAMERA_ROWS,
        ItemKind::Light => &LIGHT_ROWS,
        ItemKind::Material => &MATERIAL_ROWS,
        ItemKind::World => &WORLD_ROWS,
        ItemKind::Force => &FORCE_ROWS,
        // Per type: [`modifier_rows`].
        ItemKind::Modifier => &[],
        ItemKind::Swarm => &SWARM_ROWS,
        ItemKind::Water => &WATER_ROWS,
        ItemKind::WaterSource => &WATER_SOURCE_ROWS,
    }
}

/// The Physics section's rows, on the kinds that have the section.
fn physics_rows(kind: ItemKind) -> &'static [PropertyRow] {
    match kind {
        ItemKind::Object | ItemKind::Group => &PHYSICS_ROWS,
        _ => &[],
    }
}

/// The Copies section's rows, on the kinds that have the section.
fn copies_rows(kind: ItemKind) -> &'static [PropertyRow] {
    match kind {
        ItemKind::Object | ItemKind::Group => &COPIES_ROWS,
        _ => &[],
    }
}

/// The Ocean surface's rows, on an Object (they apply while its Shape is Ocean).
fn ocean_rows(kind: ItemKind) -> impl Iterator<Item = &'static PropertyRow> {
    let tables: &'static [&'static [PropertyRow]] = if kind == ItemKind::Object { &OCEAN_TABLES } else { &[] };
    tables.iter().flat_map(|table| table.iter())
}

/// Every row a kind declares, applicable or not, including the sections
/// that only some of its items have (Ocean, the curve shapes).
pub fn declared_rows(kind: ItemKind) -> impl Iterator<Item = &'static PropertyRow> {
    let curves = (kind == ItemKind::Object).then(curve_rows).into_iter().flatten();
    rows(kind).iter().chain(ocean_rows(kind)).chain(curves)
}

/// The rows that exist for an item with `fields`; a Modifier's are its
/// type's.
pub fn applicable(fields: &ItemFields) -> impl Iterator<Item = &'static PropertyRow> + '_ {
    let kind = fields.kind();
    rows(kind)
        .iter()
        .chain(modifier_rows(fields))
        .chain(physics_rows(kind))
        .chain(copies_rows(kind))
        .chain(ocean_rows(kind))
        .chain(curves::curve_rows())
        .filter(move |row| (row.applies)(fields))
}

/// The row for `property` on an item with `fields`, if it exists there.
pub fn row_for(fields: &ItemFields, property: &str) -> Option<&'static PropertyRow> {
    applicable(fields).find(|row| row.property == property)
}
