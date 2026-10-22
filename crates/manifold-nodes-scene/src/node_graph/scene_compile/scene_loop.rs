//! Scene Loop (SCENE_DOCUMENT_DESIGN.md ruling 21 and section 7.19
//! (Modifiers)): the scene tiled into an endless corridor every camera sees,
//! and the clock and corridor every camera on the Loop rig rides.
//!
//! The corridor is the stock `SceneLoop` recipe, one scene modifier instance
//! on every drawn object: its `node.scene_array` builds the cells around the
//! camera the render uses, so a cut to any camera keeps the tiling
//! (SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D1, D2). The Loop cameras are the
//! camera emitter's (`camera.rs`); this card's Beats, Pattern, Stride,
//! Spacing, Home, Near and Far drive all of them, so camera travel per loop
//! is always a whole number of patterns and the wrap is pure (corridor D3).
//!
//! For an object drawn once, the corridor is the first instance stage, before
//! the chain's own: it replaces the single pose with the cells, and every
//! later instance modifier (Spatial Echoes, Scene Mirror) builds on the cells.
//! An object that already draws many copies (Copies, a Swarm's particles)
//! takes the `SceneLoopCopies` recipe instead: `node.nest_copies` puts all its
//! live copies in every cell. That stage comes last in its chain, so the Copy
//! cards, Spatial Echoes and Scene Mirror shape one tile's copies and every
//! tile is the same; a Copy card after the tiling would vary the tiles and the
//! wrap would jump.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, ParamSpecDef};
use manifold_core::param_lookup::NodeParamMetadata;
use manifold_core::scene_doc::{CameraRig, ItemFields, param_id};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::{self, Exposure, LOOP_CAMERA_ROWS, PropertyRow, modifier_type};

use super::builder::{Builder, SCENE, card_spec, number, rest_box};
use super::camera::LOOP_BEATS;
use super::forces::{CardParam, bindings, node, recipe_card};

/// The most objects one instance targets (`frames::selected_objects`).
const TARGETS_PER_INSTANCE: usize = 256;

/// The stock recipe that tiles an object drawing many copies.
const COPIES_RECIPE: &str = "SceneLoopCopies";

/// A cell's size with nothing drawn to measure: the recipe's own.
const UNMEASURED_CELL: f32 = 10.0;

/// The corridor's scale, from the scene's rest-pose box: a cell is twice the
/// scene's depth along the travel axis (Z), one depth of scene and one of
/// open air (SCENE_LOOP_DESIGN.md D4, the gap rule). The box holds every
/// drawn mesh and every swarm's spread ([`super::swarm::SPREAD`]), so a
/// swarm's tiles start apart.
struct Corridor {
    cell: f32,
}

impl Corridor {
    fn measure(b: &Builder<'_>) -> Self {
        let swarms = b.swarms.iter().map(|_| &super::swarm::SPREAD);
        let depth = rest_box(b.drawn.iter().map(|drawn| &drawn.rest).chain(swarms)).map(|(lo, hi)| hi[2] - lo[2]);
        let cell = depth.filter(|depth| *depth > 0.0).map_or(UNMEASURED_CELL, |depth| 2.0 * depth);
        Self { cell }
    }

    /// A Loop camera's start, default and slider range for `property`, where
    /// the scene's scale decides them (SCENE_LOOP_DESIGN.md D11).
    fn scaled(&self, property: &str) -> Option<(f32, (f32, f32))> {
        let c = self.cell;
        Some(match property {
            "cell_size" => (c, (0.25 * c, 4.0 * c)),
            // The corridor entry: the camera starts half a cell back, in the gap.
            "home" => (-0.5 * c, (-2.0 * c, 2.0 * c)),
            "near" => ((0.002 * c).max(0.001), (0.001, 2.0 * c)),
            "far" => ((4.0 * c).max(1.0), (c.max(1.0), (20.0 * c).max(1.0))),
            _ => return None,
        })
    }
}

/// Emit the scene's Scene Loop, if it has one: the corridor and its card.
pub(crate) fn emit(b: &mut Builder<'_>) -> Result<(), String> {
    let doc = b.doc;
    let Some(item) = scene_schema::scene_loop(doc) else {
        return Ok(());
    };
    let ItemFields::Modifier(card) = &item.fields else {
        return Ok(());
    };
    let corridor = Corridor::measure(b);
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    let (graph, mut specs) = recipe_card(entry.recipe, &lookup, &host_id, &[("cell_size", corridor.cell)])?;
    for (spec, param) in &mut specs {
        if let Some((_, range)) = corridor.scaled(param) {
            (spec.min, spec.max) = range;
        }
    }

    // Every Loop camera rides this card: (rig, phase) per camera.
    let cameras: Vec<(u32, u32)> = doc
        .cameras()
        .into_iter()
        .filter(|id| matches!(doc.find(id).map(|camera| &camera.fields), Some(ItemFields::Camera(c)) if c.rig == CameraRig::Loop))
        .filter_map(|id| Some((b.id(id.as_str(), "rig")?, b.id(id.as_str(), "phase")?)))
        .collect();
    for (property, role) in LOOP_CAMERA_ROWS {
        let targets: Vec<u32> = cameras.iter().map(|&(rig, phase)| if role == "phase" { phase } else { rig }).collect();
        let type_id = if role == "phase" { "node.beat_ramp" } else { "node.loop_camera" };
        let meta = b.param_meta(type_id, property).ok_or_else(|| format!("{type_id} has no {property}"))?;
        // A corridor param the cameras share keeps its recipe spec, so one
        // manifest param drives the corridor and every camera.
        let spec = match specs.iter().find(|(_, param)| param == property) {
            Some((spec, _)) => spec.clone(),
            None => camera_spec(&corridor, &host_id(property), lookup(property), &meta)?,
        };
        b.declare(spec, &meta, &targets, property);
    }
    for (spec, param) in &specs {
        if !LOOP_CAMERA_ROWS.iter().any(|(shared, _)| shared == param) {
            b.push_params([spec.clone()]);
        }
    }

    if !card.on {
        return Ok(());
    }
    let objects = tiled_objects(b, false);
    push_instances(b, item.id.as_str(), "loop", &objects, &graph, &specs);
    Ok(())
}

/// Tile the objects that draw many copies, once every Modifier card has
/// shaped them (the module doc says why last): every live copy in every cell.
pub(crate) fn emit_copies(b: &mut Builder<'_>) -> Result<(), String> {
    let Some(item) = scene_schema::scene_loop(b.doc) else {
        return Ok(());
    };
    let ItemFields::Modifier(card) = &item.fields else {
        return Ok(());
    };
    let objects = tiled_objects(b, true);
    if !card.on || objects.is_empty() {
        return Ok(());
    }
    let corridor = Corridor::measure(b);
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    // The same card params as the corridor `emit` declared drive this one.
    let (graph, specs) = recipe_card(COPIES_RECIPE, &lookup, &host_id, &[("cell_size", corridor.cell)])?;
    push_instances(b, item.id.as_str(), "loop_copies", &objects, &graph, &specs);
    Ok(())
}

/// One instance of `graph` per [`TARGETS_PER_INSTANCE`] objects, driven by
/// the card's params.
fn push_instances(b: &mut Builder<'_>, owner: &str, role: &str, objects: &[NodeId], graph: &EffectGraphDef, specs: &[CardParam]) {
    for (chunk, part) in objects.chunks(TARGETS_PER_INSTANCE).enumerate() {
        let suffix = if chunk == 0 { role.to_owned() } else { format!("{role}:{chunk}") };
        let id = node(owner, &suffix);
        let instance = SceneModifierInstanceDef {
            id: id.clone(),
            scene: node(SCENE, "render"),
            targets: SceneTargetSelection::Explicit { objects: part.to_vec() },
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(graph.clone()),
        };
        b.push_modifier(instance, Vec::new(), bindings(specs, &id), None);
    }
}

/// The card param of a Scene Loop row that reaches the Loop cameras only.
fn camera_spec(
    corridor: &Corridor,
    id: &str,
    row: Option<&'static PropertyRow>,
    meta: &NodeParamMetadata,
) -> Result<ParamSpecDef, String> {
    let row = row.ok_or_else(|| format!("Scene Loop has no row for {id}"))?;
    let Exposure::Card(facts) = row.exposure else {
        return Err(format!("Scene Loop's {id} is not a card row"));
    };
    let (default, range) = match corridor.scaled(row.property) {
        Some(scaled) => scaled,
        // The ramp's "bars" counts beats; 0 would hand the loop back to its rate.
        None if row.property == "bars" => (LOOP_BEATS, (1.0, meta.max)),
        None => (number(&meta.default_value), (meta.min, meta.max)),
    };
    let mut spec = card_spec(id.to_owned(), row.name.unwrap_or(&meta.label), meta, default, row.section, &facts);
    (spec.min, spec.max) = range;
    Ok(spec)
}

/// The drawn objects the corridor tiles: those drawn once, or (`copies`)
/// those that already draw many copies.
fn tiled_objects(b: &mut Builder<'_>, copies: bool) -> Vec<NodeId> {
    let objects = b.objects.clone();
    let mut tiled = Vec::with_capacity(objects.len());
    for object in objects {
        if b.node_type(object) != Some("node.scene_object") || b.input(object, "instances").is_some() != copies {
            continue;
        }
        if let Some(id) = b.node_id_of(object) {
            tiled.push(id);
        }
    }
    tiled
}
