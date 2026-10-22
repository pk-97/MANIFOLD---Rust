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
//! scale follows its own target. A part's live position rows also drive its
//! source offset, so a part moved inside a bent group moves through the bend
//! (Blender's deform about an origin object). One instance per frame, all
//! driven by the card's one set of manifest params, `<card id>.<param>`.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::physics_members::BODY_TYPE_ID;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, SceneDoc, param_id};
use manifold_core::scene_modifier_preset::{SceneContextValue, SceneModifierInstanceDef, SceneStageSource, SceneTargetSelection};
use manifold_node_engine::load::expand::context_node_id;
use manifold_core::scene_schema::modifier_type;

use super::builder::{Builder, Drawn, FramePlan, SCENE};
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
        if let ItemFields::Modifier(card) = &item.fields {
            emit(b, item, card, &lineage, &mut shattered)?;
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
        for (_, _, members) in &mut frames {
            *members = shatterable(b, members, shattered);
            // Every material part of a body becomes at least one piece.
            for parts in bodies(b, members).values() {
                pieces = pieces.max(*parts as u32);
            }
        }
    }
    let held = [("fragments", pieces as f32)];
    let held: &[(&str, f32)] = if card.modifier == ModifierType::Shatter { &held } else { &[] };
    let (mut graph, specs) = recipe_card(entry.recipe, &lookup, &host_id, held)?;
    super::deformers::tune(&mut graph, card);
    let follows_offsets = reads_offsets(&graph);
    // The card keeps its params with nothing to act on, so its values,
    // mappings and modulation survive a toggle or an emptied target list.
    b.push_params(specs.iter().map(|(spec, _)| spec.clone()));
    for (role, root, members) in frames {
        if members.is_empty() {
            continue;
        }
        let shatter = card.modifier == ModifierType::Shatter;
        let (plan, follows) = plan(b, &members, shatter, root.as_ref(), lineage);
        for (chunk, part) in members.chunks(TARGETS_PER_INSTANCE).enumerate() {
            let suffix = if chunk == 0 { role.clone() } else { format!("{role}:{chunk}") };
            let id = node(item.id.as_str(), &format!("mod:{suffix}"));
            let mut objects = Vec::with_capacity(part.len());
            for drawn in part {
                objects.push(b.node_id_of(drawn.object).ok_or("a modifier target has no node id")?);
            }
            let offsets = plan.offsets.iter().filter(|(target, _)| objects.contains(target)).cloned().collect();
            let followed: Vec<&Follow> = follows.iter().filter(|(_, target, _)| objects.contains(target)).collect();
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
            if follows_offsets {
                for (transform, target, shift) in followed {
                    for (axis, (pos, value)) in AXES.into_iter().enumerate() {
                        let context = context_node_id(&id, target, value);
                        b.follow_into(*transform, pos, &context, "value", shift[axis] as f32);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Each position row and the source offset it moves.
const AXES: [(&str, SceneContextValue); 3] = [
    ("pos_x", SceneContextValue::SourceOffsetX),
    ("pos_y", SceneContextValue::SourceOffsetY),
    ("pos_z", SceneContextValue::SourceOffsetZ),
];

/// Whether a recipe reads its targets' source offsets.
fn reads_offsets(graph: &EffectGraphDef) -> bool {
    graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .is_some_and(|recipe| {
            recipe.stages.iter().flat_map(|stage| &stage.inputs).any(|input| {
                matches!(input.source, SceneStageSource::Context { value: SceneContextValue::SourceOffsetX })
            })
        })
}

/// A frame: its role, its root item (`None`: the whole scene) and its parts.
type Frame = (String, Option<ItemId>, Vec<Drawn>);

/// The card's frames, one per Object or Group target in the order they are
/// listed, or one for the whole scene. A part reached by two targets bends
/// once, in the first one's frame.
fn frames(b: &Builder<'_>, card: &ModifierFields, lineage: &BTreeMap<ItemId, Vec<ItemId>>) -> Vec<Frame> {
    match &card.targets {
        ModifierTargets::Scene => vec![("scene".to_owned(), None, b.drawn.clone())],
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
                    (target.to_string(), Some(target.clone()), parts)
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

/// A part's live position follow: its transform node, its node id, and what
/// its source offset adds to its live position (its groups' share, less the
/// frame's centre).
type Follow = (u32, NodeId, [f64; 3]);

/// Where `drawn` sits in the frame of `root` (the whole scene when `None`):
/// its own position plus every group's between it and the root, as the
/// compiler starts them. The second value is the groups' share. An Object
/// target is its own frame, so it sits at its own origin.
fn placement(
    b: &mut Builder<'_>,
    drawn: &Drawn,
    root: Option<&ItemId>,
    lineage: &BTreeMap<ItemId, Vec<ItemId>>,
) -> ([f64; 3], [f64; 3]) {
    if root == Some(&drawn.item) {
        return ([0.0; 3], [0.0; 3]);
    }
    let ancestors = lineage.get(&drawn.item).map_or(&[][..], Vec::as_slice);
    let below = match root {
        None => ancestors,
        Some(root) => ancestors.iter().position(|above| above == root).map_or(&[][..], |at| &ancestors[at + 1..]),
    };
    let position = |b: &mut Builder<'_>, item: &ItemId| -> [f64; 3] {
        let transform = b.id(item.as_str(), "transform");
        std::array::from_fn(|axis| transform.and_then(|node| b.started(node, AXES[axis].0)).map_or(0.0, f64::from))
    };
    let mut groups = [0.0; 3];
    for group in below {
        let at = position(b, group);
        for axis in 0..3 {
            groups[axis] += at[axis];
        }
    }
    let own = position(b, &drawn.item);
    (std::array::from_fn(|axis| own[axis] + groups[axis]), groups)
}

/// The shared frame of `members` rooted at `root`: each one's source offset
/// from the frame's centre, its radius, and each part's live follow. Shatter
/// follows the live body pose instead, so its offset is only its source's
/// own recentring.
fn plan(
    b: &mut Builder<'_>,
    members: &[Drawn],
    shatter: bool,
    root: Option<&ItemId>,
    lineage: &BTreeMap<ItemId, Vec<ItemId>>,
) -> (FramePlan, Vec<Follow>) {
    let placed: Vec<([f64; 3], [f64; 3])> = members.iter().map(|drawn| placement(b, drawn, root, lineage)).collect();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (drawn, (at, _)) in members.iter().zip(&placed) {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(at[axis] + f64::from(drawn.rest.lo[axis]));
            hi[axis] = hi[axis].max(at[axis] + f64::from(drawn.rest.hi[axis]));
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
    let mut offsets = Vec::with_capacity(members.len());
    let mut follows = Vec::new();
    for (drawn, (at, groups)) in members.iter().zip(&placed) {
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
            std::array::from_fn(|axis| at[axis] - centre[axis])
        };
        let own_frame = root == Some(&drawn.item);
        if !shatter
            && !own_frame
            && let Some(transform) = b.id(drawn.item.as_str(), "transform")
        {
            follows.push((transform, target.clone(), std::array::from_fn(|axis| groups[axis] - centre[axis])));
        }
        offsets.push((target, offset));
    }
    (FramePlan { offsets, radius }, follows)
}
