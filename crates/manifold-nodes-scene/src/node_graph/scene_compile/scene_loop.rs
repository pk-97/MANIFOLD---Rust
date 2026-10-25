//! Scene Loop (SCENE_DOCUMENT_DESIGN.md ruling 21 and section 7.19
//! (Modifiers)): the scene tiled into an endless corridor every camera sees,
//! and the clock and corridor every camera on the Loop rig rides.
//!
//! The corridor is the stock `SceneLoop` recipe, one scene modifier instance
//! on every drawn object: its `node.scene_array` builds the cells around the
//! camera the render uses, so a cut to any camera keeps the tiling
//! The Loop cameras are the
//! camera emitter's (`camera.rs`); this card's Beats, Pattern, Stride, Home,
//! Near and Far drive all of them, so camera travel per loop is always a
//! whole number of patterns and the wrap is pure (corridor D3).
//!
//! One cell length serves the corridor and every Loop camera: the scene's
//! `node.loop_cell` measures the rest boxes of what the loop tiles (each
//! object at its transform, each copy group's layout, each swarm's spread)
//! along the corridor, live, and adds the card's Spacing as a gap. Dragging
//! a Transform, scale or Copies row moves the tiles on the same frame; a body
//! physics moves never does, since the boxes are where things start
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
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::effect_graph_def::{EffectGraphDef, ParamSpecDef};
use manifold_core::param_lookup::NodeParamMetadata;
use manifold_core::scene_doc::{CameraRig, ItemFields, ItemId, param_id};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::{
    self, COPIES_ROLE, Exposure, LOOP_CAMERA_ROWS, LOOP_GAP_ROW, PropertyRow, modifier_type,
};

use super::builder::{Builder, SCENE, card_spec, number, rest_box};
use super::camera::LOOP_BEATS;
use super::forces::{CardParam, bindings, node, recipe_card};
use crate::node_graph::primitives::loop_cell::LOOP_CELL_TYPE_ID;

/// The most objects one instance targets (`frames::selected_objects`).
const TARGETS_PER_INSTANCE: usize = 256;

/// The stock recipe that tiles an object drawing many copies.
const COPIES_RECIPE: &str = "SceneLoopCopies";

/// Spacing with nothing drawn to measure: the corridor's own cell.
const UNMEASURED_CELL: f32 = 10.0;

/// `InstanceSpace::World` param index.
const WORLD_SPACE: u32 = 2;

/// The corridor's starting scale, from the scene's rest-pose box as the
/// compiler sees it: Spacing starts at the scene's depth along the travel
/// axis (Z), so a cell starts at twice it, one depth of scene and one of open
/// air (the gap rule). The box holds every drawn
/// mesh and every swarm's spread ([`super::swarm::SPREAD`]). The live cell is
/// `node.loop_cell`'s; this seeds Spacing and the starts and ranges of Home,
/// Near and Far.
struct Corridor {
    cell: f32,
    gap: f32,
}

impl Corridor {
    fn measure(b: &Builder<'_>) -> Self {
        let swarms = b.swarms.iter().map(|_| &super::swarm::SPREAD);
        let bounds = rest_box(b.drawn.iter().map(|drawn| &drawn.rest).chain(swarms));
        let depth = bounds.map(|(lo, hi)| hi[2] - lo[2]);
        match depth.filter(|depth| *depth > 0.0) {
            Some(depth) => Self {
                cell: 2.0 * depth,
                gap: depth,
            },
            None => Self {
                cell: UNMEASURED_CELL,
                gap: UNMEASURED_CELL,
            },
        }
    }

    /// A Scene Loop row's start, default and slider range for `property`,
    /// where the scene's scale decides them.
    fn scaled(&self, property: &str) -> Option<(f32, (f32, f32))> {
        let c = self.cell;
        Some(match property {
            "spacing" => (self.gap, (0.0, 4.0 * c)),
            // The corridor entry: the camera starts half a cell back, in the gap.
            "home" => (-0.5 * c, (-2.0 * c, 2.0 * c)),
            _ => return None,
        })
    }
}

/// Emit the scene's Scene Loop, if it has one: the cell measure, the
/// corridor and its card.
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
    let (graph, specs) = recipe_card(entry.recipe, &lookup, &host_id, &[])?;

    // Spacing is the gap the cell adds on top of what it measures.
    let cell = emit_cell(b, item.id.as_str());
    let (property, param) = LOOP_GAP_ROW;
    let meta = b
        .param_meta(LOOP_CELL_TYPE_ID, param)
        .ok_or_else(|| format!("{LOOP_CELL_TYPE_ID} has no {param}"))?;
    let spec = outside_spec(&corridor, &host_id(property), lookup(property), &meta)?;
    b.declare(spec, &meta, &[cell], param);

    // Every Loop camera rides this card: (rig, phase) per camera.
    let cameras: Vec<(u32, u32)> = doc
        .cameras()
        .into_iter()
        .filter(|id| matches!(doc.find(id).map(|camera| &camera.fields), Some(ItemFields::Camera(c)) if c.rig == CameraRig::Loop))
        .filter_map(|id| Some((b.id(id.as_str(), "rig")?, b.id(id.as_str(), "phase")?)))
        .collect();
    for &(rig, _) in &cameras {
        b.wire(cell, "cell_size", rig, "cell_size");
    }
    for (property, role) in LOOP_CAMERA_ROWS {
        let targets: Vec<u32> = cameras
            .iter()
            .map(|&(rig, phase)| if role == "phase" { phase } else { rig })
            .collect();
        let type_id = if role == "phase" {
            "node.beat_ramp"
        } else {
            "node.loop_camera"
        };
        let meta = b
            .param_meta(type_id, property)
            .ok_or_else(|| format!("{type_id} has no {property}"))?;
        // A corridor param the cameras share keeps its recipe spec, so one
        // manifest param drives the corridor and every camera.
        let spec = match specs.iter().find(|(_, param)| param == property) {
            Some((spec, _)) => spec.clone(),
            None => outside_spec(&corridor, &host_id(property), lookup(property), &meta)?,
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

/// The scene's `node.loop_cell`, fed one rest box per object drawn once, copy
/// group and swarm. Returns the node.
fn emit_cell(b: &mut Builder<'_>, owner: &str) -> u32 {
    // (the box's node and port, the frame it sits in)
    let mut boxes: Vec<((u32, &'static str), Option<u32>)> = Vec::new();
    // An object drawn many times is measured by its group's box below.
    for drawn in b.drawn.clone() {
        if b.input(drawn.object, "instances").is_some() {
            continue;
        }
        let Some((placed, within)) = rest_frame(b, drawn.object) else {
            continue;
        };
        // The mesh's box under its transform: the unit cube at the box's
        // centre, scaled to its size.
        let shape = b.add_named(
            node(owner, &format!("box:{}", drawn.item)),
            "node.transform_3d",
        );
        let (lo, hi) = (drawn.rest.lo, drawn.rest.hi);
        for (axis, (pos, scale)) in [
            ("pos_x", "scale_x"),
            ("pos_y", "scale_y"),
            ("pos_z", "scale_z"),
        ]
        .into_iter()
        .enumerate()
        {
            b.set_number(shape, pos, 0.5 * (lo[axis] + hi[axis]));
            b.set_number(shape, scale, hi[axis] - lo[axis]);
        }
        b.wire(placed, "transform", shape, "parent");
        boxes.push(((shape, "transform"), within));
    }
    // Each copy group's layout at rest. A group's own layout that a swarm
    // carries rides the swarm's spread instead, and copies on a surface sit
    // inside that surface's box.
    let mut copied = Vec::new();
    b.doc.walk(|item, _| {
        if item.fields.copies_surface().is_none() {
            copied.push(item.id.clone());
        }
    });
    for item in copied {
        // A swarm's copies are its particles, measured by its spread below.
        if b.swarms.contains(&item) {
            continue;
        }
        let Some(body) = b.id(item.as_str(), COPIES_ROLE) else {
            continue;
        };
        if b.nests.values().any(|layout| *layout == Some(body)) {
            continue;
        }
        boxes.push(((body, "rest_box"), None));
    }
    // Each swarm's lattice box, which its stirred cloud fills, in its
    // emitter's frame.
    for swarm in b.swarms.clone() {
        if let (Some(spread), Some(emitter)) = (
            b.id(swarm.as_str(), "bounds"),
            b.id(swarm.as_str(), "transform"),
        ) {
            boxes.push(((spread, "transform"), Some(emitter)));
        }
    }
    let cell = b.add(SCENE, "loop_cell", LOOP_CELL_TYPE_ID);
    b.set_number(cell, "boxes", boxes.len().max(1) as f32);
    for (k, ((from, port), within)) in boxes.into_iter().enumerate() {
        b.wire(from, port, cell, &format!("box_{k}"));
        if let Some(within) = within {
            b.wire(within, "transform", cell, &format!("within_{k}"));
        }
    }
    cell
}

/// Where `object` rests: the transform its mesh is placed by and the frame
/// that holds it, as the renderer composes them, but with a body's solved
/// pose read as the start transform the body takes. A body that draws only
/// at its pose rests at its start.
fn rest_frame(b: &Builder<'_>, object: u32) -> Option<(u32, Option<u32>)> {
    let at_rest = |frame: u32| match b.input(frame, "parent") {
        Some((body, port)) if port == "pose" => {
            b.input(body, "transform").map_or(frame, |(start, _)| start)
        }
        _ => frame,
    };
    let within = b
        .input(object, "parent_transform")
        .map(|(frame, _)| at_rest(frame));
    match b.input(object, "transform") {
        Some((placed, _)) => Some((placed, within)),
        None => within.map(|start| (start, None)),
    }
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
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    // The same card params as the corridor `emit` declared drive this one.
    let (graph, specs) = recipe_card(COPIES_RECIPE, &lookup, &host_id, &[])?;
    push_instances(b, item.id.as_str(), "loop_copies", &objects, &graph, &specs);
    Ok(())
}

/// One instance of `graph` per [`TARGETS_PER_INSTANCE`] objects, driven by
/// the card's params.
fn push_instances(
    b: &mut Builder<'_>,
    owner: &str,
    role: &str,
    objects: &[NodeId],
    graph: &EffectGraphDef,
    specs: &[CardParam],
) {
    for (chunk, part) in objects.chunks(TARGETS_PER_INSTANCE).enumerate() {
        let suffix = if chunk == 0 {
            role.to_owned()
        } else {
            format!("{role}:{chunk}")
        };
        let id = node(owner, &suffix);
        let instance = SceneModifierInstanceDef {
            id: id.clone(),
            scene: node(SCENE, "render"),
            targets: SceneTargetSelection::Explicit {
                objects: part.to_vec(),
            },
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(graph.clone()),
        };
        b.push_modifier(instance, Vec::new(), bindings(specs, &id), None);
    }
}

/// The card param of a Scene Loop row that reaches a node outside the
/// recipe: a Loop camera's, or the cell measure's gap.
fn outside_spec(
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
    let mut spec = card_spec(
        id.to_owned(),
        row.name.unwrap_or(&meta.label),
        meta,
        default,
        row.section,
        &facts,
    );
    (spec.min, spec.max) = range;
    Ok(spec)
}

/// The drawn objects the corridor tiles: those drawn once, or (`copies`)
/// those that draw many copies of an Object, Group or Swarm with a live
/// count. A Water's particle draws (foam, spray, bubbles, Physics View dots)
/// stay out: tiling a whole particle budget in every cell is not the loop.
fn tiled_objects(b: &mut Builder<'_>, copies: bool) -> Vec<NodeId> {
    let objects: Vec<(u32, ItemId)> = b
        .objects
        .iter()
        .copied()
        .zip(b.object_items.iter().cloned())
        .collect();
    let mut tiled = Vec::with_capacity(objects.len());
    for (object, item) in objects {
        if b.node_type(object) != Some("node.scene_object")
            || b.input(object, "instances").is_some() != copies
        {
            continue;
        }
        let water = matches!(
            b.doc.find(&item).map(|item| &item.fields),
            Some(ItemFields::Water(_))
        );
        // Without a live count the stage would nest the whole buffer.
        if copies && (water || b.input(object, "instance_count").is_none()) {
            continue;
        }
        if !copies {
            b.set(
                object,
                "instances_space",
                SerializedParamValue::Enum { value: WORLD_SPACE },
            );
        }
        if let Some(id) = b.node_id_of(object) {
            tiled.push(id);
        }
    }
    tiled
}
