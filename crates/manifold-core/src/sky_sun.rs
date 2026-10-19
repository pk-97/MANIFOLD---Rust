//! The sky's sun follows the scene's Sun light. `node.bake_environment`
//! paints its sun disc from the `node.light` wired into its `sun` input, so
//! the light is the one source of the sun's direction and colour. This
//! module picks that light and fills the wire for every path that builds or
//! edits a scene graph: glTF import, scene-light edits and project load.

use std::collections::BTreeSet;

use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};

pub const BAKE_ENVIRONMENT_TYPE_ID: &str = "node.bake_environment";
/// The bake's Light input the disc reads.
pub const SKY_SUN_PORT: &str = "sun";
const LIGHT_TYPE_ID: &str = "node.light";
const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
/// The bake's own sun position, from before the disc followed the light.
const WORLD_SUN_PARAMS: [&str; 3] = ["sun_x", "sun_y", "sun_z"];
const LIGHT_POS_PARAMS: [&str; 3] = ["pos_x", "pos_y", "pos_z"];
const LIGHT_AIM_PARAMS: [&str; 3] = ["aim_x", "aim_y", "aim_z"];
/// `node.light`'s position and target defaults (`light.rs`), for a saved
/// light that carries neither.
pub const LIGHT_DEFAULT_POS: [f32; 3] = [0.0, 30.0, 0.0];
pub const LIGHT_DEFAULT_AIM: [f32; 3] = [0.0, 0.0, 0.0];

fn number(value: &SerializedParamValue) -> Option<f32> {
    match value {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        SerializedParamValue::Enum { value } => Some(*value as f32),
        _ => None,
    }
}

/// Whether `node` is a `node.light` in Sun mode. `mode` 0 is Sun, and is
/// also the default a light saved without `mode` gets.
pub fn is_sun_light(node: &EffectGraphNode) -> bool {
    node.type_id == LIGHT_TYPE_ID && node.params.get("mode").and_then(number).is_none_or(|m| m.round() == 0.0)
}

/// The scene's Sun light in one graph level: the Sun-mode light on the
/// lowest-numbered `light_k` input of the level's `node.render_scene`. A
/// light that feeds no renderer lights nothing, so it is not the scene's.
pub fn scene_sun_light(nodes: &[EffectGraphNode], wires: &[EffectGraphWire]) -> Option<u32> {
    let is_renderer = |id: u32| nodes.iter().any(|n| n.id == id && n.type_id == RENDER_SCENE_TYPE_ID);
    let is_sun = |id: u32| nodes.iter().any(|n| n.id == id && is_sun_light(n));
    wires
        .iter()
        .filter(|w| is_renderer(w.to_node) && is_sun(w.from_node))
        .filter_map(|w| Some((w.to_port.strip_prefix("light_")?.parse::<u32>().ok()?, w.from_node)))
        .min()
        .map(|(_, light)| light)
}

/// Wire the scene's Sun light into every environment bake in this level
/// whose `sun` input is unwired. A wired `sun` is left alone: it is either
/// already the Sun light or a light the user chose. Returns the number of
/// wires added.
pub fn wire_sky_sun(nodes: &[EffectGraphNode], wires: &mut Vec<EffectGraphWire>) -> usize {
    let Some(sun) = scene_sun_light(nodes, wires) else { return 0 };
    let mut added = 0;
    for bake in nodes.iter().filter(|n| n.type_id == BAKE_ENVIRONMENT_TYPE_ID) {
        if wires.iter().any(|w| w.to_node == bake.id && w.to_port == SKY_SUN_PORT) {
            continue;
        }
        wires.push(EffectGraphWire {
            from_node: sun,
            from_port: "out".to_string(),
            to_node: bake.id,
            to_port: SKY_SUN_PORT.to_string(),
        });
        added += 1;
    }
    added
}

/// What [`migrate_world_sun`] did to one graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldSunMigration {
    /// The graph carried a World sun position and lost it.
    pub changed: bool,
    /// The World sun pointed somewhere other than the Sun light; the light
    /// won.
    pub moved_off_light: bool,
    /// The World sun was placed but the scene has no Sun light, so the disc
    /// is now off.
    pub no_sun_light: bool,
}

/// Drop the World's own sun position (`sun_x`, `sun_y`, `sun_z` on every
/// `node.bake_environment`, with the cards and bindings that drove them) and
/// wire the scene's Sun light into the bake instead. Runs only on a graph
/// that still carries a World sun position, so a `sun` wire the user later
/// removes stays removed. `card_value` answers a card's saved value by id;
/// a card it does not know reads its binding default.
pub fn migrate_world_sun(def: &mut EffectGraphDef, card_value: &dyn Fn(&str) -> Option<f32>) -> WorldSunMigration {
    let mut bakes = BTreeSet::new();
    collect_bakes(&def.nodes, &mut bakes);
    let bindings = def.preset_metadata.as_ref().map(|m| m.bindings.as_slice()).unwrap_or_default();
    let targets_world_sun = |target: &BindingTarget| {
        matches!(target, BindingTarget::Node { node_id, param }
            if bakes.contains(node_id.as_str()) && WORLD_SUN_PARAMS.contains(&param.as_str()))
    };
    let stored = any_world_sun_param(&def.nodes);
    if !stored && !bindings.iter().any(|b| targets_world_sun(&b.target)) {
        return WorldSunMigration::default();
    }

    // The value a param had when saved: its card's value if a card drives
    // it, else the node's own value, else the primitive default.
    let effective = |node: &EffectGraphNode, param: &str, default: f32| {
        bindings
            .iter()
            .find(|b| matches!(&b.target, BindingTarget::Node { node_id, param: p } if *node_id == node.node_id && p == param))
            .map(|b| card_value(&b.id).unwrap_or(b.default_value))
            .or_else(|| node.params.get(param).and_then(number))
            .unwrap_or(default)
    };
    let mut report = WorldSunMigration { changed: true, ..WorldSunMigration::default() };
    let sun = scene_sun_light(&def.nodes, &def.wires).and_then(|id| def.nodes.iter().find(|n| n.id == id));
    for bake in def.nodes.iter().filter(|n| n.type_id == BAKE_ENVIRONMENT_TYPE_ID) {
        let world = WORLD_SUN_PARAMS.map(|p| effective(bake, p, 0.0));
        if length(world) < 1e-6 {
            continue;
        }
        match sun {
            None => report.no_sun_light = true,
            Some(light) => {
                let toward_sun = [0, 1, 2].map(|i| {
                    effective(light, LIGHT_POS_PARAMS[i], LIGHT_DEFAULT_POS[i])
                        - effective(light, LIGHT_AIM_PARAMS[i], LIGHT_DEFAULT_AIM[i])
                });
                if cosine(world, toward_sun) < 1.0 - 1e-4 {
                    report.moved_off_light = true;
                }
            }
        }
    }

    strip_world_sun(&mut def.nodes);
    if let Some(meta) = def.preset_metadata.as_mut() {
        let mut dropped = BTreeSet::new();
        meta.bindings.retain(|b| {
            let dead = targets_world_sun(&b.target);
            if dead {
                dropped.insert(b.id.clone());
            }
            !dead
        });
        // A card that still drives something else (the Sun light's own
        // position, in an imported scene) stays.
        dropped.retain(|id| !meta.bindings.iter().any(|b| &b.id == id));
        meta.params.retain(|p| !dropped.contains(&p.id));
        meta.param_aliases
            .retain(|a| !dropped.contains(&a.old) && a.new.as_ref().is_none_or(|id| !dropped.contains(id)));
        meta.value_aliases.retain(|a| !dropped.contains(&a.param_id));
    }
    wire_levels(&mut def.nodes, &mut def.wires);
    report
}

fn collect_bakes(nodes: &[EffectGraphNode], bakes: &mut BTreeSet<String>) {
    for node in nodes {
        if node.type_id == BAKE_ENVIRONMENT_TYPE_ID && !node.node_id.as_str().is_empty() {
            bakes.insert(node.node_id.to_string());
        }
        if let Some(group) = &node.group {
            collect_bakes(&group.nodes, bakes);
        }
    }
}

fn any_world_sun_param(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|n| {
        (n.type_id == BAKE_ENVIRONMENT_TYPE_ID && WORLD_SUN_PARAMS.iter().any(|p| n.params.contains_key(*p)))
            || n.group.as_ref().is_some_and(|g| any_world_sun_param(&g.nodes))
    })
}

fn strip_world_sun(nodes: &mut [EffectGraphNode]) {
    for node in nodes {
        if node.type_id == BAKE_ENVIRONMENT_TYPE_ID {
            for p in WORLD_SUN_PARAMS {
                node.params.remove(p);
                node.exposed_params.remove(p);
            }
        }
        if let Some(group) = node.group.as_mut() {
            strip_world_sun(&mut group.nodes);
        }
    }
}

fn wire_levels(nodes: &mut [EffectGraphNode], wires: &mut Vec<EffectGraphWire>) {
    wire_sky_sun(nodes, wires);
    for node in nodes {
        if let Some(group) = node.group.as_mut() {
            let group = &mut **group;
            wire_levels(&mut group.nodes, &mut group.wires);
        }
    }
}

fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn cosine(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (length(a), length(b));
    if la < 1e-6 || lb < 1e-6 {
        return -1.0;
    }
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)
}
