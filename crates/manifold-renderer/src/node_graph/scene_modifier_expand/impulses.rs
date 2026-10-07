//! Prepared routes for event-only scene-modifier field outputs.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::node_graph::{Graph, PortType};

use super::{SceneModifierExpandError, SceneModifierNodeRoute};

/// A generated vector-field output owned by one declared scene-modifier impulse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneModifierImpulseRoute {
    pub modifier_id: NodeId,
    pub param_id: String,
    pub field_node: NodeId,
    pub field_port: String,
}

/// Validate and retain generated field outputs referenced by authored impulse
/// declarations. The live graph is the output type authority.
pub(super) fn prepare(
    owner: &EffectGraphDef,
    routes: &[SceneModifierNodeRoute],
    graph: &mut Graph,
) -> Result<Vec<SceneModifierImpulseRoute>, SceneModifierExpandError> {
    let mut prepared = Vec::new();
    for instance in &owner.scene_modifiers {
        let Some(metadata) = instance.graph.preset_metadata.as_ref() else {
            return Err(invalid(
                instance.id.to_string(),
                "modifier instance has no metadata",
            ));
        };
        let Some(recipe) = metadata.scene_modifier.as_ref() else {
            return Err(invalid(
                instance.id.to_string(),
                "modifier instance has no recipe",
            ));
        };
        for (index, impulse) in recipe.impulses.iter().enumerate() {
            let path = format!("{}.impulses[{index}]", instance.id);
            let matching: Vec<_> = routes
                .iter()
                .filter(|route| route.modifier_id == instance.id && route.local == impulse.field)
                .collect();
            let [route] = matching.as_slice() else {
                return Err(missing(
                    format!("{path}.field"),
                    if matching.is_empty() {
                        "impulse field does not name a generated local node"
                    } else {
                        "impulse field is ambiguous across generated local routes"
                    },
                ));
            };
            let [copy] = route.copies.as_slice() else {
                return Err(invalid(
                    format!("{path}.field"),
                    "impulse field must resolve to exactly one scene-scope copy",
                ));
            };
            if copy.object.is_some() {
                return Err(invalid(
                    format!("{path}.field"),
                    "impulse field cannot source an eachObject stage",
                ));
            }
            let node_instance = graph.instance_by_node_id(&copy.node_id).ok_or_else(|| {
                missing(
                    copy.node_id.to_string(),
                    "generated impulse field node is absent",
                )
            })?;
            let node = graph.get_node(node_instance).ok_or_else(|| {
                missing(copy.node_id.to_string(), "generated field node is absent")
            })?;
            let output = node
                .node
                .outputs()
                .iter()
                .find(|output| output.name == impulse.port)
                .ok_or_else(|| {
                    missing(
                        format!("{path}.port"),
                        format!("generated node has no output port '{}'", impulse.port),
                    )
                })?;
            if output.ty != PortType::VectorField {
                return Err(invalid(
                    format!("{path}.port"),
                    format!("impulse source must be a VectorField, got {:?}", output.ty),
                ));
            }
            graph
                .add_external_output(node_instance, &impulse.port)
                .map_err(|error| {
                    invalid(
                        format!("{path}.port"),
                        format!("failed to retain impulse field output: {error}"),
                    )
                })?;
            prepared.push(SceneModifierImpulseRoute {
                modifier_id: instance.id.clone(),
                param_id: impulse.param_id.clone(),
                field_node: copy.node_id.clone(),
                field_port: impulse.port.clone(),
            });
        }
    }
    Ok(prepared)
}

fn missing(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: path.into(),
        detail: detail.into(),
    }
}

fn invalid(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: path.into(),
        detail: detail.into(),
    }
}
