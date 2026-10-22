//! World: physics (`node.physics_world`, emitted once an item has physics),
//! fog (`node.atmosphere`, when the fog field is on) and render settings
//! (the scene's `node.render_scene`).

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{FieldControl, FieldRow, FieldValue, enabled, is_camera, is_on, on_off, parse_on, always, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::ItemFields;

const PHYSICS: Reach = Reach::Own("physics");
const FOG: Reach = Reach::Own("fog");
const RENDER: Reach = Reach::Scene("render");
use ControlGroup::{Fog, PhysicsWorld, RayTracing, RenderMode};

/// The solver runs at 0 to 4 times real time. The row clamps to that, so a
/// mapped or typed Speed never reaches the solver out of range.
pub const SPEED_RANGE: (f32, f32) = (0.0, 4.0);

const fn gravity(property: &'static str) -> PropertyRow {
    row(property, PHYSICS, PhysicsWorld, setup()).unit(Unit::MetresPerSecondSquared).limit(HardLimit::Unlimited)
}

const fn fog(property: &'static str) -> PropertyRow {
    row(property, FOG, Fog, setup())
}

const fn ray_tracing(property: &'static str) -> PropertyRow {
    row(property, RENDER, RayTracing, setup())
}

pub static WORLD_ROWS: [PropertyRow; 29] = [
    gravity("gravity_x"),
    gravity("gravity_y").default(-9.81),
    gravity("gravity_z"),
    row("speed", PHYSICS, PhysicsWorld, setup()).limit(HardLimit::Between(SPEED_RANGE.0, SPEED_RANGE.1)),
    row("reset", PHYSICS, PhysicsWorld, perform(CardBand::Scene, 1)),
    row("air_resistance", PHYSICS, PhysicsWorld, setup()).unit(Unit::PerSecond).limit(AT_LEAST_ZERO),
    row("air_drag", PHYSICS, PhysicsWorld, Exposure::Internal("drag is each item's own; the World has Air Resistance")),
    row("bodies", PHYSICS, PhysicsWorld, Exposure::Automatic("the bodies in the scene")),
    row("copy_groups", PHYSICS, PhysicsWorld, Exposure::Automatic("the copy groups in the scene")),
    // Fog: a light haze when switched on.
    fog("fog_density").default(0.02).limit(AT_LEAST_ZERO),
    fog("height_falloff").limit(AT_LEAST_ZERO),
    fog("shaft_intensity").limit(AT_LEAST_ZERO),
    fog("fog_color_r"),
    fog("fog_color_g"),
    fog("fog_color_b"),
    fog("ambient_tint_r"),
    fog("ambient_tint_g"),
    fog("ambient_tint_b"),
    fog("shaft_anisotropy"),
    fog("shaft_quality"),
    // Render.
    row("render_mode", RENDER, RenderMode, setup()),
    ray_tracing("rt_enabled"),
    ray_tracing("temporal_upscale"),
    ray_tracing("rt_reflections"),
    ray_tracing("rt_shadows"),
    ray_tracing("rt_ao"),
    ray_tracing("rt_gi"),
    row("rt_firefly_clamp", RENDER, RayTracing, Exposure::Internal("always on; it only guards against fireflies")),
    row("rt_denoise_feed", RENDER, RayTracing, Exposure::Internal("a debug view of the denoiser input")),
];

pub static WORLD_FIELDS: [FieldRow; 2] = [
    FieldRow {
        id: "fog",
        label: "Fog",
        section: Fog,
        control: on_off(),
        applies: always,
        disabled: enabled,
        read: |fields| is_on(matches!(fields, ItemFields::World(w) if w.fog)),
        write: |fields, value| {
            let on = parse_on("fog", value)?;
            match fields {
                ItemFields::World(w) => w.fog = on,
                _ => return Err(wrong_value("fog", value)),
            }
            Ok(())
        },
    },
    FieldRow {
        id: "active_camera",
        label: "Default Camera",
        section: RenderMode,
        control: FieldControl::Item { items: is_camera, none: "First camera" },
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::World(w) => FieldValue::Item(w.active_camera.clone()),
            _ => FieldValue::Item(None),
        },
        write: |fields, value| match (fields, value) {
            (ItemFields::World(w), FieldValue::Item(camera)) => {
                w.active_camera = camera.clone();
                Ok(())
            }
            _ => Err(wrong_value("active_camera", value)),
        },
    },
];
