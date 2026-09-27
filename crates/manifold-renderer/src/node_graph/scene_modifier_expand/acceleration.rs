//! Resolve scene objects to the physical input that owns their acceleration.

use std::collections::BTreeSet;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};

use crate::node_graph::persistence::PrimitiveRegistry;

use super::{SceneModifierExpandError, index::FlatSceneIndex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Recipient {
    pub object: SceneNodeRef,
    pub node: SceneNodeRef,
    pub port: String,
}

fn unsupported(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::UnsupportedEndpoint {
        path: path.into(),
        detail: detail.into(),
    }
}

/// Resolve one object. `None` means that the object is a valid scene object
/// with no physical producer behind it, which is an inactive force target.
pub(super) fn resolve(
    index: &FlatSceneIndex,
    object: &SceneNodeRef,
    registry: &PrimitiveRegistry,
) -> Result<Option<Recipient>, SceneModifierExpandError> {
    let mut candidates = Vec::new();
    for input in ["transform", "parent_transform", "instances", "vertices"] {
        if let Some(wire) = index.input(object, input)? {
            let Some(producer) = index.by_id.get(&wire.from_node) else {
                return Err(unsupported(
                    format!("{object:?}.{input}"),
                    "physical producer has no stable scene reference",
                ));
            };
            candidates.extend(trace(index, producer, &wire.from_port, registry)?);
        }
    }
    candidates.sort_by(|a, b| a.node.cmp(&b.node).then(a.port.cmp(&b.port)));
    candidates.dedup_by(|a, b| a.node == b.node && a.port == b.port);
    match candidates.as_slice() {
        [] => Ok(None),
        [candidate] => Ok(Some(Recipient {
            object: object.clone(),
            node: candidate.node.clone(),
            port: candidate.port.clone(),
        })),
        _ => Err(unsupported(
            format!("{object:?}"),
            "scene object resolves to multiple distinct physical recipients",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    node: SceneNodeRef,
    port: String,
}

fn trace(
    index: &FlatSceneIndex,
    start: &SceneNodeRef,
    start_port: &str,
    registry: &PrimitiveRegistry,
) -> Result<Vec<Candidate>, SceneModifierExpandError> {
    let mut pending = vec![(start.clone(), start_port.to_owned(), false)];
    let mut active = BTreeSet::new();
    let mut completed = BTreeSet::new();
    let mut candidates = Vec::new();
    while let Some((node, port, leaving)) = pending.pop() {
        let key = (node.clone(), port.clone());
        if leaving {
            active.remove(&key);
            completed.insert(key);
            continue;
        }
        if completed.contains(&key) {
            continue;
        }
        if !active.insert(key.clone()) {
            return Err(unsupported(
                format!("{node:?}.{port}"),
                "typed physical source chain contains a cycle",
            ));
        }
        pending.push((node.clone(), port.clone(), true));
        let def = index.node(&node)?;
        if def.type_id == "node.physics_world" {
            let input = if port == "instances" {
                Some(("copies".to_string(), "copies_acceleration".to_string()))
            } else {
                port.strip_prefix("pose_")
                    .filter(|slot| slot.parse::<usize>().is_ok_and(|slot| slot < 64))
                    .map(|slot| (format!("body_{slot}"), format!("body_acceleration_{slot}")))
            };
            if let Some((input, port)) = input {
                if index.input(&node, &input)?.is_some() {
                    candidates.push(Candidate { node, port });
                }
                continue;
            }
        }
        if def.type_id == "node.fluid_surface" && port == "vertices" {
            candidates.push(Candidate {
                node,
                port: "acceleration_field".into(),
            });
            continue;
        }
        let primitive = registry.construct(&def.type_id).ok_or_else(|| {
            unsupported(
                def.type_id.clone(),
                "physical source chain contains an unregistered producer",
            )
        })?;
        let Some(output) = primitive
            .outputs()
            .iter()
            .find(|output| output.name.as_ref() == port)
        else {
            return Err(unsupported(
                format!("{node:?}.{port}"),
                "physical source chain names a missing output port",
            ));
        };
        // Inspect connected provenance, not the number of declared inputs. A
        // render-only combine stays inactive; a combine/mux with two physical
        // sources is rejected by resolve rather than silently picking one.
        for input in primitive
            .inputs()
            .iter()
            .filter(|input| input.ty == output.ty)
        {
            let Some(wire) = index.input(&node, input.name.as_ref())? else {
                continue;
            };
            let producer = index.by_id.get(&wire.from_node).ok_or_else(|| {
                unsupported(
                    format!("{node:?}.{}", input.name),
                    "physical source chain has no stable producer reference",
                )
            })?;
            pending.push((producer.clone(), wire.from_port.clone(), false));
        }
    }
    Ok(candidates)
}

/// Return every scene object that can be named as a physical force target.
/// Material parts are retained here; recipient deduplication is performed by
/// the compiler before per-recipient stages are cloned.
pub(super) fn authoring_objects(
    owner: &EffectGraphDef,
    scene: &SceneNodeRef,
    registry: &PrimitiveRegistry,
) -> Result<Vec<SceneNodeRef>, SceneModifierExpandError> {
    let index = FlatSceneIndex::build(owner)?;
    let mut eligible = Vec::new();
    for object in index.scene_objects(scene)? {
        if resolve(&index, &object, registry)?.is_some() {
            eligible.push(object);
        }
    }
    Ok(eligible)
}

pub(super) fn selected(
    index: &FlatSceneIndex,
    selection: &SceneTargetSelection,
    scene: &SceneNodeRef,
    registry: &PrimitiveRegistry,
) -> Result<Vec<SceneNodeRef>, SceneModifierExpandError> {
    let available = index.scene_objects(scene)?;
    let candidates = match selection {
        SceneTargetSelection::AllObjects => available,
        SceneTargetSelection::Explicit { objects } => {
            let members: BTreeSet<_> = available.iter().collect();
            let mut selected = BTreeSet::new();
            for object in objects {
                if !members.contains(object) {
                    return Err(SceneModifierExpandError::MissingTarget {
                        path: format!("{object:?}"),
                        detail: "target is not an object in the selected scene".into(),
                    });
                }
                if !selected.insert(object.clone()) {
                    return Err(SceneModifierExpandError::DuplicateIdentity {
                        path: format!("{object:?}"),
                        detail: "target is selected more than once".into(),
                    });
                }
            }
            selected.into_iter().collect()
        }
    };
    // Keep all named physical parts in the target list. The compiler uses the
    // resolved recipient key to avoid cloning a force stage twice.
    let mut result = Vec::new();
    for object in candidates {
        if resolve(index, &object, registry)?.is_some() {
            result.push(object);
        }
    }
    if result.len() > 256 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: format!("{scene:?}"),
            detail: "a modifier supports at most 256 object targets".into(),
        });
    }
    Ok(result)
}

pub(super) fn recipient_key(
    index: &FlatSceneIndex,
    object: &SceneNodeRef,
    registry: &PrimitiveRegistry,
) -> Result<Option<(SceneNodeRef, String)>, SceneModifierExpandError> {
    Ok(resolve(index, object, registry)?.map(|recipient| (recipient.node, recipient.port)))
}

/// Prepare the same physical selection for discrete hits. Several visible
/// material parts may share one body; several bodies may share one world.
/// Merge both cases before delivery so one hit reaches each body only once.
pub(crate) fn impulse_recipients(
    owner: &EffectGraphDef,
    scene: &SceneNodeRef,
    selection: &SceneTargetSelection,
    registry: &PrimitiveRegistry,
) -> Result<
    Vec<(
        manifold_core::NodeId,
        crate::node_graph::physics_events::ImpulseTarget,
    )>,
    SceneModifierExpandError,
> {
    let index = FlatSceneIndex::build(owner)?;
    impulse_recipients_with_index(&index, scene, selection, registry)
}

pub(super) fn impulse_recipients_with_index(
    index: &FlatSceneIndex,
    scene: &SceneNodeRef,
    selection: &SceneTargetSelection,
    registry: &PrimitiveRegistry,
) -> Result<
    Vec<(
        manifold_core::NodeId,
        crate::node_graph::physics_events::ImpulseTarget,
    )>,
    SceneModifierExpandError,
> {
    use crate::node_graph::physics::RigidImpulseTargets;
    use crate::node_graph::physics_events::ImpulseTarget;
    let mut worlds = std::collections::BTreeMap::new();
    for object in selected(index, selection, scene, registry)? {
        let Some(recipient) = resolve(index, &object, registry)? else {
            continue;
        };
        let node = index.node(&recipient.node)?;
        let target = if node.type_id == "node.fluid_surface" {
            ImpulseTarget::Fluid
        } else {
            let mut targets = RigidImpulseTargets::default();
            if recipient.port == "copies_acceleration" {
                targets.copies = true;
            } else {
                let slot = recipient
                    .port
                    .strip_prefix("body_acceleration_")
                    .and_then(|slot| slot.parse::<usize>().ok())
                    .filter(|&slot| slot < 64)
                    .ok_or_else(|| {
                        unsupported(&recipient.port, "invalid rigid impulse recipient")
                    })?;
                targets.bodies = 1u64 << slot;
            }
            ImpulseTarget::Rigid(targets)
        };
        let previous = worlds
            .entry(node.node_id.as_str().to_owned())
            .or_insert(target);
        if let (ImpulseTarget::Rigid(previous), ImpulseTarget::Rigid(target)) = (previous, target) {
            previous.bodies |= target.bodies;
            previous.copies |= target.copies;
        }
    }
    Ok(worlds
        .into_iter()
        .map(|(id, target)| (manifold_core::NodeId::new(id), target))
        .collect())
}
