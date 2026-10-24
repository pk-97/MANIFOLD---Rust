//! Force: the rows of each of the five types, from the force recipes
//! (`scene-modifier-presets`). A force compiles to one scene modifier
//! instance, so a row reaches a param of that instance's recipe: the row's
//! property is the recipe's own param id. Drag is the Wind recipe with its
//! speed and gusts held at zero, which is still-air drag.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ForceTargets, ForceType, Item, ItemFields, ItemId, ItemKind, SceneDoc};

const F: ControlGroup = ControlGroup::Force;
const FORCES: CardBand = CardBand::Forces;

fn is(fields: &ItemFields, force: ForceType) -> bool {
    matches!(fields, ItemFields::Force(f) if f.force == force)
}

fn radial(fields: &ItemFields) -> bool {
    is(fields, ForceType::Radial)
}
fn uniform(fields: &ItemFields) -> bool {
    is(fields, ForceType::Uniform)
}
fn vortex(fields: &ItemFields) -> bool {
    is(fields, ForceType::Vortex)
}
fn wind(fields: &ItemFields) -> bool {
    is(fields, ForceType::Wind)
}
fn drag(fields: &ItemFields) -> bool {
    is(fields, ForceType::Drag)
}

const fn r(
    property: &'static str,
    exposure: Exposure,
    when: fn(&ItemFields) -> bool,
) -> PropertyRow {
    row(property, Reach::Modifier, F, exposure).when(when)
}

const fn on_off(when: fn(&ItemFields) -> bool) -> PropertyRow {
    r("enabled", perform(FORCES, 0), when).name("On")
}

const fn centre(property: &'static str, when: fn(&ItemFields) -> bool) -> PropertyRow {
    r(property, setup(), when)
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
}

const fn strength(when: fn(&ItemFields) -> bool) -> PropertyRow {
    r("strength", perform(FORCES, 1), when).unit(Unit::MetresPerSecondSquared)
}

const fn impulse_size(order: u8, when: fn(&ItemFields) -> bool) -> PropertyRow {
    r("impulse_strength", perform(FORCES, order), when).unit(Unit::MetresPerSecond)
}

const fn impulse(when: fn(&ItemFields) -> bool) -> PropertyRow {
    r("fire", perform(FORCES, 2), when)
}

const fn radius(when: fn(&ItemFields) -> bool) -> PropertyRow {
    r("radius", setup(), when).unit(Unit::Metres)
}

const fn drag_held(property: &'static str, why: &'static str, value: f32) -> PropertyRow {
    r(property, Exposure::Internal(why), drag).default(value)
}

const NO_WIND: &str = "Drag is still air: it has no wind to aim";

pub static FORCE_ROWS: [PropertyRow; 46] = [
    // Radial: pushes out from a centre, or pulls in at a negative Strength.
    on_off(radial),
    centre("center_x", radial),
    centre("center_y", radial),
    centre("center_z", radial),
    radius(radial).tip("Full-strength reach in Inverse mode, or outer boundary in Edge mode"),
    r("falloff", setup(), radial)
        .tip("How steeply strength fades (0 holds full strength everywhere)"),
    r("curve", setup(), radial).tip("Edge stops at Radius; Inverse reaches past Radius"),
    strength(radial),
    impulse_size(3, radial),
    impulse(radial),
    // Uniform: one steady push along a vector.
    on_off(uniform),
    r("direction_x", setup(), uniform),
    r("direction_y", setup(), uniform),
    r("direction_z", setup(), uniform),
    strength(uniform),
    impulse_size(3, uniform),
    impulse(uniform),
    // Vortex: swirls around an axis through a centre.
    on_off(vortex),
    centre("center_x", vortex),
    centre("center_y", vortex),
    centre("center_z", vortex),
    r("axis_x", setup(), vortex),
    r("axis_y", setup(), vortex),
    r("axis_z", setup(), vortex),
    radius(vortex),
    r("falloff", setup(), vortex),
    r("curve", setup(), vortex),
    strength(vortex),
    impulse_size(3, vortex),
    impulse(vortex),
    // Wind: a steady stream with gusts; no impulse.
    on_off(wind),
    r("direction_x", setup(), wind),
    r("direction_y", setup(), wind),
    r("direction_z", setup(), wind),
    r("speed", perform(FORCES, 1), wind).unit(Unit::MetresPerSecond),
    r("turbulence", perform(FORCES, 2), wind),
    r("turbulence_size", setup(), wind).unit(Unit::Metres),
    r("air_resistance", perform(FORCES, 3), wind).unit(Unit::PerSecond),
    // Drag: still-air drag, the Wind recipe at speed zero.
    on_off(drag),
    drag_held("direction_x", NO_WIND, 1.0),
    drag_held("direction_y", NO_WIND, 0.0),
    drag_held("direction_z", NO_WIND, 0.0),
    drag_held("speed", "Drag is still air: its speed is zero", 0.0),
    drag_held("turbulence", "Drag is still air: it has no gusts", 0.0),
    drag_held("turbulence_size", "Drag is still air: it has no gusts", 2.0),
    r("air_resistance", perform(FORCES, 1), drag)
        .unit(Unit::PerSecond)
        .name("Drag"),
];

/// What a Force can be aimed at: Objects and Groups (the bodies at or under
/// them), and Swarms.
fn is_force_target(item: &Item) -> bool {
    is_object_or_group(item) || item.kind() == ItemKind::Swarm
}

/// Whether the scene holds an item of any of `kinds`.
fn holds(doc: &SceneDoc, kinds: &[ItemKind]) -> bool {
    let mut any = false;
    doc.walk(|item, _| any |= kinds.contains(&item.kind()));
    any
}

/// Whether `id` is a Swarm or holds one.
fn swarm_under(doc: &SceneDoc, id: &ItemId) -> bool {
    let mut any = false;
    doc.walk(|item, ancestors| {
        any |= item.kind() == ItemKind::Swarm
            && (&item.id == id || ancestors.iter().any(|holder| &holder.id == id));
    });
    any
}

/// Why a Force's Impulse cannot fire right now because of what the scene
/// holds, for the disabled control's hover text; `None` when it can. Whether
/// the force is switched on is a live param and is the card's to check.
pub fn impulse_unavailable(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let ItemFields::Force(force) = &item.fields else {
        return None;
    };
    if matches!(force.force, ForceType::Wind | ForceType::Drag) {
        return Some("This force has no impulse");
    }
    match &force.targets {
        ForceTargets::Physics
            if !doc.any_physics() && !holds(doc, &[ItemKind::Swarm, ItemKind::Water]) =>
        {
            Some("Nothing in the scene has physics, a swarm or water for this force to push")
        }
        ForceTargets::Items(ids) if ids.is_empty() => Some("Choose the items this force pushes"),
        ForceTargets::Items(ids)
            if !ids
                .iter()
                .any(|id| doc.physical_under(id) || swarm_under(doc, id)) =>
        {
            Some("None of the chosen items has physics or is a swarm")
        }
        _ => None,
    }
}

const TYPES: &[Choice] = &[
    Choice {
        key: "radial",
        label: "Radial",
    },
    Choice {
        key: "uniform",
        label: "Uniform",
    },
    Choice {
        key: "vortex",
        label: "Vortex",
    },
    Choice {
        key: "wind",
        label: "Wind",
    },
    Choice {
        key: "drag",
        label: "Drag",
    },
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
                _ => return Err(wrong_value("force_type", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "targets",
        label: "Targets",
        section: ControlGroup::Force,
        control: FieldControl::ItemSet {
            items: is_force_target,
            all: "Everything with physics",
            refused: |_, _, _| None,
        },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Force(f) => match &f.targets {
                ForceTargets::Physics => FieldValue::Everything,
                ForceTargets::Items(ids) => FieldValue::Items(ids.clone()),
            },
            _ => FieldValue::Everything,
        },
        write: |fields, value| {
            let ItemFields::Force(f) = fields else {
                return Err(wrong_value("targets", value));
            };
            f.targets = match value {
                FieldValue::Everything => ForceTargets::Physics,
                // Unchoosing the last item goes back to everything with physics.
                FieldValue::Items(ids) if ids.is_empty() => ForceTargets::Physics,
                FieldValue::Items(ids) => ForceTargets::Items(ids.clone()),
                _ => return Err(wrong_value("targets", value)),
            };
            Ok(())
        },
    },
];
