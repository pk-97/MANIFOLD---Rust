//! Swarm: Emit From and Draw As are fields (section 7.11). Its manifest rows
//! arrive with the Swarm emitter (lane L6).

use manifold_foundation::scene_surface::ControlGroup;

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, wrong_value};
use crate::scene_doc::{DrawAs, EmitFrom, ItemFields};

const EMITTERS: &[Choice] = &[Choice { key: "box", label: "Box volume" }, Choice { key: "sphere", label: "Sphere" }];

const SHAPES: &[Choice] = &[
    Choice { key: "tetrahedron", label: "Tetrahedron" },
    Choice { key: "cube", label: "Cube" },
    Choice { key: "octahedron", label: "Octahedron" },
    Choice { key: "icosahedron", label: "Icosahedron" },
    Choice { key: "sphere", label: "Sphere" },
];

pub static SWARM_FIELDS: [FieldRow; 2] = [
    FieldRow {
        id: "emit_from",
        label: "Emit From",
        section: ControlGroup::Source,
        control: FieldControl::ClosedOrItem { closed: EMITTERS, items: is_object_or_group },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Swarm(s) => match &s.emit_from {
                EmitFrom::Box => FieldValue::Choice("box".to_owned()),
                EmitFrom::Sphere => FieldValue::Choice("sphere".to_owned()),
                EmitFrom::Item(id) => FieldValue::Item(Some(id.clone())),
            },
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let ItemFields::Swarm(s) = fields else {
                return Err(wrong_value("emit_from", value));
            };
            s.emit_from = match value {
                FieldValue::Choice(key) if key == "box" => EmitFrom::Box,
                FieldValue::Choice(key) if key == "sphere" => EmitFrom::Sphere,
                FieldValue::Item(Some(id)) => EmitFrom::Item(id.clone()),
                other => return Err(wrong_value("emit_from", other)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "draw_as",
        label: "Draw As",
        section: ControlGroup::Source,
        control: FieldControl::ClosedOrItem { closed: SHAPES, items: is_object_or_group },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Swarm(s) => match &s.draw_as {
                DrawAs::Tetrahedron => FieldValue::Choice("tetrahedron".to_owned()),
                DrawAs::Cube => FieldValue::Choice("cube".to_owned()),
                DrawAs::Octahedron => FieldValue::Choice("octahedron".to_owned()),
                DrawAs::Icosahedron => FieldValue::Choice("icosahedron".to_owned()),
                DrawAs::Sphere => FieldValue::Choice("sphere".to_owned()),
                DrawAs::Item(id) => FieldValue::Item(Some(id.clone())),
            },
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let ItemFields::Swarm(s) = fields else {
                return Err(wrong_value("draw_as", value));
            };
            s.draw_as = match value {
                FieldValue::Choice(key) => match key.as_str() {
                    "tetrahedron" => DrawAs::Tetrahedron,
                    "cube" => DrawAs::Cube,
                    "octahedron" => DrawAs::Octahedron,
                    "icosahedron" => DrawAs::Icosahedron,
                    "sphere" => DrawAs::Sphere,
                    _ => return Err(wrong_value("draw_as", value)),
                },
                FieldValue::Item(Some(id)) => DrawAs::Item(id.clone()),
                other => return Err(wrong_value("draw_as", other)),
            };
            Ok(())
        },
    },
];
