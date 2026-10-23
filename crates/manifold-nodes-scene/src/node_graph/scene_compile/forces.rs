//! Forces (SCENE_DOCUMENT_DESIGN.md section 7.12 (Forces)): each compiles to
//! one scene modifier instance on the layer's graph, from the stock recipe
//! its type names, and the `load/expand` passes build the rest at load. The
//! recipe's own params become the item's manifest params, `<item id>.<param>`,
//! each bound to the instance's param. Which of them are on the card, and
//! where, is the (kind, property) table (`scene_schema::force`). Modifier
//! cards reuse [`recipe_card`] and [`bindings`] (`modifiers.rs`).

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef};
use manifold_core::effects::ParamConvert;
use manifold_core::scene_doc::{ForceFields, ForceTargets, ForceType, Item, ItemFields, ItemId, ItemKind, PhysicsSetting, param_id};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::{self, Exposure, PropertyRow};
use manifold_foundation::scene_surface::SurfaceRole;

use super::builder::{Builder, SCENE};
use crate::node_graph::scene_modifier_authoring::{stock_force, tune_force_card};

/// The stock recipe a force type is. Drag is the Wind recipe held at speed
/// zero (the rows hold it there).
fn recipe_of(force: ForceType) -> &'static str {
    match force {
        ForceType::Radial => "RadialForce",
        ForceType::Uniform => "UniformForce",
        ForceType::Vortex => "VortexForce",
        ForceType::Wind | ForceType::Drag => "Wind",
    }
}

/// Emit every Force, in tree order.
pub(crate) fn emit_all(b: &mut Builder<'_>) -> Result<(), String> {
    let doc = b.doc;
    let mut found: Vec<Placed<'_>> = Vec::new();
    doc.walk(|item, ancestors| {
        let physical = item.fields.physics() != PhysicsSetting::None
            || ancestors.iter().any(|holder| holder.fields.physics() != PhysicsSetting::None);
        let lineage = ancestors.iter().map(|holder| holder.id.clone()).chain([item.id.clone()]).collect();
        found.push(Placed { item, physical, lineage });
    });
    for Placed { item, .. } in &found {
        if let ItemFields::Force(force) = &item.fields {
            emit_force(b, item, force, &found)?;
        }
    }
    Ok(())
}

/// An item in tree order: whether physics moves it (its own setting or an
/// enclosing group's) and the ids from the top down to it.
struct Placed<'d> {
    item: &'d Item,
    physical: bool,
    lineage: Vec<ItemId>,
}

pub(super) fn node(owner: &str, role: &str) -> NodeId {
    NodeId::new(format!("{owner}:{role}"))
}

/// The scene object node an Object item compiled to, if it did.
fn object_ref(b: &Builder<'_>, item: &Item) -> Option<NodeId> {
    b.id(item.id.as_str(), "object").map(|_| node(item.id.as_str(), "object"))
}

/// The scene objects a swarm draws its particles with: its own built-in
/// shape, or the Object or Group it draws as (every part of the Group). A
/// force aimed at them enters at the swarm's forces node.
fn swarm_objects(b: &mut Builder<'_>, swarm: &ItemId, items: &[Placed<'_>]) -> Vec<NodeId> {
    let copies = b.id(swarm.as_str(), "copies");
    let drawn_by_swarm = |owner: &ItemId| {
        copies.is_some()
            && items.iter().find(|placed| &placed.item.id == owner).is_some_and(|placed| {
                placed.lineage.iter().any(|above| b.swarm_draws.get(above).copied() == copies)
            })
    };
    let drawn: Vec<u32> = b
        .objects
        .iter()
        .zip(&b.object_items)
        .filter(|(_, owner)| *owner == swarm || drawn_by_swarm(owner))
        .map(|(&object, _)| object)
        .collect();
    drawn.into_iter().filter_map(|object| b.node_id_of(object)).collect()
}

fn emit_force(b: &mut Builder<'_>, item: &Item, force: &ForceFields, items: &[Placed<'_>]) -> Result<(), String> {
    let targets = match &force.targets {
        ForceTargets::Physics => SceneTargetSelection::AllObjects,
        ForceTargets::Items(ids) => {
            // The objects at or under each chosen item that physics moves, once each.
            let mut objects: Vec<NodeId> = Vec::new();
            for placed in items.iter().filter(|placed| placed.physical) {
                if ids.iter().any(|chosen| placed.lineage.contains(chosen))
                    && let Some(target) = object_ref(b, placed.item)
                    && !objects.contains(&target)
                {
                    objects.push(target);
                }
            }
            // A chosen Swarm (or one inside a chosen Group): what it draws.
            for placed in items.iter().filter(|placed| placed.item.kind() == ItemKind::Swarm) {
                if ids.iter().any(|chosen| placed.lineage.contains(chosen)) {
                    for target in swarm_objects(b, &placed.item.id, items) {
                        if !objects.contains(&target) {
                            objects.push(target);
                        }
                    }
                }
            }
            SceneTargetSelection::Explicit { objects }
        }
    };
    let lookup = |param: &str| scene_schema::row_for(&item.fields, param);
    let (graph, specs) = recipe_card(recipe_of(force.force), &lookup, &|param| param_id(&item.id, param), &[])?;
    let id = node(item.id.as_str(), "force");
    let bindings = bindings(&specs, &id);
    let instance = SceneModifierInstanceDef {
        id,
        scene: node(SCENE, "render"),
        targets,
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(graph),
    };
    b.push_modifier(instance, specs.into_iter().map(|(spec, _)| spec).collect(), bindings, None);
    Ok(())
}

/// One manifest param and the recipe param it drives.
pub(super) type CardParam = (ParamSpecDef, String);

/// The stock `recipe`, tuned to its rows' defaults and to `held` (values
/// the item's fields set), with a manifest param for every recipe param its
/// row puts on the card, named by `host_id`.
pub(super) fn recipe_card(
    recipe: &str,
    lookup: &dyn Fn(&str) -> Option<&'static PropertyRow>,
    host_id: &dyn Fn(&str) -> String,
    held: &[(&str, f32)],
) -> Result<(EffectGraphDef, Vec<CardParam>), String> {
    let mut graph = stock_force(recipe, &[])?;
    let local = graph.preset_metadata.clone().ok_or_else(|| format!("The stock {recipe} recipe has no card"))?;
    let preparation: &[String] = local.scene_modifier.as_ref().map_or(&[], |recipe| &recipe.preparation_params);

    let mut rows = Vec::new();
    for spec in local.params.iter().filter(|spec| !preparation.contains(&spec.id)) {
        let row = lookup(&spec.id)
            .ok_or_else(|| format!("{recipe}: the scene table has no row for its {} param", spec.id))?;
        rows.push((spec, row));
    }
    // The kind's defaults reach the recipe itself, so its graph agrees with its card.
    let mut tuning: Vec<(&str, f32)> =
        rows.iter().filter_map(|(spec, row)| row.default.map(|value| (spec.id.as_str(), value))).collect();
    tuning.extend_from_slice(held);
    tune_force_card(&mut graph, &tuning)?;
    let tuned = graph.preset_metadata.as_ref().map_or(&[][..], |meta| meta.params.as_slice()).to_vec();

    let mut specs = Vec::new();
    for (spec, row) in rows {
        let Exposure::Card(facts) = row.exposure else {
            continue;
        };
        let current = tuned.iter().find(|candidate| candidate.id == spec.id).unwrap_or(spec);
        let mut host: ParamSpecDef = current.clone();
        host.id = host_id(&spec.id);
        if let Some(name) = row.name {
            host.name = name.to_owned();
        }
        if let Some((min, max)) = facts.range {
            host.min = min;
            host.max = max;
        }
        host.scene = Some(facts.desc(row.section));
        host.card_visible = facts.role == SurfaceRole::Performance;
        host.limit = facts.limit;
        host.unit = facts.unit;
        specs.push((host, spec.id.clone()));
    }
    Ok((graph, specs))
}

/// The bindings that drive instance `modifier`'s recipe params from `specs`.
pub(super) fn bindings(specs: &[CardParam], modifier: &NodeId) -> Vec<BindingDef> {
    specs
        .iter()
        .map(|(host, param)| BindingDef {
            id: host.id.clone(),
            label: host.name.clone(),
            default_value: host.default_value,
            target: BindingTarget::SceneModifier { modifier_id: modifier.clone(), param_id: param.clone() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        })
        .collect()
}
