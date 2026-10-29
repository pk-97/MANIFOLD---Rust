//! Water Source: a child of a Water or Sand with a role (Fill, Inflow, Drain) and a
//! shape (Box, Sphere, Model). It compiles to one `node.fluid_role_source`
//! wired into its Water's domain. A Box or Sphere is placed by its Transform;
//! a Sphere draws its region from a `node.sphere_mesh` one metre across, the
//! size of the unit box. A Model takes a scene Object's mesh where the Object
//! stands, so it has no Transform of its own. Role is an open list (section
//! 7.18): absorb zones join here later.
//!
//! A Fill fills its shape once, and a Water or Sand takes one: the Fill
//! choice is disabled with its reason beside another Fill (section 7.10).
//! Sand only fills, so Inflow and Drain are disabled in a Sand.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, always, enabled, wrong_value};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::Shape;
use crate::scene_doc::{Item, ItemFields, SceneDoc, SourceRole, SourceShape};

const ROLES: &[Choice] = &[
    Choice {
        key: "fill",
        label: "Fill",
    },
    Choice {
        key: "inflow",
        label: "Inflow",
    },
    Choice {
        key: "drain",
        label: "Drain",
    },
];

const SHAPES: &[Choice] = &[
    Choice {
        key: "box",
        label: "Box",
    },
    Choice {
        key: "sphere",
        label: "Sphere",
    },
    Choice {
        key: "model",
        label: "Model",
    },
];

/// `node.fluid_role_source`'s Role values.
pub const FILL_ROLE: u32 = 0;
pub const INFLOW_ROLE: u32 = 1;
pub const DRAIN_ROLE: u32 = 2;

/// The `node.fluid_role_source` Role a source's role sets.
pub fn role_index(role: SourceRole) -> u32 {
    match role {
        SourceRole::Fill => FILL_ROLE,
        SourceRole::Inflow => INFLOW_ROLE,
        SourceRole::Drain => DRAIN_ROLE,
    }
}

const T: Reach = Reach::Own("transform");
const SOURCE: Reach = Reach::Own("role");
const SPHERE: Reach = Reach::Own("sphere");
use ControlGroup::{Source, Transform};

fn source(fields: &ItemFields) -> Option<&crate::scene_doc::WaterSourceFields> {
    match fields {
        ItemFields::WaterSource(s) => Some(s),
        _ => None,
    }
}

/// A Box or Sphere, which its Transform places; a Model stands where its
/// Object does.
fn placed(fields: &ItemFields) -> bool {
    source(fields).is_some_and(|s| s.shape != SourceShape::Model)
}

/// A placed source that turns: anything but a Fill's box, which fills
/// straight.
fn turns(fields: &ItemFields) -> bool {
    source(fields).is_some_and(|s| {
        s.shape == SourceShape::Sphere || s.role != SourceRole::Fill && s.shape == SourceShape::Box
    })
}

fn modelled(fields: &ItemFields) -> bool {
    source(fields).is_some_and(|s| s.shape == SourceShape::Model)
}

/// An Object with a mesh a source can take: anything but an Ocean or Terrain.
pub fn is_model(item: &Item) -> bool {
    matches!(&item.fields, ItemFields::Object(o) if !matches!(o.shape, Shape::Ocean | Shape::Terrain))
}

fn inflows(fields: &ItemFields) -> bool {
    source(fields).is_some_and(|s| s.role == SourceRole::Inflow)
}

fn sphere(fields: &ItemFields) -> bool {
    source(fields).is_some_and(|s| s.shape == SourceShape::Sphere)
}

const BOX_OR_SPHERE: &str = "a source is a box, a sphere or a scene Object's mesh";

const fn shaped(property: &'static str) -> PropertyRow {
    row(property, SOURCE, Source, Exposure::Internal(BOX_OR_SPHERE))
}

const fn inflow(property: &'static str, name: &'static str, order: u8) -> PropertyRow {
    row(property, SOURCE, Source, perform(CardBand::Scene, order))
        .name(name)
        .unit(Unit::MetresPerSecond)
        .limit(HardLimit::Unlimited)
        .when(inflows)
}

/// A source's size, metres: never zero, since the solver takes a source's
/// scale only when it is positive on every axis.
const SOURCE_SIZE: HardLimit = HardLimit::AtLeast(0.001);

pub static WATER_SOURCE_ROWS: [PropertyRow; 32] = [
    row("pos_x", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .when(placed),
    row("pos_y", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .when(placed),
    row("pos_z", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .when(placed),
    row("rot_x", T, Transform, setup())
        .limit(HardLimit::Unlimited)
        .when(turns),
    row("rot_y", T, Transform, setup())
        .limit(HardLimit::Unlimited)
        .when(turns),
    row("rot_z", T, Transform, setup())
        .limit(HardLimit::Unlimited)
        .when(turns),
    row("scale_x", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(SOURCE_SIZE)
        .when(placed),
    row("scale_y", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(SOURCE_SIZE)
        .when(placed),
    row("scale_z", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(SOURCE_SIZE)
        .when(placed),
    row(
        "billboard",
        T,
        Transform,
        Exposure::Internal("a source does not face the camera"),
    ),
    row(
        "role",
        SOURCE,
        Source,
        Exposure::Automatic("the Role field"),
    ),
    row(
        "enabled",
        SOURCE,
        Source,
        Exposure::Internal("a source acts while it is in the scene"),
    ),
    inflow("velocity_x", "Inflow Speed X", 0),
    inflow("velocity_y", "Inflow Speed Y", 1),
    inflow("velocity_z", "Inflow Speed Z", 2),
    row(
        "inherit_motion",
        SOURCE,
        Source,
        Exposure::Internal("a source's box does not move the water it adds"),
    ),
    row(
        "friction",
        SOURCE,
        Source,
        Exposure::Internal("friction is a solid's, and solids come from the World"),
    ),
    shaped("path"),
    shaped("compound_materials"),
    shaped("mesh_index"),
    shaped("primitive_index"),
    shaped("material_index"),
    shaped("fit"),
    shaped("recenter"),
    shaped("translate_x"),
    shaped("translate_y"),
    shaped("translate_z"),
    shaped("fragment_count"),
    shaped("fragment_index"),
    // A ball one metre across fills the unit box the Transform scales.
    row(
        "sphere_radius",
        SPHERE,
        Source,
        Exposure::Internal(BOX_OR_SPHERE),
    )
    .param("radius")
    .default(0.5)
    .when(sphere),
    // The solver samples the ball's region, not its triangles.
    row(
        "sphere_segments",
        SPHERE,
        Source,
        Exposure::Internal(BOX_OR_SPHERE),
    )
    .param("segments")
    .when(sphere),
    row(
        "sphere_rings",
        SPHERE,
        Source,
        Exposure::Internal(BOX_OR_SPHERE),
    )
    .param("rings")
    .when(sphere),
];

/// The Water or Sand `item` sits in.
fn water_of<'d>(doc: &'d SceneDoc, item: &Item) -> Option<&'d Item> {
    let (parent, _) = doc.locate(&item.id)?;
    doc.find(&parent?)
}

/// Why `item` cannot take the role or shape `key`, or `None`.
pub fn choice_unavailable(
    doc: &SceneDoc,
    item: &Item,
    field: &str,
    key: &str,
) -> Option<&'static str> {
    let fields = source(&item.fields)?;
    let sand =
        || water_of(doc, item).is_some_and(|parent| matches!(parent.fields, ItemFields::Sand(_)));
    match (field, key) {
        ("role", "fill") if fields.role != SourceRole::Fill => {
            let water = water_of(doc, item)?;
            let filled = water.children.iter().any(|child| {
                child.id != item.id && matches!(&child.fields, ItemFields::WaterSource(s) if s.role == SourceRole::Fill)
            });
            filled.then_some(crate::scene_doc::ONE_FILL)
        }
        ("role", "inflow" | "drain") if sand() => {
            Some("Sand fills only; Inflow and Drain are Water's")
        }
        _ => None,
    }
}

pub static WATER_SOURCE_FIELDS: [FieldRow; 3] = [
    FieldRow {
        id: "role",
        label: "Role",
        section: ControlGroup::Source,
        control: FieldControl::Closed(ROLES),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::WaterSource(s) => FieldValue::Choice(
                match s.role {
                    SourceRole::Fill => "fill",
                    SourceRole::Inflow => "inflow",
                    SourceRole::Drain => "drain",
                }
                .to_owned(),
            ),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::WaterSource(s), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("role", value));
            };
            s.role = match key.as_str() {
                "fill" => SourceRole::Fill,
                "inflow" => SourceRole::Inflow,
                "drain" => SourceRole::Drain,
                _ => return Err(wrong_value("role", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "shape",
        label: "Shape",
        section: ControlGroup::Source,
        control: FieldControl::Closed(SHAPES),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::WaterSource(s) => FieldValue::Choice(
                match s.shape {
                    SourceShape::Box => "box",
                    SourceShape::Sphere => "sphere",
                    SourceShape::Model => "model",
                }
                .to_owned(),
            ),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::WaterSource(s), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("shape", value));
            };
            s.shape = match key.as_str() {
                "box" => SourceShape::Box,
                "sphere" => SourceShape::Sphere,
                "model" => SourceShape::Model,
                _ => return Err(wrong_value("shape", value)),
            };
            Ok(())
        },
    },
    // The Object a Model takes its mesh and place from.
    FieldRow {
        id: "model",
        label: "Model",
        section: ControlGroup::Source,
        control: FieldControl::Item {
            items: is_model,
            none: "None",
        },
        applies: modelled,
        disabled: enabled,
        read: |fields| FieldValue::Item(source(fields).and_then(|s| s.model.clone())),
        write: |fields, value| {
            let (ItemFields::WaterSource(s), FieldValue::Item(model)) = (fields, value) else {
                return Err(wrong_value("model", value));
            };
            s.model.clone_from(model);
            Ok(())
        },
    },
];
