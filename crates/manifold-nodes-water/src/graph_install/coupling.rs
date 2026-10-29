//! Prepare the physical rigid/fluid pairs visible in authored render scenes.

use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::SceneTargetSelection;

use manifold_core::scene_impulse::{ImpulseTarget, RigidImpulseTargets};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::primitive::PrimitiveSpec;

use manifold_gas::primitives::gas_volume::GasVolume;

use manifold_node_engine::load::expand::SceneModifierExpandError;
use manifold_node_engine::load::expand::impulse_recipients_with_index;

/// One physical fluid domain and the rigid recipients coupled to it, and the
/// scene's other fluid domains, which take its solids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoupledSceneBinding {
    pub fluid: NodeId,
    pub rigid: NodeId,
    pub colliders: RigidImpulseTargets,
    pub observers: Vec<NodeId>,
}

/// Prepare the physical rigid/fluid pairs visible in every render scene.
///
/// This only resolves authored graph provenance. Runtime scheduling and
/// publication remain the responsibility of the caller.
pub fn prepare_coupled_scenes(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<Vec<CoupledSceneBinding>, SceneModifierExpandError> {
    let index = FlatSceneIndex::build(owner)?;
    let mut pairs = BTreeMap::<(String, String), (RigidImpulseTargets, Vec<String>)>::new();
    let mut fluid_owners = BTreeMap::<String, String>::new();
    let mut rigid_owners = BTreeMap::<String, String>::new();

    for scene in render_scenes(&index)? {
        let fluids = scene_fluids(&index, &scene)?;
        let rigids = scene_rigids(&index, &scene, registry)?;
        if fluids.is_empty() || rigids.is_empty() {
            continue;
        }
        if rigids.len() != 1 {
            return Err(ambiguous(
                &scene,
                format!(
                    "scene must contain one rigid world (found {})",
                    rigids.len()
                ),
            ));
        }

        // The rigid world steps inside one domain, so the first water drawn
        // owns it; every other water takes its solids.
        let mut fluids = fluids.into_iter();
        let fluid = fluids.next().expect("a fluid was checked");
        let observers: Vec<String> = fluids.collect();
        let (rigid, colliders) = rigids
            .into_iter()
            .next()
            .expect("one rigid world was checked");

        if let Some(previous) = fluid_owners.insert(fluid.clone(), rigid.clone())
            && previous != rigid
        {
            return Err(ambiguous(
                &scene,
                format!(
                    "fluid domain '{}' is coupled to rigid worlds '{}' and '{}'",
                    fluid, previous, rigid
                ),
            ));
        }
        if let Some(previous) = rigid_owners.insert(rigid.clone(), fluid.clone())
            && previous != fluid
        {
            return Err(ambiguous(
                &scene,
                format!(
                    "rigid world '{}' is coupled to fluid domains '{}' and '{}'",
                    rigid, previous, fluid
                ),
            ));
        }

        let (held, watching) = pairs.entry((fluid, rigid)).or_default();
        *held = std::mem::take(held).union(colliders);
        for observer in observers {
            if !watching.contains(&observer) {
                watching.push(observer);
            }
        }
    }

    Ok(pairs
        .into_iter()
        .map(
            |((fluid, rigid), (colliders, observers))| CoupledSceneBinding {
                fluid: NodeId::new(fluid),
                rigid: NodeId::new(rigid),
                colliders,
                observers: observers.into_iter().map(NodeId::new).collect(),
            },
        )
        .collect())
}

/// Where a Smoke or Fire takes its scene's solids from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GasSolids {
    /// The water that owns the scene's World, as the scene's other waters do.
    Water(NodeId),
    /// The World itself, which runs free with no water to own it.
    World(NodeId),
}

/// Every Smoke and Fire drawn in a render scene with a rigid world, and
/// where it takes the scene's Colliders and Bodies from.
pub fn prepare_gas_solids(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<Vec<(NodeId, GasSolids)>, SceneModifierExpandError> {
    let index = FlatSceneIndex::build(owner)?;
    let mut found = Vec::<(NodeId, GasSolids)>::new();
    for scene in render_scenes(&index)? {
        let gases = scene_gases(&index, &scene)?;
        if gases.is_empty() {
            continue;
        }
        let rigids = scene_rigids(&index, &scene, registry)?;
        let rigid = match rigids.len() {
            0 => continue,
            1 => rigids
                .into_keys()
                .next()
                .expect("one rigid world was counted"),
            found => {
                return Err(ambiguous(
                    &scene,
                    format!("scene must contain one rigid world (found {found})"),
                ));
            }
        };
        let source = match scene_fluids(&index, &scene)?.into_iter().next() {
            Some(water) => GasSolids::Water(NodeId::new(water)),
            None => GasSolids::World(NodeId::new(rigid)),
        };
        for gas in gases {
            if !found.iter().any(|(known, _)| *known == gas) {
                found.push((gas, source.clone()));
            }
        }
    }
    Ok(found)
}

/// Every render scene of the def.
fn render_scenes(index: &FlatSceneIndex) -> Result<Vec<NodeId>, SceneModifierExpandError> {
    let mut scenes = Vec::new();
    for reference in index.by_ref.keys() {
        if index.node(reference)?.type_id == "node.render_scene" {
            scenes.push(reference.clone());
        }
    }
    Ok(scenes)
}

/// A scene's waters in draw order. Water pairs by its particle producer,
/// whether or not that domain takes scene forces yet.
fn scene_fluids(
    index: &FlatSceneIndex,
    scene: &NodeId,
) -> Result<Vec<String>, SceneModifierExpandError> {
    let mut fluids = Vec::<String>::new();
    for object in index.scene_objects(scene)? {
        if let Some(domain) = liquid_domain_of(index, &object)?
            && !fluids.iter().any(|known| known == domain.as_str())
        {
            fluids.push(domain.as_str().to_owned());
        }
    }
    Ok(fluids)
}

/// A scene's gas solvers in draw order: what its objects' `gas_state` reads.
fn scene_gases(
    index: &FlatSceneIndex,
    scene: &NodeId,
) -> Result<Vec<NodeId>, SceneModifierExpandError> {
    let mut gases = Vec::<NodeId>::new();
    for object in index.scene_objects(scene)? {
        let Some(wire) = index.input(&object, "gas_state")? else {
            continue;
        };
        if let Some(gas) = index.by_id.get(&wire.from_node)
            && index.node(gas)?.type_id == <GasVolume as PrimitiveSpec>::TYPE_ID
            && !gases.contains(gas)
        {
            gases.push(gas.clone());
        }
    }
    Ok(gases)
}

/// A scene's rigid worlds, each with the bodies the scene's hits reach.
fn scene_rigids(
    index: &FlatSceneIndex,
    scene: &NodeId,
    registry: &PrimitiveRegistry,
) -> Result<BTreeMap<String, RigidImpulseTargets>, SceneModifierExpandError> {
    let recipients =
        impulse_recipients_with_index(index, scene, &SceneTargetSelection::AllObjects, registry)?;
    let mut rigids = BTreeMap::<String, RigidImpulseTargets>::new();
    for (node, target) in recipients {
        match target {
            // A swarm feels the liquid through no coupling: it pairs with nothing.
            ImpulseTarget::Fluid | ImpulseTarget::Swarm => {}
            ImpulseTarget::Rigid(targets) => {
                rigids
                    .entry(node.as_str().to_owned())
                    .and_modify(|existing| {
                        *existing = std::mem::take(existing).union(targets.clone())
                    })
                    .or_insert(targets);
            }
            ImpulseTarget::FluidAndRigid(_) => {
                return Err(ambiguous(
                    scene,
                    "scene recipient combines fluid and rigid delivery",
                ));
            }
        }
    }
    Ok(rigids)
}

fn ambiguous(scene: &NodeId, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::AmbiguousScene {
        path: scene.to_string(),
        detail: detail.into(),
    }
}
