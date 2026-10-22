//! Resolve scene objects to the physical input that owns their acceleration.

use std::collections::BTreeSet;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::{is_liquid_domain, liquid_domain_of};
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::SceneTargetSelection;

use crate::persistence::PrimitiveRegistry;
use manifold_core::physics_members::{
    ACCELERATION_ALIAS, BODY_TYPE_ID, Family, MEMBER_OUTPUT, WORLD_TYPE_ID, is_alias_output,
    member_side_output, world_member,
};

use super::SceneModifierExpandError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Recipient {
    pub object: NodeId,
    pub node: NodeId,
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
    object: &NodeId,
    registry: &PrimitiveRegistry,
) -> Result<Option<Recipient>, SceneModifierExpandError> {
    let mut candidates = Vec::new();
    if let Some(domain) = liquid_domain_of(index, object)? {
        let type_id = &index.node(&domain)?.type_id;
        let takes_forces = registry.construct(type_id).is_some_and(|primitive| {
            primitive
                .inputs()
                .iter()
                .any(|input| input.name.as_ref() == LIQUID_ACCELERATION_PORT)
        });
        if takes_forces {
            candidates.push(Candidate {
                node: domain,
                port: LIQUID_ACCELERATION_PORT.into(),
            });
        }
    }
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

/// The liquid domain input that receives scene forces. A domain that does not
/// declare it yet is still water (pairing finds it) but takes no forces.
const LIQUID_ACCELERATION_PORT: &str = "acceleration_field";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    node: NodeId,
    port: String,
}

fn trace(
    index: &FlatSceneIndex,
    start: &NodeId,
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
        if def.type_id == BODY_TYPE_ID && is_alias_output(&port) {
            // A body's pose, instances and count come from its world: forces
            // enter at the body's acceleration input, but only once the body
            // is wired into a world slot of the matching family.
            let in_world = index.by_ref.get(&node).is_some_and(|flat| {
                world_member(&index.flat.nodes, &index.flat.wires, *flat)
                    .is_some_and(|member| member.family.world_output(&port, member.k).is_some())
            });
            if in_world {
                candidates.push(Candidate {
                    node,
                    port: ACCELERATION_ALIAS.into(),
                });
            }
            continue;
        }
        // A graph already compiled to world slots reaches a body through its
        // world's numbered output: the body wired into that slot is the source.
        if def.type_id == WORLD_TYPE_ID
            && let Some((family, k)) = member_side_output(&port)
            && let Some(body) = index.flat.wires.iter().find(|wire| {
                wire.to_node == index.by_ref.get(&node).copied().unwrap_or(u32::MAX)
                    && wire.to_port == family.member_port(k)
                    && wire.from_port == MEMBER_OUTPUT
            })
            && let Some(body) = index.by_id.get(&body.from_node)
        {
            candidates.push(Candidate { node: body.clone(), port: ACCELERATION_ALIAS.into() });
            continue;
        }
        let primitive = registry.construct(&def.type_id).ok_or_else(|| {
            unsupported(
                def.type_id.clone(),
                "physical source chain contains an unregistered producer",
            )
        })?;
        // A node that declares where scene forces enter (a swarm's forces
        // node) ends the trace there.
        if let Some(input) = primitive.acceleration_input() {
            candidates.push(Candidate { node, port: input.into() });
            continue;
        }
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
        // A node may name the inputs its output comes from (copies from
        // particles); otherwise they are the inputs of the output's type. A
        // state capture is last frame's loop, never a new source.
        let sources = primitive.physical_sources(port.as_str());
        let captures = primitive.state_capture_input_ports();
        for input in primitive.inputs().iter().filter(|input| {
            let name = input.name.as_ref();
            sources.map_or(input.ty == output.ty, |sources| sources.contains(&name)) && !captures.contains(&name)
        }) {
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
    scene: &NodeId,
    registry: &PrimitiveRegistry,
) -> Result<Vec<NodeId>, SceneModifierExpandError> {
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
    scene: &NodeId,
    registry: &PrimitiveRegistry,
) -> Result<Vec<NodeId>, SceneModifierExpandError> {
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
    object: &NodeId,
    registry: &PrimitiveRegistry,
) -> Result<Option<(NodeId, String)>, SceneModifierExpandError> {
    Ok(resolve(index, object, registry)?.map(|recipient| (recipient.node, recipient.port)))
}

/// Prepare the same physical selection for discrete hits. Several visible
/// material parts may share one body; several bodies may share one world.
/// Merge both cases before delivery so one hit reaches each body only once.
pub fn impulse_recipients(
    owner: &EffectGraphDef,
    scene: &NodeId,
    selection: &SceneTargetSelection,
    registry: &PrimitiveRegistry,
) -> Result<
    Vec<(
        manifold_core::NodeId,
        manifold_core::scene_impulse::ImpulseTarget,
    )>,
    SceneModifierExpandError,
> {
    let index = FlatSceneIndex::build(owner)?;
    impulse_recipients_with_index(&index, scene, selection, registry)
}

pub fn impulse_recipients_with_index(
    index: &FlatSceneIndex,
    scene: &NodeId,
    selection: &SceneTargetSelection,
    registry: &PrimitiveRegistry,
) -> Result<
    Vec<(
        manifold_core::NodeId,
        manifold_core::scene_impulse::ImpulseTarget,
    )>,
    SceneModifierExpandError,
> {
    use manifold_core::scene_impulse::{ImpulseTarget, RigidImpulseTargets};
    let mut worlds = std::collections::BTreeMap::new();
    for object in selected(index, selection, scene, registry)? {
        let Some(recipient) = resolve(index, &object, registry)? else {
            continue;
        };
        let node = index.node(&recipient.node)?;
        // A body's hit goes to its world, on its own slot or copy group.
        if node.type_id == BODY_TYPE_ID {
            let member = index
                .by_ref
                .get(&recipient.node)
                .and_then(|flat| world_member(&index.flat.nodes, &index.flat.wires, *flat))
                .ok_or_else(|| unsupported(&recipient.port, "rigid body is not in a Physics World"))?;
            let world = index
                .flat
                .nodes
                .iter()
                .find(|candidate| candidate.id == member.world)
                .ok_or_else(|| unsupported(&recipient.port, "rigid body world is missing"))?;
            let slot = member.k as usize;
            let targets = match member.family {
                Family::Body => RigidImpulseTargets::body(slot),
                Family::Copies => RigidImpulseTargets::group(slot),
            }
            .ok_or_else(|| unsupported(&recipient.port, "invalid rigid impulse recipient"))?;
            let previous = worlds
                .entry(world.node_id.as_str().to_owned())
                .or_insert_with(|| ImpulseTarget::Rigid(RigidImpulseTargets::default()));
            if let ImpulseTarget::Rigid(previous) = previous {
                *previous = std::mem::take(previous).union(targets);
            }
            continue;
        }
        // A node that declares where scene forces enter (a swarm's forces
        // node) takes hits there too, on everything it moves.
        let whole = registry
            .construct(&node.type_id)
            .and_then(|primitive| primitive.acceleration_input())
            .is_some_and(|input| input == recipient.port);
        let target = if is_liquid_domain(&node.type_id) {
            ImpulseTarget::Fluid
        } else if whole {
            ImpulseTarget::Swarm
        } else {
            return Err(unsupported(&recipient.port, "invalid rigid impulse recipient"));
        };
        worlds.entry(node.node_id.as_str().to_owned()).or_insert(target);
    }
    Ok(worlds
        .into_iter()
        .map(|(id, target)| (manifold_core::NodeId::new(id), target))
        .collect())
}
