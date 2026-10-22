//! Light: a Sun, Point or Spot is one `node.light`; the Environment is the
//! baked environment map (with its HDRI file in HDRI mode) and owns the
//! scene's Exposure on every camera lens. The first Sun lights the softbox
//! sky's sun disc. A scene takes one Environment: a second compiles to
//! nothing.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphNode, SerializedParamValue};
use manifold_core::scene_doc::{EnvironmentSource, Item, ItemFields, LightType, SkyStyle};

use super::builder::Builder;
use crate::node_graph::scene_graph::{float, plain_node};

/// A new lamp: about 45 degrees up, white, casting shadows. `mode` is the
/// light type (`node.light`'s Sun 0, Point 1, Spot 2).
pub(crate) fn lamp_params(mode: u32) -> BTreeMap<String, SerializedParamValue> {
    BTreeMap::from([
        ("mode".to_owned(), SerializedParamValue::Enum { value: mode }),
        ("pos_x".to_owned(), float(0.0)),
        ("pos_y".to_owned(), float(7.0)),
        ("pos_z".to_owned(), float(7.0)),
        ("color_r".to_owned(), float(1.0)),
        ("color_g".to_owned(), float(1.0)),
        ("color_b".to_owned(), float(1.0)),
        ("intensity".to_owned(), float(1.0)),
        ("cast_shadows".to_owned(), float(1.0)),
    ])
}

/// A new Sun light node, as the old Add Light inserted it.
pub(crate) fn lamp_node(id: u32, handle: &str) -> EffectGraphNode {
    let mut light = plain_node(id, "light", "node.light", handle);
    light.params = lamp_params(0);
    light
}

/// Emit every light in tree order.
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
        b.lights.push(light);
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
    // `node.bake_environment`'s modes: Gradient 0, Softbox 1, HDRI 2.
    let mode = match source {
        EnvironmentSource::None => {
            b.rows(item);
            return;
        }
        EnvironmentSource::Sky { style: SkyStyle::Gradient } => 0,
        EnvironmentSource::Sky { style: SkyStyle::Softbox } => 1,
        EnvironmentSource::Hdri { .. } => 2,
    };
    let environment = b.add(owner, "environment", "node.bake_environment");
    b.set(environment, "mode", SerializedParamValue::Enum { value: mode });
    if let EnvironmentSource::Hdri { file } = source {
        let hdri = b.add(owner, "hdri", "node.hdri_source");
        b.set(hdri, "path", SerializedParamValue::String { value: file.clone() });
        b.wire(hdri, "out", environment, "hdri");
    }
    if let Some(sun) = sun {
        b.wire(sun, "out", environment, "sun");
    }
    let render = b.scene("render");
    b.wire(environment, "envmap", render, "envmap");
    b.rows(item);
}
