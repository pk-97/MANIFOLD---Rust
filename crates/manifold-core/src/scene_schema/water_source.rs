//! Water Source: a child of a Water with a role (Fill, Inflow, Drain) and a
//! shape. Its manifest rows arrive with the Water emitter (lane L7); the
//! role and shape are fields now, so the card can set them. Role and Shape
//! are open lists (section 7.18): mesh-shaped fills and absorb zones join
//! here later.

use manifold_foundation::scene_surface::ControlGroup;

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, always, enabled, wrong_value};
use crate::scene_doc::{ItemFields, SourceRole, SourceShape};

const ROLES: &[Choice] = &[
    Choice { key: "fill", label: "Fill" },
    Choice { key: "inflow", label: "Inflow" },
    Choice { key: "drain", label: "Drain" },
];

const SHAPES: &[Choice] = &[Choice { key: "box", label: "Box" }, Choice { key: "sphere", label: "Sphere" }];

pub static WATER_SOURCE_FIELDS: [FieldRow; 2] = [
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
            ItemFields::WaterSource(s) => {
                FieldValue::Choice(if s.shape == SourceShape::Sphere { "sphere" } else { "box" }.to_owned())
            }
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::WaterSource(s), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("shape", value));
            };
            s.shape = match key.as_str() {
                "box" => SourceShape::Box,
                "sphere" => SourceShape::Sphere,
                _ => return Err(wrong_value("shape", value)),
            };
            Ok(())
        },
    },
];
