//! Light: a Sun, Point or Spot (`node.light`), or the Environment
//! (`node.bake_environment`, with `node.hdri_source` in HDRI mode). The type
//! is a field, so a row a type does not read does not exist for it.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, CardLabel, ControlGroup, RgbChannel, Unit};

use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{EnvironmentSource, ItemFields, LightType, SkyStyle};

const LIGHT: Reach = Reach::Own("light");
const ENV: Reach = Reach::Own("environment");
const HDRI: Reach = Reach::Own("hdri");
use ControlGroup::{Environment, Lens, Light, Shadows, ShadowsAdvanced};

fn light_type(fields: &ItemFields) -> Option<&LightType> {
    match fields {
        ItemFields::Light(l) => Some(&l.light),
        _ => None,
    }
}

fn lamp(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Sun | LightType::Point | LightType::Spot))
}

fn point_or_spot(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Point | LightType::Spot))
}

fn spot(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Spot))
}

fn environment(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Environment { .. }))
}

fn sky(fields: &ItemFields, style: SkyStyle) -> bool {
    matches!(light_type(fields), Some(LightType::Environment { source: EnvironmentSource::Sky { style: s } }) if *s == style)
}

fn gradient(fields: &ItemFields) -> bool {
    sky(fields, SkyStyle::Gradient)
}

fn softbox(fields: &ItemFields) -> bool {
    sky(fields, SkyStyle::Softbox)
}

fn hdri(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Environment { source: EnvironmentSource::Hdri { .. } }))
}

const fn metres(property: &'static str) -> PropertyRow {
    row(property, LIGHT, Light, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited).when(lamp)
}

const fn softbox_row(property: &'static str) -> PropertyRow {
    row(property, ENV, Environment, setup()).when(softbox)
}

pub static LIGHT_ROWS: [PropertyRow; 38] = [
    // Sun, Point, Spot. A new light starts where the compiler's lamp body
    // puts it (`scene_compile::light::lamp_params`).
    row("light_type", LIGHT, Light, Exposure::Automatic("the light's type")).param("mode").when(lamp),
    row("intensity", LIGHT, Light, perform(CardBand::Lighting, 2))
        .label(CardLabel::ItemName)
        .limit(AT_LEAST_ZERO)
        .when(lamp),
    metres("pos_x"),
    metres("pos_y"),
    metres("pos_z"),
    metres("aim_x"),
    metres("aim_y"),
    metres("aim_z"),
    row("range", LIGHT, Light, setup()).unit(Unit::Metres).limit(AT_LEAST_ZERO).when(lamp),
    row("light_size", LIGHT, Light, setup()).unit(Unit::Metres).limit(AT_LEAST_ZERO).when(lamp),
    row("color_r", LIGHT, Light, setup()).colour(RgbChannel::R).when(lamp),
    row("color_g", LIGHT, Light, setup()).colour(RgbChannel::G).when(lamp),
    row("color_b", LIGHT, Light, setup()).colour(RgbChannel::B).when(lamp),
    row("falloff", LIGHT, Light, setup()).when(point_or_spot),
    row("inner_cone_angle", LIGHT, Light, setup()).when(spot),
    row("outer_cone_angle", LIGHT, Light, setup()).when(spot),
    row("cast_shadows", LIGHT, Shadows, setup()).when(lamp),
    row("shadow_softness", LIGHT, Shadows, setup()).when(lamp),
    row("shadow_bias", LIGHT, ShadowsAdvanced, setup()).when(lamp),
    row("shadow_resolution", LIGHT, ShadowsAdvanced, setup()).when(lamp),
    // Environment.
    row("exposure", Reach::EveryCameraLens, Lens, perform(CardBand::Lighting, 0))
        .param("exposure_ev")
        .unit(Unit::Ev)
        .limit(HardLimit::Unlimited)
        .when(environment),
    row("env_intensity", ENV, Environment, perform(CardBand::Lighting, 1))
        .param("intensity")
        .label(CardLabel::GroupName)
        .limit(AT_LEAST_ZERO)
        .when(environment),
    row("env_mode", ENV, Environment, Exposure::Automatic("the Environment source field")).param("mode").when(environment),
    row("bake_width", ENV, Environment, Exposure::Automatic("bake size is automatic")).param("width").when(environment),
    row("bake_height", ENV, Environment, Exposure::Automatic("bake size is automatic"))
        .param("height")
        .when(environment),
    row("horizon_strength", ENV, Environment, setup()).when(gradient),
    row("azimuth_variation", ENV, Environment, setup()).when(gradient),
    softbox_row("emitter_count"),
    softbox_row("emitter_intensity"),
    softbox_row("emitter_elevation"),
    softbox_row("emitter_width"),
    softbox_row("sun_disc_intensity"),
    softbox_row("sun_disc_size"),
    softbox_row("fill").default(0.3),
    row("white_furnace", ENV, Environment, Exposure::Internal("White Furnace is a lighting test, not a look"))
        .param("uniform")
        .when(environment),
    row("hdri_file", HDRI, Environment, Exposure::Automatic("the HDRI file field")).param("path").when(hdri),
    row("hdri_width", HDRI, Environment, Exposure::Internal("the file's own size")).param("width").when(hdri),
    row("hdri_height", HDRI, Environment, Exposure::Internal("the file's own size")).param("height").when(hdri),
];
