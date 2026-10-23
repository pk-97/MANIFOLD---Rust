//! Modifiers (SCENE_DOCUMENT_DESIGN.md section 7.19 (Modifiers)): the
//! scene's Modifier items are one chain, applied in tree order. Each card
//! compiles to scene modifier instances of its type's stock recipe, and the
//! `load/expand` passes build the rest at load, addressed by node id.
//!
//! A card's targets decide its frames (ruling 20): each Object target bends
//! in its own frame, each Group target bends all its parts in one shared
//! frame, and the whole scene bends as one group of everything. A frame is
//! the box of its parts' rest poses, each at its place in the frame: its own
//! position plus every group's between it and the frame's root, as the
//! compiler starts them (translation only). Every part's source offset is
//! its place from the box's centre, and the radius is half the box's
//! diagonal, so a recipe reads every shape the same way and a modifier's
//! scale follows its own target. In a Group's or the scene's frame each
//! part's offset is also measured live (`place`), from the part's transform
//! to the frame's rest centre, so the group bends as one piece wherever its
//! parts sit and as they move (Blender's deform about an origin object). An
//! imported part drawn in its own node's space is carried whole instead:
//! its mesh enters the frame through its full transform before the card's
//! stages and leaves after (`SceneMeshReferenceFrame::frame_transform`), so
//! the file's rotation and scale on its card bend with the group's axes and
//! units. A skinned, animated or morphing part bends its posed result. A Copy card
//! (Copy Wave, Jitter, Noise, Blend) takes the world its copies body lays
//! the copies out in, with no offset ([`copies_plan`]). One instance
//! per frame, all driven by the card's one set of manifest params,
//! `<card id>.<param>`.
//!
//! The scene-wide types have no targets and take the whole-scene frame.
//! Math View compiles here like any card; Scene Loop (`scene_loop.rs`) and
//! Scene Mirror (`scene_mirror.rs`) have their own emitters.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::physics_members::BODY_TYPE_ID;
use manifold_core::scene_doc::{
    Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, PhysicsSetting, SceneDoc, param_id,
};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::{acts_on_copies, modifier_type, shatter_parts};
use manifold_node_engine::load::expand::TargetPlacement;

use super::builder::{Builder, Drawn, FramePlan, SCENE};
use super::forces::{bindings, node, recipe_card};

/// The most objects one instance targets (`frames::selected_objects`); a
/// larger frame is split into instances that share it.
const TARGETS_PER_INSTANCE: usize = 256;

/// A frame's smallest radius, so a flat or empty box still measures.
const MIN_RADIUS: f64 = 1e-3;

/// A drawn mesh's own rest radius: half its rest box's diagonal, or the
/// smallest radius when the box does not measure.
fn own_radius(drawn: &Drawn) -> f64 {
    let radius = (0..3)
        .map(|axis| f64::from(drawn.rest.hi[axis]) - f64::from(drawn.rest.lo[axis]))
        .map(|size| size * size * 0.25)
        .sum::<f64>()
        .sqrt();
    if radius.is_finite() { radius.max(MIN_RADIUS) } else { MIN_RADIUS }
}

use manifold_foundation::mesh_partition::MAX_PIECES;

/// Emit every Modifier card, in chain order.
pub(crate) fn emit_all(b: &mut Builder<'_>) -> Result<(), String> {
    let doc = b.doc;
    let mut lineage: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    doc.walk(|item, ancestors| {
        lineage.insert(item.id.clone(), ancestors.iter().map(|above| above.id.clone()).collect());
    });

    for item in doc.items.iter().filter(|item| item.kind() == ItemKind::Modifier) {
        let ItemFields::Modifier(card) = &item.fields else {
            continue;
        };
        match card.modifier {
            // The corridor has its own emitter (`scene_loop.rs`).
            ModifierType::SceneLoop => {}
            ModifierType::SceneMirror => super::scene_mirror::emit(b, item, card)?,
            _ => emit(b, item, card, &lineage)?,
        }
    }
    Ok(())
}

/// The Objects a smoothly bending card that is on reaches, directly, through a
/// Group target, or as the whole scene.
pub(crate) fn smoothed_items(doc: &SceneDoc) -> BTreeSet<ItemId> {
    let cards: Vec<&ModifierTargets> = doc
        .items
        .iter()
        .filter_map(|item| match &item.fields {
            ItemFields::Modifier(card) if card.on && modifier_type(card.modifier).smooth_detail => Some(&card.targets),
            _ => None,
        })
        .collect();
    let mut reached = BTreeSet::new();
    if cards.is_empty() {
        return reached;
    }
    doc.walk(|item, ancestors| {
        if item.kind() != ItemKind::Object {
            return;
        }
        let hit = cards.iter().any(|targets| match targets {
            ModifierTargets::Scene => true,
            ModifierTargets::Items(ids) => {
                ids.contains(&item.id) || ancestors.iter().any(|above| ids.contains(&above.id))
            }
        });
        if hit {
            reached.insert(item.id.clone());
        }
    });
    reached
}

fn emit(
    b: &mut Builder<'_>,
    item: &Item,
    card: &ModifierFields,
    lineage: &BTreeMap<ItemId, Vec<ItemId>>,
) -> Result<(), String> {
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    let mut frames = if card.on { frames(b, card, lineage) } else { Vec::new() };
    let mut pieces = card.pieces.clamp(2, MAX_PIECES);
    if card.modifier == ModifierType::Shatter {
        for frame in &mut frames {
            frame.members = shatterable(b, &item.id, &frame.members, lineage);
            // Every material part of a body becomes at least one piece.
            for parts in bodies(b, &frame.members).values() {
                pieces = pieces.max(*parts as u32);
            }
        }
    }
    let held = [("fragments", pieces as f32)];
    let held: &[(&str, f32)] = if card.modifier == ModifierType::Shatter { &held } else { &[] };
    let (mut graph, specs) = recipe_card(entry.recipe, &lookup, &host_id, held)?;
    super::deformers::tune(&mut graph, card);
    // The card keeps its params with nothing to act on, so its values,
    // mappings and modulation survive a toggle or an emptied target list.
    b.push_params(specs.iter().map(|(spec, _)| spec.clone()));
    for frame in frames {
        if frame.members.is_empty() {
            continue;
        }
        let role = &frame.role;
        let mut plan = if acts_on_copies(card.modifier) {
            copies_plan(b, &frame)
        } else {
            plan(b, item, &frame, lineage, card.modifier == ModifierType::Shatter)
        };
        // A later swarm's copy of a drawn object bends with it, placed as it is.
        let mut targets = Vec::with_capacity(frame.members.len());
        for drawn in &frame.members {
            let original = b.node_id_of(drawn.object).ok_or("a modifier target has no node id")?;
            targets.push(original.clone());
            for copy in b.alike_of(drawn.object).collect::<Vec<_>>() {
                let copy = b.node_id_of(copy).ok_or("a modifier target has no node id")?;
                let placed: Vec<TargetPlacement> = plan
                    .placements
                    .iter()
                    .filter(|placement| placement.target == original)
                    .map(|placement| TargetPlacement { target: copy.clone(), ..placement.clone() })
                    .collect();
                plan.placements.extend(placed);
                targets.push(copy);
            }
        }
        for (chunk, part) in targets.chunks(TARGETS_PER_INSTANCE).enumerate() {
            let suffix = if chunk == 0 { role.clone() } else { format!("{role}:{chunk}") };
            let id = node(item.id.as_str(), &format!("mod:{suffix}"));
            let objects = part.to_vec();
            let placements = plan.placements.iter().filter(|placement| objects.contains(&placement.target)).cloned().collect();
            let instance = SceneModifierInstanceDef {
                id: id.clone(),
                scene: node(SCENE, "render"),
                targets: SceneTargetSelection::Explicit { objects },
                mesh_frames: Vec::new(),
                legacy_math_view_carrier: None,
                graph: Box::new(graph.clone()),
            };
            let frame = FramePlan { placements, radius: plan.radius };
            b.push_modifier(instance, Vec::new(), bindings(&specs, &id), Some(frame));
        }
    }
    Ok(())
}

/// What a frame belongs to.
enum Scope {
    /// One Object target: it bends in its own frame.
    Own,
    /// A Group target: its parts bend in its one frame.
    Group(ItemId),
    /// The whole scene, as one group of everything.
    Scene,
}

/// One frame of a card: its role in node ids, what it belongs to, its root
/// item (`None`: the whole scene), its parts.
struct CardFrame {
    role: String,
    scope: Scope,
    root: Option<ItemId>,
    members: Vec<Drawn>,
}

/// The card's frames, one per Object or Group target in the order they are
/// listed, or one for the whole scene. A part reached by two targets bends
/// once, in the first one's frame.
fn frames(b: &Builder<'_>, card: &ModifierFields, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Vec<CardFrame> {
    let scene = || vec![CardFrame { role: "scene".to_owned(), scope: Scope::Scene, root: None, members: b.drawn.clone() }];
    // A scene-wide card (Math View) always reads the whole scene.
    if card.modifier.scene_wide() {
        return scene();
    }
    match &card.targets {
        ModifierTargets::Scene => scene(),
        ModifierTargets::Items(ids) => {
            let mut claimed = BTreeSet::new();
            ids.iter()
                .map(|target| {
                    let members: Vec<Drawn> = b
                        .drawn
                        .iter()
                        .filter(|drawn| {
                            &drawn.item == target || lineage.get(&drawn.item).is_some_and(|above| above.contains(target))
                        })
                        .filter(|drawn| claimed.insert(drawn.object))
                        .cloned()
                        .collect();
                    let own = b.doc.find(target).is_some_and(|item| item.kind() == ItemKind::Object);
                    let scope = if own { Scope::Own } else { Scope::Group(target.clone()) };
                    CardFrame { role: target.to_string(), scope, root: Some(target.clone()), members }
                })
                .collect()
        }
    }
}

/// The body node a drawn object is posed by, if a rigid body poses it.
fn body_of(b: &Builder<'_>, drawn: &Drawn) -> Option<u32> {
    // Straight from the pose, or through the transforms it is placed in: the
    // body's pose frame, the groups inside a group body.
    let (mut from, mut port) = b.input(drawn.object, "parent_transform").or_else(|| b.input(drawn.object, "transform"))?;
    loop {
        match b.node_type(from) {
            Some(BODY_TYPE_ID) => return (port == "pose").then_some(from),
            Some("node.transform_3d") => (from, port) = b.input(from, "parent")?,
            _ => return None,
        }
    }
}

/// The parts of `members` the Shatter card `shatter` breaks: the parts
/// [`shatter_parts`] keeps of the Body around each, which is what the
/// Targets menu reads, posed by that body and drawing a mesh with a rest
/// pose (the load pass cuts the pieces).
fn shatterable(b: &Builder<'_>, shatter: &ItemId, members: &[Drawn], lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Vec<Drawn> {
    let doc = b.doc;
    let mut breaks: BTreeMap<&ItemId, Vec<ItemId>> = BTreeMap::new();
    members
        .iter()
        .filter(|drawn| {
            let Some(body) = body_item(doc, &drawn.item, lineage) else {
                return false;
            };
            let parts = breaks.entry(&body.id).or_insert_with(|| shatter_parts(doc, shatter, body).unwrap_or_default());
            parts.contains(&drawn.item) && body_of(b, drawn).is_some()
        })
        .cloned()
        .collect()
}

/// The Body item `item` belongs to: itself or the group around it whose
/// Physics is Body.
fn body_item<'d>(doc: &'d SceneDoc, item: &ItemId, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Option<&'d Item> {
    let above = lineage.get(item).map_or(&[][..], Vec::as_slice);
    std::iter::once(item)
        .chain(above.iter().rev())
        .filter_map(|id| doc.find(id))
        .find(|found| found.fields.physics() == PhysicsSetting::Body)
}

/// How many parts of `members` each body poses.
fn bodies(b: &Builder<'_>, members: &[Drawn]) -> BTreeMap<u32, usize> {
    let mut counts = BTreeMap::new();
    for drawn in members {
        if let Some(body) = body_of(b, drawn) {
            *counts.entry(body).or_insert(0) += 1;
        }
    }
    counts
}

/// Where `drawn` sits in the frame of `root` (the whole scene when `None`):
/// its own position plus every group's between it and the root, as the
/// compiler starts them. An Object target is its own frame, so it sits at
/// its own origin.
fn placement(b: &mut Builder<'_>, drawn: &Drawn, root: Option<&ItemId>, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> [f64; 3] {
    if root == Some(&drawn.item) {
        return [0.0; 3];
    }
    let ancestors = lineage.get(&drawn.item).map_or(&[][..], Vec::as_slice);
    let below = match root {
        None => ancestors,
        Some(root) => ancestors.iter().position(|above| above == root).map_or(&[][..], |at| &ancestors[at + 1..]),
    };
    let mut at = [0.0; 3];
    for item in below.iter().chain(std::iter::once(&drawn.item)) {
        let transform = b.id(item.as_str(), "transform");
        for (axis, param) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
            at[axis] += transform.and_then(|node| b.started(node, param)).map_or(0.0, f64::from);
        }
    }
    at
}

/// The import Group nearest above `item`: the Group whose Source is a file.
fn import_group<'l>(doc: &SceneDoc, item: &ItemId, lineage: &'l BTreeMap<ItemId, Vec<ItemId>>) -> Option<&'l ItemId> {
    lineage.get(item)?.iter().rev().find(|above| {
        doc.find(above).is_some_and(|found| matches!(&found.fields, ItemFields::Group(group) if group.source.is_some()))
    })
}

/// A frame's placements: each part's source offset from the frame's rest
/// centre, and the frame's radius. A part that shares a Group's or the
/// scene's frame also reads its offset live, so it bends wherever it sits
/// as it moves; one drawn in its own node's space is carried into the frame
/// whole. Shatter follows the live body pose instead, so its offset is
/// only its source's own shift.
fn plan(
    b: &mut Builder<'_>,
    card: &Item,
    frame: &CardFrame,
    lineage: &BTreeMap<ItemId, Vec<ItemId>>,
    shatter: bool,
) -> FramePlan {
    let members = &frame.members;
    let placed: Vec<[f64; 3]> = members.iter().map(|drawn| placement(b, drawn, frame.root.as_ref(), lineage)).collect();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (drawn, at) in members.iter().zip(&placed) {
        // An imported part's box where the file puts it is its box in its
        // import Group's frame, the scene's while that Group starts at rest:
        // its own Transform and the groups between start where the file
        // puts them, which the compiler does not see.
        let (part_lo, part_hi) = match drawn.rest.placed {
            Some(box_in_import) if frame.root.is_none() || frame.root.as_ref() == import_group(b.doc, &drawn.item, lineage) => {
                (box_in_import.0.map(f64::from), box_in_import.1.map(f64::from))
            }
            _ => (
                std::array::from_fn(|axis| at[axis] + f64::from(drawn.rest.lo[axis])),
                std::array::from_fn(|axis| at[axis] + f64::from(drawn.rest.hi[axis])),
            ),
        };
        for axis in 0..3 {
            lo[axis] = lo[axis].min(part_lo[axis]);
            hi[axis] = hi[axis].max(part_hi[axis]);
        }
    }
    if !(0..3).all(|axis| lo[axis].is_finite() && hi[axis].is_finite()) {
        lo = [0.0; 3];
        hi = [0.0; 3];
    }
    let centre: [f64; 3] = std::array::from_fn(|axis| (lo[axis] + hi[axis]) * 0.5);
    let radius = (0..3)
        .map(|axis| (hi[axis] - lo[axis]) * 0.5)
        .map(|half| half * half)
        .sum::<f64>()
        .sqrt()
        .max(MIN_RADIUS);
    let centre_node = match &frame.scope {
        _ if shatter => None,
        Scope::Own => None,
        Scope::Group(group) => {
            let held_in = group_frame(b, group, lineage);
            Some(frame_centre(b, card, &frame.role, centre, held_in))
        }
        Scope::Scene => Some(frame_centre(b, card, &frame.role, centre, None)),
    };
    let mut placements = Vec::with_capacity(members.len());
    for (drawn, at) in members.iter().zip(&placed) {
        let Some(target) = b.node_id_of(drawn.object) else {
            continue;
        };
        // A part drawn in its own node's space enters the shared frame
        // whole, rotation and scale included, so it bends in the frame's
        // axes and units as one body with the rest.
        if let Some(centre) = centre_node.filter(|_| drawn.rest.own_space) {
            let frame_transform = b.node_id_of(centre);
            placements.push(TargetPlacement { target, offset: [0.0; 3], live: None, radius: own_radius(drawn), frame_transform });
            continue;
        }
        let live = centre_node.and_then(|centre| place(b, card, &frame.role, drawn, centre));
        let offset = if shatter {
            let source = b.input(drawn.object, "vertices").map(|(source, _)| source);
            std::array::from_fn(|axis| {
                let param = ["translate_x", "translate_y", "translate_z"][axis];
                source.and_then(|source| b.started(source, param)).map_or(0.0, f64::from)
            })
        } else {
            std::array::from_fn(|axis| at[axis] - centre[axis])
        };
        placements.push(TargetPlacement { target, offset, live, radius: own_radius(drawn), frame_transform: None });
    }
    FramePlan { placements, radius }
}

/// A Copy card's frame: copy modifiers move whole copies through their
/// instances, which the copies body already lays out in the world, so a
/// target needs no offset. Copy Blend reads the body's own layout rows
/// through the copies context instead.
fn copies_plan(b: &mut Builder<'_>, frame: &CardFrame) -> FramePlan {
    let placements = frame.members.iter()
        .filter_map(|drawn| {
            Some(TargetPlacement { target: b.node_id_of(drawn.object)?, offset: [0.0; 3], live: None, radius: own_radius(drawn), frame_transform: None })
        })
        .collect();
    FramePlan { placements, radius: MIN_RADIUS }
}

/// The transform a Group's parts are placed in: its frame, or `None` at a
/// copies root, whose parts each sit inside one copy.
fn group_frame(b: &Builder<'_>, group: &ItemId, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Option<u32> {
    let copied = |id: &ItemId| b.swarm_draws.contains_key(id) || b.doc.find(id).is_some_and(|item| item.fields.copies());
    let above = lineage.get(group).map_or(&[][..], Vec::as_slice);
    if copied(group) && !above.iter().any(copied) {
        return None;
    }
    // Inside a copy a group records no world frame: its own transform is
    // its place in the copy.
    b.group_frames.get(group).copied().or_else(|| b.id(group.as_str(), "transform"))
}

/// The frame's rest centre as a transform, inside `held_in` (the world when
/// `None`): the place every part's live offset is measured from.
fn frame_centre(b: &mut Builder<'_>, card: &Item, role: &str, centre: [f64; 3], held_in: Option<u32>) -> u32 {
    let node = b.add_named(node(card.id.as_str(), &format!("centre:{role}")), "node.transform_3d");
    for (axis, param) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
        b.set(node, param, SerializedParamValue::Float { value: centre[axis] as f32 });
    }
    if let Some(frame) = held_in {
        b.wire(frame, "transform", node, "parent");
    }
    node
}

/// The node that measures where `drawn` sits from `centre`, live: its
/// transform inside the parent it draws in, the same chain the renderer
/// composes, so it follows rows, modulation and bodies as they move.
fn place(b: &mut Builder<'_>, card: &Item, role: &str, drawn: &Drawn, centre: u32) -> Option<NodeId> {
    let node = b.add_named(node(card.id.as_str(), &format!("place:{role}:{}", drawn.item)), "node.transform_components");
    for port in ["transform", "parent_transform"] {
        if let Some((from, from_port)) = b.input(drawn.object, port) {
            let to = if port == "transform" { "transform" } else { "parent" };
            b.wire(from, &from_port, node, to);
        }
    }
    b.wire(centre, "transform", node, "frame");
    b.node_id_of(node)
}
