//! Runtime destinations resolved once from authored sources and compiler routes.

use super::value_sources::{SceneModifierValueSource, SceneModifierValueSourcePlan};
use super::{SceneModifierExpandError, SceneModifierNodeRoute};
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::{Graph, NodeInstanceId};
use ahash::AHashMap;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use std::collections::BTreeMap;

struct Destination {
    node: NodeInstanceId,
    param: String,
    enum_as_number: bool,
    baseline: ParamValue,
}

struct Write {
    source: SceneModifierValueSource,
    destinations: Vec<Destination>,
}

struct OwnerWrites {
    modifier: Option<NodeId>,
    writes: Vec<Write>,
}

/// A structural cache shared by watched and fused generator runtimes. Value
/// updates borrow the authored snapshot; they never clone or expand its graph.
pub struct PreparedGraphValueWrites {
    owners: Vec<OwnerWrites>,
}

fn invalid(detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidBinding {
        path: "preparedValueWrites".into(),
        detail: detail.into(),
    }
}

impl PreparedGraphValueWrites {
    pub fn prepare(
        owner: &EffectGraphDef,
        routes: &[SceneModifierNodeRoute],
        graph: &Graph,
        fused_retarget: &AHashMap<(String, String), (NodeId, String)>,
    ) -> Result<Self, SceneModifierExpandError> {
        let host_index = super::index::FlatSceneIndex::build(owner)?;
        let mut owners = Vec::with_capacity(1 + owner.scene_modifiers.len());
        for (modifier, local) in std::iter::once((None, owner)).chain(
            owner
                .scene_modifiers
                .iter()
                .map(|instance| (Some(&instance.id), &*instance.graph)),
        ) {
            let copies: BTreeMap<_, Vec<NodeId>> = if let Some(id) = modifier {
                routes
                    .iter()
                    .filter(|route| route.modifier_id == *id)
                    .map(|route| {
                        (
                            route.local.clone(),
                            route
                                .copies
                                .iter()
                                .map(|copy| copy.node_id.clone())
                                .collect(),
                        )
                    })
                    .collect()
            } else {
                host_index
                    .by_ref
                    .keys()
                    .map(|reference| (reference.clone(), vec![reference.node.clone()]))
                    .collect()
            };
            let mut leaf_params = BTreeMap::new();
            for (reference, generated) in &copies {
                let mut names = Vec::new();
                for node_id in generated {
                    if let Some(instance) = graph
                        .instance_by_node_id(node_id)
                        .and_then(|id| graph.get_node(id))
                    {
                        names.extend(
                            instance
                                .node
                                .parameters()
                                .iter()
                                .map(|param| param.name.to_string()),
                        );
                    } else {
                        names.extend(
                            fused_retarget
                                .keys()
                                .filter(|(id, _)| id == node_id.as_str())
                                .map(|(_, param)| param.clone()),
                        );
                    }
                }
                names.sort();
                names.dedup();
                leaf_params.insert(reference.clone(), names);
            }
            let plan = SceneModifierValueSourcePlan::prepare_with_leaf_params(local, &leaf_params)?;
            let mut writes = Vec::with_capacity(plan.entries().len());
            for source in plan.entries() {
                let generated = copies
                    .get(&source.local)
                    .ok_or_else(|| invalid(format!("missing route for {:?}", source.local)))?;
                let mut destinations = Vec::with_capacity(generated.len());
                for node_id in generated {
                    let (runtime_id, param, fused) =
                        if let Some(runtime_id) = graph.instance_by_node_id(node_id) {
                            (runtime_id, source.param.clone(), false)
                        } else {
                            let (target, field) = fused_retarget
                                .get(&(node_id.to_string(), source.param.clone()))
                                .ok_or_else(|| {
                                    invalid(format!(
                                        "missing runtime route for {node_id}.{}",
                                        source.param
                                    ))
                                })?;
                            let runtime_id = graph
                                .instance_by_node_id(target)
                                .ok_or_else(|| invalid(format!("missing fused target {target}")))?;
                            (runtime_id, field.clone(), true)
                        };
                    let parameter = graph
                        .get_node(runtime_id)
                        .and_then(|node| {
                            node.node
                                .parameters()
                                .iter()
                                .find(|definition| definition.name == param)
                        })
                        .ok_or_else(|| invalid(format!("missing parameter {node_id}.{param}")))?;
                    let baseline = graph
                        .get_node(runtime_id)
                        .and_then(|node| node.params.get(param.as_str()))
                        .cloned()
                        .ok_or_else(|| {
                            invalid(format!("missing installed value for {node_id}.{param}"))
                        })?;
                    destinations.push(Destination {
                        node: runtime_id,
                        param,
                        enum_as_number: fused && parameter.ty == ParamType::Int,
                        baseline,
                    });
                }
                writes.push(Write {
                    source: source.clone(),
                    destinations,
                });
            }
            owners.push(OwnerWrites {
                modifier: modifier.cloned(),
                writes,
            });
        }
        Ok(Self { owners })
    }

    fn local<'a>(
        owner: &'a EffectGraphDef,
        modifier: Option<&NodeId>,
    ) -> Result<&'a EffectGraphDef, SceneModifierExpandError> {
        match modifier {
            None => Ok(owner),
            Some(id) => owner
                .scene_modifiers
                .iter()
                .find(|instance| instance.id == *id)
                .map(|instance| &*instance.graph)
                .ok_or_else(|| invalid(format!("modifier {id} was removed without rebuilding"))),
        }
    }

    pub fn apply(
        &self,
        owner: &EffectGraphDef,
        graph: &mut Graph,
    ) -> Result<(), SceneModifierExpandError> {
        // Validate every source before mutating so a stale structural cache
        // cannot partially apply an edit to unrelated nodes.
        for batch in &self.owners {
            let local = Self::local(owner, batch.modifier.as_ref())?;
            for write in &batch.writes {
                write.source.value(local)?;
            }
        }
        for batch in &self.owners {
            let local = Self::local(owner, batch.modifier.as_ref())?;
            for write in &batch.writes {
                let value = write.source.value(local)?;
                for destination in &write.destinations {
                    let value = match (destination.enum_as_number, value) {
                        (true, Some(SerializedParamValue::Enum { value: index })) => {
                            ParamValue::Float(*index as f32)
                        }
                        (_, Some(value)) => value.clone().into(),
                        (_, None) => destination.baseline.clone(),
                    };
                    graph
                        .set_param(destination.node, &destination.param, value)
                        .map_err(|error| invalid(error.to_string()))?;
                }
            }
        }
        Ok(())
    }
}
