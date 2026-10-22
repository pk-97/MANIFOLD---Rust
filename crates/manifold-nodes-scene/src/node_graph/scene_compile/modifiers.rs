//! Modifiers (SCENE_DOCUMENT_DESIGN.md section 7.19 (Modifiers)): the
//! scene's Modifier items are one chain, applied in tree order. Each card
//! compiles to scene modifier instances of its type's stock recipe, and the
//! `load/expand` passes build the rest at load, addressed by node id.
//!
//! A card's targets decide its frames (ruling 20): each Object target bends
//! in its own frame, each Group target bends all its parts in one shared
//! frame, and the whole scene bends as one group of everything. A frame is
//! the rest-pose box of its parts (`Builder::drawn`): every part's source
//! offset is its place in that box, from the box's centre, and the radius is
//! half the box's diagonal, so a recipe reads every shape the same way and a
//! modifier's scale follows its own target. One instance per frame, all
//! driven by the card's one set of manifest params, `<card id>.<param>`.
//!
//! The scene-wide types have no targets and take the whole-scene frame.
//! Math View compiles here like any card; Scene Loop (`scene_loop.rs`) and
//! Scene Mirror (`scene_mirror.rs`) have their own emitters.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::physics_members::BODY_TYPE_ID;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, SceneDoc, param_id};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::modifier_type;

use super::builder::{Builder, Drawn, FramePlan, SCENE, rest_box};
use super::forces::{bindings, node, recipe_card};

/// The most objects one instance targets (`frames::selected_objects`); a
/// larger frame is split into instances that share it.
const TARGETS_PER_INSTANCE: usize = 256;

/// A frame's smallest radius, so a flat or empty box still measures.
const MIN_RADIUS: f64 = 1e-3;

/// Shatter's most pieces per body (the recipe's Pieces range).
const MAX_PIECES: u32 = 32;

/// Emit every Modifier card, in chain order.
pub(crate) fn emit_all(b: &mut Builder<'_>) -> Result<(), String> {
    let doc = b.doc;
    let mut lineage: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    doc.walk(|item, ancestors| {
        lineage.insert(item.id.clone(), ancestors.iter().map(|above| above.id.clone()).collect());
    });
    let mut shattered = BTreeSet::new();
    for item in doc.items.iter().filter(|item| item.kind() == ItemKind::Modifier) {
        let ItemFields::Modifier(card) = &item.fields else {
            continue;
        };
        match card.modifier {
            // The corridor is the first instance stage (`scene_loop.rs`).
            ModifierType::SceneLoop => {}
            ModifierType::SceneMirror => super::scene_mirror::emit(b, item, card)?,
            _ => emit(b, item, card, &lineage, &mut shattered)?,
        }
    }
    Ok(())
}

/// The Objects a deforming card that is on reaches, directly, through a
/// Group target, or as the whole scene.
pub(crate) fn deformed_items(doc: &SceneDoc) -> BTreeSet<ItemId> {
    let cards: Vec<&ModifierTargets> = doc
        .items
        .iter()
        .filter_map(|item| match &item.fields {
            ItemFields::Modifier(card) if card.on && modifier_type(card.modifier).deforms => Some(&card.targets),
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
    shattered: &mut BTreeSet<u32>,
) -> Result<(), String> {
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    let mut frames = if card.on { frames(b, card, lineage) } else { Vec::new() };
    let mut pieces = card.pieces.clamp(2, MAX_PIECES);
    if card.modifier == ModifierType::Shatter {
        for (_, members) in &mut frames {
            *members = shatterable(b, members, shattered);
            // Every material part of a body becomes at least one piece.
            for parts in bodies(b, members).values() {
                pieces = pieces.max(*parts as u32);
            }
        }
    }
    let held = [("fragments", pieces as f32)];
    let held: &[(&str, f32)] = if card.modifier == ModifierType::Shatter { &held } else { &[] };
    let (graph, specs) = recipe_card(entry.recipe, &lookup, &host_id, held)?;
    // The card keeps its params with nothing to act on, so its values,
    // mappings and modulation survive a toggle or an emptied target list.
    b.push_params(specs.iter().map(|(spec, _)| spec.clone()));
    for (role, members) in frames {
        if members.is_empty() {
            continue;
        }
        let plan = plan(b, &members, card.modifier == ModifierType::Shatter);
        for (chunk, part) in members.chunks(TARGETS_PER_INSTANCE).enumerate() {
            let suffix = if chunk == 0 { role.clone() } else { format!("{role}:{chunk}") };
            let id = node(item.id.as_str(), &format!("mod:{suffix}"));
            let mut objects = Vec::with_capacity(part.len());
            for drawn in part {
                objects.push(b.node_id_of(drawn.object).ok_or("a modifier target has no node id")?);
            }
            let offsets = plan.offsets.iter().filter(|(target, _)| objects.contains(target)).cloned().collect();
            let instance = SceneModifierInstanceDef {
                id: id.clone(),
                scene: node(SCENE, "render"),
                targets: SceneTargetSelection::Explicit { objects },
                mesh_frames: Vec::new(),
                legacy_math_view_carrier: None,
                graph: Box::new(graph.clone()),
            };
            let frame = FramePlan { offsets, radius: plan.radius };
            b.push_modifier(instance, Vec::new(), bindings(&specs, &id), Some(frame));
        }
    }
    Ok(())
}

/// The card's frames: (role, parts), one per Object or Group target in the
/// order they are listed, or one for the whole scene. A part reached by two
/// targets bends once, in the first one's frame.
fn frames(b: &Builder<'_>, card: &ModifierFields, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Vec<(String, Vec<Drawn>)> {
    // A scene-wide card (Math View) always reads the whole scene.
    if card.modifier.scene_wide() {
        return vec![("scene".to_owned(), b.drawn.clone())];
    }
    match &card.targets {
        ModifierTargets::Scene => vec![("scene".to_owned(), b.drawn.clone())],
        ModifierTargets::Items(ids) => {
            let mut claimed = BTreeSet::new();
            ids.iter()
                .map(|target| {
                    let parts = b
                        .drawn
                        .iter()
                        .filter(|drawn| {
                            &drawn.item == target || lineage.get(&drawn.item).is_some_and(|above| above.contains(target))
                        })
                        .filter(|drawn| claimed.insert(drawn.object))
                        .cloned()
                        .collect();
                    (target.to_string(), parts)
                })
                .collect()
        }
    }
}

/// The body node a drawn object is posed by, if a rigid body poses it.
fn body_of(b: &Builder<'_>, drawn: &Drawn) -> Option<u32> {
    let (from, port) = b.input(drawn.object, "parent_transform").or_else(|| b.input(drawn.object, "transform"))?;
    (port == "pose" && b.node_type(from) == Some(BODY_TYPE_ID)).then_some(from)
}

/// The parts of `members` Shatter can break: posed by a Body no earlier
/// Shatter card breaks, drawing an undeformed mesh from a file.
fn shatterable(b: &Builder<'_>, members: &[Drawn], shattered: &mut BTreeSet<u32>) -> Vec<Drawn> {
    let kept: Vec<Drawn> = members
        .iter()
        .filter(|drawn| {
            let from_file = b
                .input(drawn.object, "vertices")
                .is_some_and(|(source, _)| b.node_type(source) == Some("node.gltf_mesh_source"));
            from_file && body_of(b, drawn).is_some_and(|body| !shattered.contains(&body))
        })
        .cloned()
        .collect();
    // A body whose parts outnumber the most pieces stays whole.
    let counts = bodies(b, &kept);
    let kept: Vec<Drawn> = kept
        .into_iter()
        .filter(|drawn| body_of(b, drawn).is_some_and(|body| counts.get(&body).is_some_and(|parts| *parts <= MAX_PIECES as usize)))
        .collect();
    shattered.extend(kept.iter().filter_map(|drawn| body_of(b, drawn)));
    kept
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

/// The shared frame of `members`: each one's source offset from the frame's
/// centre, and its radius. Shatter follows the live body pose instead, so
/// its offset is only its source's own recentring.
fn plan(b: &mut Builder<'_>, members: &[Drawn], shatter: bool) -> FramePlan {
    let (lo, hi) = rest_box(members.iter().map(|drawn| &drawn.rest)).unwrap_or(([0.0; 3], [0.0; 3]));
    let centre: [f64; 3] = std::array::from_fn(|axis| (f64::from(lo[axis]) + f64::from(hi[axis])) * 0.5);
    let radius = (0..3)
        .map(|axis| (f64::from(hi[axis]) - f64::from(lo[axis])) * 0.5)
        .map(|half| half * half)
        .sum::<f64>()
        .sqrt()
        .max(MIN_RADIUS);
    let mut offsets = Vec::with_capacity(members.len());
    for drawn in members {
        let Some(target) = b.node_id_of(drawn.object) else {
            continue;
        };
        let offset = if shatter {
            let source = b.input(drawn.object, "vertices").map(|(source, _)| source);
            std::array::from_fn(|axis| {
                let param = ["translate_x", "translate_y", "translate_z"][axis];
                source.and_then(|source| b.started(source, param)).map_or(0.0, f64::from)
            })
        } else {
            std::array::from_fn(|axis| f64::from(drawn.rest.offset[axis]) - centre[axis])
        };
        offsets.push((target, offset));
    }
    FramePlan { offsets, radius }
}
