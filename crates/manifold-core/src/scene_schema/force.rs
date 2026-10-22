//! Force: its type and its targets are fields (section 7.12). Its manifest
//! rows arrive with the Force emitter (lane L5), which also owns the reason a
//! target list that cannot resolve is disabled.

use manifold_foundation::scene_surface::ControlGroup;

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, wrong_value};
use crate::scene_doc::{ForceTargets, ForceType, ItemFields};

const TYPES: &[Choice] = &[
    Choice { key: "radial", label: "Radial" },
    Choice { key: "uniform", label: "Uniform" },
    Choice { key: "vortex", label: "Vortex" },
    Choice { key: "wind", label: "Wind" },
    Choice { key: "drag", label: "Drag" },
    Choice { key: "attractor", label: "Attractor" },
    Choice { key: "explosion", label: "Explosion" },
];

pub static FORCE_FIELDS: [FieldRow; 2] = [
    FieldRow {
        id: "force_type",
        label: "Type",
        section: ControlGroup::Force,
        control: FieldControl::Closed(TYPES),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Force(f) => FieldValue::Choice(
                match f.force {
                    ForceType::Radial => "radial",
                    ForceType::Uniform => "uniform",
                    ForceType::Vortex => "vortex",
                    ForceType::Wind => "wind",
                    ForceType::Drag => "drag",
                    ForceType::Attractor => "attractor",
                    ForceType::Explosion => "explosion",
                }
                .to_owned(),
            ),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::Force(f), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("force_type", value));
            };
            f.force = match key.as_str() {
                "radial" => ForceType::Radial,
                "uniform" => ForceType::Uniform,
                "vortex" => ForceType::Vortex,
                "wind" => ForceType::Wind,
                "drag" => ForceType::Drag,
                "attractor" => ForceType::Attractor,
                "explosion" => ForceType::Explosion,
                _ => return Err(wrong_value("force_type", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "targets",
        label: "Targets",
        section: ControlGroup::Force,
        control: FieldControl::ItemSet { items: is_object_or_group, all: "Everything with physics" },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Force(f) => FieldValue::Items(match &f.targets {
                ForceTargets::Physics => Vec::new(),
                ForceTargets::Items(ids) => ids.clone(),
            }),
            _ => FieldValue::Items(Vec::new()),
        },
        write: |fields, value| {
            let (ItemFields::Force(f), FieldValue::Item(pick)) = (fields, value) else {
                return Err(wrong_value("targets", value));
            };
            f.targets = match (pick, std::mem::take(&mut f.targets)) {
                (None, _) => ForceTargets::Physics,
                (Some(id), ForceTargets::Physics) => ForceTargets::Items(vec![id.clone()]),
                (Some(id), ForceTargets::Items(mut ids)) => {
                    match ids.iter().position(|held| held == id) {
                        Some(at) => {
                            ids.remove(at);
                        }
                        None => ids.push(id.clone()),
                    }
                    if ids.is_empty() { ForceTargets::Physics } else { ForceTargets::Items(ids) }
                }
            };
            Ok(())
        },
    },
];
