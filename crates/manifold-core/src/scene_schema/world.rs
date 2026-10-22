//! World: physics (`node.physics_world`, emitted once an item has physics),
//! fog (`node.atmosphere`, when the fog field is on), the render mode
//! (`node.render_mode`), ambient occlusion (`node.ssao_gtao` and its blur,
//! when the ambient occlusion field is on) and the rest of the render
//! settings (the scene's `node.render_scene`).

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Feature, Gate, Modes, RgbChannel, Unit};

use super::fields::{FieldControl, FieldRow, FieldValue, enabled, is_camera, is_on, on_off, parse_on, always, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::ItemFields;

const PHYSICS: Reach = Reach::Own("physics");
const FOG: Reach = Reach::Own("fog");
const RENDER: Reach = Reach::Scene("render");
const MODE: Reach = Reach::Own("render_mode");
const AO: Reach = Reach::Own("ao");
const AO_BLUR_H: Reach = Reach::Own("ao_blur_h");
const AO_BLUR_V: Reach = Reach::Own("ao_blur_v");
const AO_MULTIPLY: Reach = Reach::Own("ao_multiply");
const AO_MASK: Reach = Reach::Own("ao_mask");
use ControlGroup::{AmbientOcclusion, Fog, PhysicsWorld, RayTracing, RenderMode, Shading};

/// The solver runs at 0 to 4 times real time. The row clamps to that, so a
/// mapped or typed Speed never reaches the solver out of range.
pub const SPEED_RANGE: (f32, f32) = (0.0, 4.0);

// The render modes that read each colour row (`node.render_mode`): Solid
// draws in the clay colour, Wireframe and Points in the line colour, and
// only Points has a size.
const SOLID: Gate = Gate::Only(Feature::RenderMode, Modes(0b0010));
const LINES: Gate = Gate::Only(Feature::RenderMode, Modes(0b1100));
const POINTS: Gate = Gate::Only(Feature::RenderMode, Modes(0b1000));

const fn gravity(property: &'static str) -> PropertyRow {
    row(property, PHYSICS, PhysicsWorld, setup()).unit(Unit::MetresPerSecondSquared).limit(HardLimit::Unlimited)
}

const fn fog(property: &'static str) -> PropertyRow {
    row(property, FOG, Fog, setup())
}

const fn ray_tracing(property: &'static str) -> PropertyRow {
    row(property, RENDER, RayTracing, setup())
}

const fn look(property: &'static str, reach: Reach, section: ControlGroup, order: u8) -> PropertyRow {
    row(property, reach, section, perform(CardBand::Look, order))
}

pub static WORLD_ROWS: [PropertyRow; 51] = [
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
    look("fog_density", FOG, Fog, 0).default(0.02).limit(AT_LEAST_ZERO),
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
    // Render mode, and the colours each mode draws in.
    row("render_mode", MODE, RenderMode, setup()).param("mode").switch(Feature::RenderMode),
    look("clay_color_r", MODE, Shading, 10).colour(RgbChannel::R).gate(SOLID),
    look("clay_color_g", MODE, Shading, 11).colour(RgbChannel::G).gate(SOLID),
    look("clay_color_b", MODE, Shading, 12).colour(RgbChannel::B).gate(SOLID),
    look("line_color_r", MODE, Shading, 13).colour(RgbChannel::R).gate(LINES),
    look("line_color_g", MODE, Shading, 14).colour(RgbChannel::G).gate(LINES),
    look("line_color_b", MODE, Shading, 15).colour(RgbChannel::B).gate(LINES),
    look("line_brightness", MODE, Shading, 16).gate(LINES),
    look("point_size", MODE, Shading, 17).unit(Unit::Pixels).gate(POINTS),
    row("scene_render_mode", RENDER, RenderMode, Exposure::Internal("the World's render mode node drives the render"))
        .param("render_mode"),
    // Ambient occlusion: contact shadows from the depth buffer, denoised by
    // a depth-aware blur and multiplied onto the picture where the surface
    // takes it (`ao_mask`: lit raster surfaces, never under ray tracing).
    look("ao_strength", AO, AmbientOcclusion, 20).param("intensity").name("Strength").limit(AT_LEAST_ZERO),
    row("ao_distance", AO, AmbientOcclusion, setup()).param("radius").name("Distance").unit(Unit::Metres).limit(HardLimit::AtLeast(0.01)),
    row("ao_slices", AO, AmbientOcclusion, Exposure::Internal("the sampling quality main shipped")).param("slices"),
    row("ao_steps", AO, AmbientOcclusion, Exposure::Internal("the sampling quality main shipped")).param("steps"),
    row("ao_projection", AO, AmbientOcclusion, Exposure::Internal("a scene reads its depth through the camera, never as a height field")).param("projection"),
    row("ao_relief", AO, AmbientOcclusion, Exposure::Internal("read only by the height field projection")).param("relief"),
    row("ao_blur_h_axis", AO_BLUR_H, AmbientOcclusion, Exposure::Automatic("the horizontal pass")).param("axis"),
    row("ao_blur_h_sigma", AO_BLUR_H, AmbientOcclusion, Exposure::Internal("the denoise main shipped")).param("depth_sigma"),
    row("ao_blur_v_axis", AO_BLUR_V, AmbientOcclusion, Exposure::Automatic("the vertical pass")).param("axis"),
    row("ao_blur_v_sigma", AO_BLUR_V, AmbientOcclusion, Exposure::Internal("the denoise main shipped")).param("depth_sigma"),
    row("ao_multiply_amount", AO_MULTIPLY, AmbientOcclusion, Exposure::Automatic("the whole occlusion")).param("amount"),
    row("ao_multiply_mode", AO_MULTIPLY, AmbientOcclusion, Exposure::Automatic("multiplied onto the picture")).param("mode"),
    row("ao_mask_amount", AO_MASK, AmbientOcclusion, Exposure::Internal("the render's ao mask decides where it lands")).param("amount"),
    // Render.
    ray_tracing("rt_enabled"),
    ray_tracing("temporal_upscale"),
    ray_tracing("rt_reflections"),
    ray_tracing("rt_shadows"),
    ray_tracing("rt_ao"),
    ray_tracing("rt_gi"),
    row("rt_firefly_clamp", RENDER, RayTracing, Exposure::Internal("always on; it only guards against fireflies")),
    row("rt_denoise_feed", RENDER, RayTracing, Exposure::Internal("a debug view of the denoiser input")),
];

pub static WORLD_FIELDS: [FieldRow; 3] = [
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
        id: "ambient_occlusion",
        label: "Ambient Occlusion",
        section: AmbientOcclusion,
        control: on_off(),
        applies: always,
        disabled: enabled,
        read: |fields| is_on(matches!(fields, ItemFields::World(w) if w.ambient_occlusion)),
        write: |fields, value| {
            let on = parse_on("ambient_occlusion", value)?;
            match fields {
                ItemFields::World(w) => w.ambient_occlusion = on,
                _ => return Err(wrong_value("ambient_occlusion", value)),
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
