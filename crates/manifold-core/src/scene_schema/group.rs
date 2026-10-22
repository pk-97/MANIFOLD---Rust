//! Group: its transform (the frame everything inside it sits in) and its
//! Visible, which multiplies down the tree through each nested group.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::fields::{COPIES, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, PHYSICS, enabled, on_off, is_on, parse_on, wrong_value};
use super::{Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::ItemFields;

const T: Reach = Reach::Own("transform");
const VISIBLE: Reach = Reach::Own("visible");
const CHAIN: Reach = Reach::Own("visible_chain");
use ControlGroup::{Look, Transform};

pub static GROUP_ROWS: [PropertyRow; 14] = [
    row("pos_x", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_y", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_z", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("rot_x", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_y", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_z", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("scale_x", T, Transform, setup()),
    row("scale_y", T, Transform, setup()),
    row("scale_z", T, Transform, setup()),
    row("billboard", T, Transform, Exposure::Internal("only an Object turns to face the camera")),
    row("visible", VISIBLE, Look, setup()).param("value").default(1.0).name("Visible").limit(HardLimit::Between(0.0, 1.0)),
    row("chain_a", CHAIN, Look, Exposure::Automatic("this group's Visible")).param("a"),
    row("chain_b", CHAIN, Look, Exposure::Automatic("the enclosing group's visibility")).param("b"),
    row("chain_op", CHAIN, Look, Exposure::Automatic("Multiply, so a hidden outer group hides everything inside it"))
        .param("op"),
];

fn import(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Group(g) if g.source.is_some())
}

/// A Group's Source file is an import; its children come from the file (lane
/// L3 owns the swap that keeps the children whose path still exists).
pub static GROUP_FIELDS: [FieldRow; 5] = [
    FieldRow {
        id: "model_file",
        label: "Model File",
        section: ControlGroup::Source,
        control: FieldControl::File { extensions: &["glb", "gltf"] },
        applies: import,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Group(g) => FieldValue::File(g.source.clone().unwrap_or_default()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| match (fields, value) {
            (ItemFields::Group(g), FieldValue::File(path)) => {
                g.source = Some(path.clone());
                Ok(())
            }
            _ => Err(wrong_value("model_file", value)),
        },
    },
    FieldRow {
        id: "combine_meshes",
        label: "Combine Meshes",
        section: ControlGroup::Source,
        control: on_off(),
        applies: import,
        disabled: enabled,
        read: |fields| is_on(matches!(fields, ItemFields::Group(g) if g.combine_meshes)),
        write: |fields, value| {
            let on = parse_on("combine_meshes", value)?;
            match fields {
                ItemFields::Group(g) => g.combine_meshes = on,
                _ => return Err(wrong_value("combine_meshes", value)),
            }
            Ok(())
        },
    },
    MATERIAL_BINDING,
    PHYSICS,
    COPIES,
];
