//! Prepare the physical rigid/fluid pairs visible in authored render scenes.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};

use crate::persistence::PrimitiveRegistry;
use crate::scene::impulse::{ImpulseTarget, RigidImpulseTargets};

use super::SceneModifierExpandError;
use super::acceleration::impulse_recipients_with_index;

/// One physical fluid domain and the rigid recipients coupled to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoupledSceneBinding {
    pub fluid: NodeId,
    pub rigid: NodeId,
    pub colliders: RigidImpulseTargets,
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

    let mut pairs = BTreeMap::<(String, String), RigidImpulseTargets>::new();
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
        let mut fluids = BTreeSet::new();
        for object in index.scene_objects(&scene)? {
            if let Some(domain) = liquid_domain_of(&index, &object)? {
                fluids.insert(domain.node.as_str().to_owned());
            }
        }
        let mut rigids = BTreeMap::<String, RigidImpulseTargets>::new();
        for (node, target) in recipients {
            match target {
                ImpulseTarget::Fluid => {}
                ImpulseTarget::Rigid(targets) => {
                    rigids
                        .entry(node.as_str().to_owned())
                        .and_modify(|existing| {
                            existing.bodies |= targets.bodies;
                            existing.copies |= targets.copies;
                        })
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
        if fluids.len() != 1 || rigids.len() != 1 {
            return Err(ambiguous(
                &scene,
                format!(
                    "scene must contain exactly one fluid domain and one rigid world (found {} fluids and {} rigid worlds)",
                    fluids.len(),
                    rigids.len()
                ),
            ));
        }

        let fluid = fluids.into_iter().next().expect("one fluid was checked");
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

        pairs
            .entry((fluid, rigid))
            .and_modify(|existing| {
                existing.bodies |= colliders.bodies;
                existing.copies |= colliders.copies;
            })
            .or_insert(colliders);
    }

    Ok(pairs
        .into_iter()
        .map(|((fluid, rigid), colliders)| CoupledSceneBinding {
            fluid: NodeId::new(fluid),
            rigid: NodeId::new(rigid),
            colliders,
        })
        .collect())
}

fn ambiguous(scene: &SceneNodeRef, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::AmbiguousScene {
        path: scene_path(scene),
        detail: detail.into(),
    }
}

fn scene_path(scene: &SceneNodeRef) -> String {
    scene
        .scope
        .iter()
        .map(NodeId::as_str)
        .chain(std::iter::once(scene.node.as_str()))
        .collect::<Vec<_>>()
        .join("/")
}
