//! Light: a Sun, Point or Spot (`node.light`), or the Environment
//! (`node.bake_environment`, with `node.hdri_source` in HDRI mode). The type
//! is a field, so a row a type does not read does not exist for it.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, CardLabel, ControlGroup, Feature, Gate, Modes, RgbChannel, Unit};

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, enabled, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{EnvironmentSource, ItemFields, LightType, SkyStyle};

const LIGHT: Reach = Reach::Own("light");
const ENV: Reach = Reach::Own("environment");
const HDRI: Reach = Reach::Own("hdri");
// The picture behind the scene, in HDRI mode (SCENE_DOCUMENT_DESIGN.md
// section 8): the sky the HDRI makes for the active camera, blurred, dimmed
// or lifted, laid under the finished image when the Background switch is on.
const BG_BLUR: Reach = Reach::Own("bg_blur");
const BG_GAIN: Reach = Reach::Own("bg_gain");
const BG_SELECT: Reach = Reach::Own("bg_select");
/// The Background switch is a two-mode switch: off 0, on 1.
const BACKGROUND_ON: Gate = Gate::Only(Feature::Background, Modes(0b10));
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

pub static LIGHT_ROWS: [PropertyRow; 43] = [
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
    // Turns the HDRI about the up axis: its lighting, reflections and the
    // background behind the scene together.
    row("hdri_rotation", ENV, Environment, perform(CardBand::Lighting, 3))
        .param("rotation")
        .limit(HardLimit::Unlimited)
        .when(hdri),
    row("hdri_width", HDRI, Environment, Exposure::Internal("the file's own size")).param("width").when(hdri),
    row("hdri_height", HDRI, Environment, Exposure::Internal("the file's own size")).param("height").when(hdri),
    row("bg_enabled", BG_SELECT, Environment, setup()).param("selector").switch(Feature::Background).toggle().default(0.0).when(hdri),
    row("bg_blur", BG_BLUR, Environment, setup()).param("radius").range(0.0, 64.0).limit(AT_LEAST_ZERO).gate(BACKGROUND_ON).default(0.0).when(hdri),
    row("bg_brightness", BG_GAIN, Environment, setup()).param("gain").range(0.0, 4.0).limit(AT_LEAST_ZERO).gate(BACKGROUND_ON).default(1.0).when(hdri),
    row("bg_inputs", BG_SELECT, Environment, Exposure::Automatic("two pictures: the scene, and the scene over the sky")).param("num_inputs").when(hdri),
];

const LAMPS: &[Choice] = &[
    Choice { key: "sun", label: "Sun" },
    Choice { key: "point", label: "Point" },
    Choice { key: "spot", label: "Spot" },
];

const SOURCES: &[Choice] = &[
    Choice { key: "sky", label: "Sky" },
    Choice { key: "hdri", label: "HDRI File" },
    Choice { key: "none", label: "None" },
];

const SKIES: &[Choice] = &[Choice { key: "gradient", label: "Gradient" }, Choice { key: "softbox", label: "Softbox" }];

fn sky_selected(fields: &ItemFields) -> bool {
    matches!(light_type(fields), Some(LightType::Environment { source: EnvironmentSource::Sky { .. } }))
}

pub static LIGHT_FIELDS: [FieldRow; 4] = [
    FieldRow {
        id: "light_type",
        label: "Type",
        section: Light,
        control: FieldControl::Closed(LAMPS),
        applies: lamp,
        disabled: enabled,
        read: |fields| {
            FieldValue::Choice(
                match light_type(fields) {
                    Some(LightType::Point) => "point",
                    Some(LightType::Spot) => "spot",
                    _ => "sun",
                }
                .to_owned(),
            )
        },
        write: |fields, value| {
            let (ItemFields::Light(l), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("light_type", value));
            };
            l.light = match key.as_str() {
                "sun" => LightType::Sun,
                "point" => LightType::Point,
                "spot" => LightType::Spot,
                _ => return Err(wrong_value("light_type", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "environment_source",
        label: "Source",
        section: Environment,
        control: FieldControl::Closed(SOURCES),
        applies: environment,
        disabled: enabled,
        read: |fields| {
            FieldValue::Choice(
                match light_type(fields) {
                    Some(LightType::Environment { source: EnvironmentSource::Hdri { .. } }) => "hdri",
                    Some(LightType::Environment { source: EnvironmentSource::None }) => "none",
                    _ => "sky",
                }
                .to_owned(),
            )
        },
        write: |fields, value| {
            let (ItemFields::Light(crate::scene_doc::LightFields { light: LightType::Environment { source } }), FieldValue::Choice(key)) =
                (fields, value)
            else {
                return Err(wrong_value("environment_source", value));
            };
            *source = match (key.as_str(), &*source) {
                ("sky", EnvironmentSource::Sky { .. }) => source.clone(),
                ("sky", _) => EnvironmentSource::Sky { style: SkyStyle::default() },
                ("hdri", EnvironmentSource::Hdri { .. }) => source.clone(),
                ("hdri", _) => EnvironmentSource::Hdri { file: String::new() },
                ("none", _) => EnvironmentSource::None,
                _ => return Err(wrong_value("environment_source", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "sky_style",
        label: "Sky",
        section: Environment,
        control: FieldControl::Closed(SKIES),
        applies: sky_selected,
        disabled: enabled,
        read: |fields| {
            FieldValue::Choice(
                match light_type(fields) {
                    Some(LightType::Environment { source: EnvironmentSource::Sky { style: SkyStyle::Gradient } }) => "gradient",
                    _ => "softbox",
                }
                .to_owned(),
            )
        },
        write: |fields, value| {
            let (ItemFields::Light(crate::scene_doc::LightFields { light: LightType::Environment { source } }), FieldValue::Choice(key)) =
                (fields, value)
            else {
                return Err(wrong_value("sky_style", value));
            };
            let style = match key.as_str() {
                "gradient" => SkyStyle::Gradient,
                "softbox" => SkyStyle::Softbox,
                _ => return Err(wrong_value("sky_style", value)),
            };
            *source = EnvironmentSource::Sky { style };
            Ok(())
        },
    },
    // Always on the card in HDRI mode, even with no file picked yet.
    FieldRow {
        id: "hdri_file",
        label: "HDRI File",
        section: Environment,
        control: FieldControl::File { extensions: &["exr"] },
        applies: hdri,
        disabled: enabled,
        read: |fields| match light_type(fields) {
            Some(LightType::Environment { source: EnvironmentSource::Hdri { file } }) => FieldValue::File(file.clone()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::Light(crate::scene_doc::LightFields { light: LightType::Environment { source } }), FieldValue::File(path)) =
                (fields, value)
            else {
                return Err(wrong_value("hdri_file", value));
            };
            *source = EnvironmentSource::Hdri { file: path.clone() };
            Ok(())
        },
    },
];
