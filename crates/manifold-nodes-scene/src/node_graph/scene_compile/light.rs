//! Light: a Sun, Point or Spot is one `node.light`; the Environment is the
//! baked environment map (with its HDRI file in HDRI mode, under open sea in
//! Sea Horizon mode) and owns the
//! scene's Exposure on every camera lens. The first Sun in compile order
//! (item id order) lights the softbox sky's sun disc; the sky takes one Sun
//! (BUG-u3gco, sky sun disc from every Sun). Under the Daylight sky that Sun
//! reaches the render through the sky, tinted by the air it crosses; under
//! an HDRI it reaches it through the environment too, which can make it the
//! HDRI's own sun (Sun From HDRI). A scene takes one Environment: a second
//! compiles to nothing.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{EnvironmentSource, Item, ItemFields, LightType, SkyStyle};

use super::builder::Builder;
use crate::node_graph::scene_graph::float;

/// A Sun's starting intensity. Under the default Softbox sky (fill 0.3,
/// strips 1.5) it gives about 70% of the light on open ground from 45
/// degrees up, so it is the scene's main light and its shadows read.
/// Following an HDRI's sun, this intensity gives the sun as captured.
pub(crate) const SUN_INTENSITY: f32 = 6.0;

/// A new lamp: about 45 degrees up, white, casting shadows. `mode` is the
/// light type (`node.light`'s Sun 0, Point 1, Spot 2).
pub(crate) fn lamp_params(mode: u32) -> BTreeMap<String, SerializedParamValue> {
    BTreeMap::from([
        (
            "mode".to_owned(),
            SerializedParamValue::Enum { value: mode },
        ),
        ("pos_x".to_owned(), float(0.0)),
        ("pos_y".to_owned(), float(7.0)),
        ("pos_z".to_owned(), float(7.0)),
        ("color_r".to_owned(), float(1.0)),
        ("color_g".to_owned(), float(1.0)),
        ("color_b".to_owned(), float(1.0)),
        (
            "intensity".to_owned(),
            float(if mode == 0 { SUN_INTENSITY } else { 1.0 }),
        ),
        ("cast_shadows".to_owned(), float(1.0)),
    ])
}

/// Emit every light in compile order.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let mut lights = Vec::new();
    b.doc.walk(|item, _| {
        if let ItemFields::Light(_) = item.fields {
            lights.push(item.clone());
        }
    });
    let mut sun = None;
    let mut environment = None;
    for item in &lights {
        let ItemFields::Light(fields) = &item.fields else {
            continue;
        };
        let mode = match &fields.light {
            LightType::Sun => 0,
            LightType::Point => 1,
            LightType::Spot => 2,
            LightType::Environment { .. } => {
                if environment.is_none() {
                    environment = Some(item.clone());
                }
                continue;
            }
        };
        let light = b.add(item.id.as_str(), "light", "node.light");
        for (param, value) in lamp_params(mode) {
            b.set(light, &param, value);
        }
        b.lights.push((light, "out"));
        if mode == 0 && sun.is_none() {
            sun = Some(light);
        }
        b.rows(item);
    }
    if let Some(item) = environment {
        emit_environment(b, &item, sun);
    }
}

fn emit_environment(b: &mut Builder<'_>, item: &Item, sun: Option<u32>) {
    let ItemFields::Light(fields) = &item.fields else {
        return;
    };
    let LightType::Environment { source } = &fields.light else {
        return;
    };
    let owner = item.id.as_str();
    // `node.bake_environment`'s modes: Gradient 0, Softbox 1, HDRI 2,
    // Daylight 3.
    let mode = match source {
        EnvironmentSource::None => {
            b.rows(item);
            return;
        }
        EnvironmentSource::Sky {
            style: SkyStyle::Gradient,
        } => 0,
        EnvironmentSource::Sky {
            style: SkyStyle::Softbox,
        } => 1,
        EnvironmentSource::Sky {
            style: SkyStyle::Daylight,
        } => 3,
        EnvironmentSource::Hdri { .. } | EnvironmentSource::SeaHorizon { .. } => 2,
    };
    let environment = b.add(owner, "environment", "node.bake_environment");
    b.set(
        environment,
        "mode",
        SerializedParamValue::Enum { value: mode },
    );
    if let EnvironmentSource::Hdri { file } | EnvironmentSource::SeaHorizon { file } = source {
        let hdri = b.add(owner, "hdri", "node.hdri_source");
        b.set(
            hdri,
            "path",
            SerializedParamValue::String {
                value: file.clone(),
            },
        );
        b.wire(hdri, "sun", environment, "hdri_sun");
        if matches!(source, EnvironmentSource::SeaHorizon { .. }) {
            // The photo's lower half is whatever stood under its camera; the sea
            // replaces it before the bake, so lighting, reflections and the
            // background all see one horizon.
            let sea = b.add(owner, "sea", "node.sea_horizon_env");
            b.wire(hdri, "out", sea, "sky");
            b.wire(sea, "out", environment, "hdri");
        } else {
            b.wire(hdri, "out", environment, "hdri");
        }
        background(b, owner, environment);
    }
    if mode == 3 {
        background(b, owner, environment);
    }
    if let Some(sun) = sun {
        b.wire(sun, "out", environment, "sun");
        if (mode == 2 || mode == 3)
            && let Some(entry) = b.lights.iter_mut().find(|(light, _)| *light == sun)
        {
            *entry = (environment, "sun");
        }
    }
    // An HDRI lights the scene from `lighting`, which loses the sun when the
    // Sun follows it; the background keeps `envmap`, sun and all.
    let render = b.scene("render");
    let lighting = if mode == 2 { "lighting" } else { "envmap" };
    b.wire(environment, lighting, render, "envmap");
    b.rows(item);
}

/// The HDRI as a picture behind the scene, off by default: `bg_select` picks
/// the finished scene (0) or the scene over the sky (1), and the switch
/// prunes the sky chain while it is off. The sky is the baked environment,
/// so it turns with HDRI Rotation and follows Intensity as the lighting
/// does. The finished image becomes the select's output.
fn background(b: &mut Builder<'_>, owner: &str, environment: u32) {
    let (Some(tail), Some(lens)) = (b.tail, b.active_lens) else {
        return;
    };
    let sky = b.add(owner, "bg_sky", "node.camera_sky");
    let blur = b.add(owner, "bg_blur", "node.blur");
    let gain = b.add(owner, "bg_gain", "node.exposure");
    let over = b.add(owner, "bg_over", "node.over");
    let select = b.add(owner, "bg_select", "node.switch_texture");
    b.set(select, "num_inputs", SerializedParamValue::Int { value: 2 });
    b.set(
        select,
        "selector",
        SerializedParamValue::Float { value: 0.0 },
    );
    b.set(blur, "radius", SerializedParamValue::Float { value: 0.0 });
    b.wire(tail, "out", select, "in_0");
    b.wire(tail, "out", over, "top");
    b.wire(environment, "envmap", sky, "sky");
    b.wire(lens, "out", sky, "camera");
    b.wire(sky, "out", blur, "source");
    b.wire(blur, "out", gain, "in");
    b.wire(gain, "out", over, "bottom");
    b.wire(over, "out", select, "in_1");
    b.tail = Some(select);
}
