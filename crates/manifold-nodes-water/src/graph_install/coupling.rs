//! Prepare the physical rigid/fluid pairs visible in authored render scenes.

use std::collections::BTreeMap;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::SceneTargetSelection;

use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_core::scene_impulse::{ImpulseTarget, RigidImpulseTargets};

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
    let mut scenes = Vec::new();
    for reference in index.by_ref.keys() {
        if index.node(reference)?.type_id == "node.render_scene" {
            scenes.push(reference.clone());
        }
    }

    let mut pairs = BTreeMap::<(String, String), (RigidImpulseTargets, Vec<String>)>::new();
    let mut fluid_owners = BTreeMap::<String, String>::new();
    let mut rigid_owners = BTreeMap::<String, String>::new();

    for scene in scenes {
        let recipients = impulse_recipients_with_index(
            &index,
            &scene,
            &SceneTargetSelection::AllObjects,
            registry,
        )?;
        // Water pairs by its particle producer, whether or not that domain
        // takes scene forces yet.
        let mut fluids = Vec::<String>::new();
        for object in index.scene_objects(&scene)? {
            if let Some(domain) = liquid_domain_of(&index, &object)?
                && !fluids.iter().any(|known| known == domain.as_str())
            {
                fluids.push(domain.as_str().to_owned());
            }
        }
        let mut rigids = BTreeMap::<String, RigidImpulseTargets>::new();
        for (node, target) in recipients {
            match target {
                // A swarm feels the liquid through no coupling: it pairs with nothing.
                ImpulseTarget::Fluid | ImpulseTarget::Swarm => {}
                ImpulseTarget::Rigid(targets) => {
                    rigids
                        .entry(node.as_str().to_owned())
                        .and_modify(|existing| *existing = std::mem::take(existing).union(targets.clone()))
                        .or_insert(targets);
                }
                ImpulseTarget::FluidAndRigid(_) => {
                    return Err(ambiguous(
                        &scene,
                        "scene recipient combines fluid and rigid delivery",
                    ));
                }
            }
        }

        if fluids.is_empty() || rigids.is_empty() {
            continue;
        }
        if rigids.len() != 1 {
            return Err(ambiguous(&scene, format!("scene must contain one rigid world (found {})", rigids.len())));
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
        .map(|((fluid, rigid), (colliders, observers))| CoupledSceneBinding {
            fluid: NodeId::new(fluid),
            rigid: NodeId::new(rigid),
            colliders,
            observers: observers.into_iter().map(NodeId::new).collect(),
        })
        .collect())
}

fn ambiguous(scene: &NodeId, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::AmbiguousScene {
        path: scene.to_string(),
        detail: detail.into(),
    }
}
