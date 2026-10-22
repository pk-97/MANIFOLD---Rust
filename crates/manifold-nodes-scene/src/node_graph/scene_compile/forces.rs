//! Forces and mesh modifiers (SCENE_DOCUMENT_DESIGN.md section 7.12 (Forces)
//! and section 6, difference 9): each compiles to one scene modifier
//! instance on the layer's graph, from the stock recipe its type names, and
//! the `load/expand` passes build the rest at load. The recipe's own params
//! become the item's manifest params, `<item id>.<param>` for a Force and
//! `<object id>.<modifier key>.<param>` for an Object's modifier, each
//! bound to the instance's param. Which of them are on the card, and where, is
//! the (kind, property) table (`scene_schema::force`, `scene_schema::modifier`).

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingDef, BindingTarget, ParamSpecDef};
use manifold_core::effects::ParamConvert;
use manifold_core::scene_doc::{
    ForceFields, ForceTargets, ForceType, Item, ItemFields, ItemId, MeshModifier, PhysicsSetting, param_id,
};
use manifold_core::scene_modifier_preset::{
    MAX_STAGE_CARRYING_MODIFIERS, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection, force_slots_message,
};
use manifold_core::scene_schema::{self, Exposure, PropertyRow, modifier_param_id, modifier_rows, modifiers_unavailable};
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
        ForceType::Attractor => "Attractor",
        ForceType::Explosion => "Explosion",
    }
}

/// Emit every Force and every Object's modifiers, in tree order.
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
        match &item.fields {
            ItemFields::Force(force) => emit_force(b, item, force, &found)?,
            ItemFields::Object(object) => {
                for modifier in &object.modifiers {
                    emit_mesh_modifier(b, item, modifier)?;
                }
            }
            _ => {}
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

fn node(owner: &str, role: &str) -> NodeId {
    NodeId::new(format!("{owner}:{role}"))
}

/// The scene object node an Object item compiled to, if it did.
fn object_ref(b: &Builder<'_>, item: &Item) -> Option<SceneNodeRef> {
    b.id(item.id.as_str(), "object").map(|_| SceneNodeRef { scope: Vec::new(), node: node(item.id.as_str(), "object") })
}

fn emit_force(b: &mut Builder<'_>, item: &Item, force: &ForceFields, items: &[Placed<'_>]) -> Result<(), String> {
    let targets = match &force.targets {
        ForceTargets::Physics => SceneTargetSelection::AllObjects,
        ForceTargets::Items(ids) => {
            // The objects at or under each chosen item that physics moves, once each.
            let mut objects: Vec<SceneNodeRef> = Vec::new();
            for placed in items.iter().filter(|placed| placed.physical) {
                if ids.iter().any(|chosen| placed.lineage.contains(chosen))
                    && let Some(target) = object_ref(b, placed.item)
                    && !objects.contains(&target)
                {
                    objects.push(target);
                }
            }
            SceneTargetSelection::Explicit { objects }
        }
    };
    let lookup = |param: &str| scene_schema::row_for(&item.fields, param);
    attach(b, item, "force", recipe_of(force.force), targets, &|param| param_id(&item.id, param), &lookup)
}

fn emit_mesh_modifier(b: &mut Builder<'_>, item: &Item, modifier: &MeshModifier) -> Result<(), String> {
    if let ItemFields::Object(object) = &item.fields
        && let Some(reason) = modifiers_unavailable(&object.shape)
    {
        return Err(format!("{}: {reason}", item.name));
    }
    // An object that compiled to no draw (a mesh from a file before its
    // emitter) has nothing to modify.
    let Some(target) = object_ref(b, item) else {
        return Ok(());
    };
    let key = modifier.key.as_str();
    let lookup = |param: &str| modifier_rows(modifier.kind).iter().find(|row| row.property == param);
    attach(
        b,
        item,
        &format!("mod:{key}"),
        modifier.kind.recipe(),
        SceneTargetSelection::Explicit { objects: vec![target] },
        &|param| modifier_param_id(&item.id, key, param),
        &lookup,
    )
}

/// One scene modifier instance of `recipe` for `item`, with a manifest param
/// for every recipe param its row puts on the card.
fn attach(
    b: &mut Builder<'_>,
    item: &Item,
    role: &str,
    recipe: &str,
    targets: SceneTargetSelection,
    host_id: &dyn Fn(&str) -> String,
    lookup: &dyn Fn(&str) -> Option<&'static PropertyRow>,
) -> Result<(), String> {
    if b.modifier_count() >= MAX_STAGE_CARRYING_MODIFIERS {
        return Err(force_slots_message("Adding a force or modifier", 1, 0, MAX_STAGE_CARRYING_MODIFIERS));
    }
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
    let held: Vec<(&str, f32)> = rows.iter().filter_map(|(spec, row)| row.default.map(|value| (spec.id.as_str(), value))).collect();
    tune_force_card(&mut graph, &held)?;
    let tuned = graph.preset_metadata.as_ref().map_or(&[][..], |meta| meta.params.as_slice()).to_vec();

    let modifier_id = node(item.id.as_str(), role);
    let mut specs = Vec::new();
    let mut bindings = Vec::new();
    for (spec, row) in rows {
        let Exposure::Card(facts) = row.exposure else {
            continue;
        };
        let current = tuned.iter().find(|candidate| candidate.id == spec.id).unwrap_or(spec);
        let id = host_id(&spec.id);
        let mut host: ParamSpecDef = current.clone();
        host.id = id.clone();
        if let Some(name) = row.name {
            host.name = name.to_owned();
        }
        host.scene = Some(facts.desc(row.section));
        host.card_visible = facts.role == SurfaceRole::Performance;
        host.limit = facts.limit;
        host.unit = facts.unit;
        bindings.push(BindingDef {
            id,
            label: host.name.clone(),
            default_value: host.default_value,
            target: BindingTarget::SceneModifier { modifier_id: modifier_id.clone(), param_id: spec.id.clone() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
        specs.push(host);
    }
    let instance = SceneModifierInstanceDef {
        id: modifier_id,
        scene: SceneNodeRef { scope: Vec::new(), node: node(SCENE, "render") },
        targets,
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(graph),
    };
    b.push_modifier(instance, specs, bindings);
    Ok(())
}
