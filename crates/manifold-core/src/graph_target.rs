//! Unified target identifier for graph-editing commands.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::effect_graph_def::EffectGraphDef;
use crate::id::{EffectId, LayerId, NodeId};

/// Identifies which graph an editing command should mutate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphTarget {
    /// An effect instance's per-card graph.
    Effect(EffectId),
    /// A layer's per-layer generator graph.
    Generator(LayerId),
    /// A stable scene-modifier snapshot inside an owning graph.
    SceneModifier {
        owner: Box<GraphTarget>,
        modifier_id: NodeId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum GraphTargetWire {
    Effect {
        id: EffectId,
    },
    Generator {
        id: LayerId,
    },
    SceneModifier {
        owner: Box<GraphTargetWire>,
        #[serde(rename = "modifierId")]
        modifier_id: NodeId,
    },
}

impl From<&GraphTarget> for GraphTargetWire {
    fn from(target: &GraphTarget) -> Self {
        match target {
            GraphTarget::Effect(id) => Self::Effect { id: id.clone() },
            GraphTarget::Generator(id) => Self::Generator { id: id.clone() },
            GraphTarget::SceneModifier { owner, modifier_id } => Self::SceneModifier {
                owner: Box::new(Self::from(owner.as_ref())),
                modifier_id: modifier_id.clone(),
            },
        }
    }
}

impl From<GraphTargetWire> for GraphTarget {
    fn from(target: GraphTargetWire) -> Self {
        match target {
            GraphTargetWire::Effect { id } => Self::Effect(id),
            GraphTargetWire::Generator { id } => Self::Generator(id),
            GraphTargetWire::SceneModifier { owner, modifier_id } => Self::SceneModifier {
                owner: Box::new((*owner).into()),
                modifier_id,
            },
        }
    }
}

impl Serialize for GraphTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        GraphTargetWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GraphTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        GraphTargetWire::deserialize(deserializer).map(Into::into)
    }
}

impl GraphTarget {
    /// Returns the owning effect/generator target when this target is valid.
    /// Version one permits exactly one generator owner and a non-empty id.
    pub fn host_target(&self) -> Option<&GraphTarget> {
        match self {
            Self::Effect(_) | Self::Generator(_) => Some(self),
            Self::SceneModifier { owner, modifier_id }
                if !modifier_id.is_empty() && matches!(owner.as_ref(), Self::Generator(_)) =>
            {
                Some(owner.as_ref())
            }
            Self::SceneModifier { .. } => None,
        }
    }

    /// Select this target's graph inside an owning graph snapshot.
    pub fn graph_in<'a>(&self, owner_graph: &'a EffectGraphDef) -> Option<&'a EffectGraphDef> {
        match self {
            Self::Effect(_) | Self::Generator(_) => Some(owner_graph),
            Self::SceneModifier { modifier_id, .. } => {
                self.host_target()?;
                let mut found = None;
                for modifier in &owner_graph.scene_modifiers {
                    if &modifier.id == modifier_id {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(modifier.graph.as_ref());
                    }
                }
                found
            }
        }
    }

    /// Mutable counterpart to [`Self::graph_in`]. Missing or duplicate stable
    /// modifier ids are rejected before returning a mutable graph.
    pub fn graph_in_mut<'a>(
        &self,
        owner_graph: &'a mut EffectGraphDef,
    ) -> Option<&'a mut EffectGraphDef> {
        match self {
            Self::Effect(_) | Self::Generator(_) => Some(owner_graph),
            Self::SceneModifier { modifier_id, .. } => {
                self.host_target()?;
                let mut found = None;
                for (index, modifier) in owner_graph.scene_modifiers.iter().enumerate() {
                    if &modifier.id == modifier_id {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(index);
                    }
                }
                found.map(|index| owner_graph.scene_modifiers[index].graph.as_mut())
            }
        }
    }

    /// Short human-readable string suitable for logs and error messages.
    pub fn card_slot_for_node_param(
        &self,
        owner_graph: &EffectGraphDef,
        node_doc_id: u32,
        param_key: &str,
    ) -> Option<crate::effects::CardSlotWrite> {
        let local = self.graph_in(owner_graph)?;
        let slot = crate::effects::card_slot_for_node_param(local, node_doc_id, param_key)?;
        let Self::SceneModifier { modifier_id, .. } = self else {
            return Some(slot);
        };
        let metadata = owner_graph.preset_metadata.as_ref()?;
        let binding = metadata.bindings.iter().find(|binding| {
            matches!(&binding.target,
            crate::effect_graph_def::BindingTarget::SceneModifier { modifier_id: mid, param_id }
                if mid == modifier_id && param_id == &slot.outer_id)
        })?;
        let spec = metadata
            .params
            .iter()
            .find(|param| param.id == binding.id)?;
        Some(crate::effects::CardSlotWrite {
            outer_id: binding.id.clone(),
            min: spec.min,
            max: spec.max,
            invert: spec.invert,
            curve: spec.curve,
            scale: binding.scale * slot.scale,
            offset: binding.offset * slot.scale + slot.offset,
            authored_default: binding.default_value,
        })
    }

    /// Short human-readable string suitable for logs and error messages.
    pub fn label(&self) -> String {
        match self {
            Self::Effect(eid) => format!("effect/{}", eid.as_str()),
            Self::Generator(lid) => format!("generator/{}", lid.as_str()),
            Self::SceneModifier { owner, modifier_id } => {
                format!("{}/modifier/{}", owner.label(), modifier_id.as_str())
            }
        }
    }

    /// The preset kind this target addresses.
    pub fn preset_kind(&self) -> crate::preset_def::PresetKind {
        match self {
            Self::Effect(_) => crate::preset_def::PresetKind::Effect,
            Self::Generator(_) => crate::preset_def::PresetKind::Generator,
            Self::SceneModifier { .. } => crate::preset_def::PresetKind::SceneModifier,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphNode};
    use serde_json::json;

    fn graph(id: &str) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![EffectGraphNode {
                id: 1,
                node_id: NodeId::new(id),
                type_id: "node.value".into(),
                handle: None,
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            }],
            wires: Vec::new(),
        }
    }

    #[test]
    fn scene_modifier_target_serde_uses_explicit_shape() {
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(LayerId::new("layer"))),
            modifier_id: NodeId::new("modifier"),
        };
        let value = serde_json::to_value(&target).unwrap();
        assert_eq!(
            value,
            json!({"kind":"sceneModifier","owner":{"kind":"generator","id":"layer"},"modifierId":"modifier"})
        );
        assert_eq!(
            serde_json::from_value::<GraphTarget>(value).unwrap(),
            target
        );
    }

    #[test]
    fn base_targets_serde_with_explicit_ids() {
        let effect = GraphTarget::Effect(EffectId::new("effect"));
        let generator = GraphTarget::Generator(LayerId::new("layer"));
        assert_eq!(
            serde_json::to_value(&effect).unwrap(),
            json!({"kind":"effect","id":"effect"})
        );
        assert_eq!(
            serde_json::to_value(&generator).unwrap(),
            json!({"kind":"generator","id":"layer"})
        );
        assert_eq!(
            serde_json::from_value::<GraphTarget>(serde_json::to_value(effect).unwrap()).unwrap(),
            GraphTarget::Effect(EffectId::new("effect"))
        );
        assert_eq!(
            serde_json::from_value::<GraphTarget>(serde_json::to_value(generator).unwrap())
                .unwrap(),
            GraphTarget::Generator(LayerId::new("layer"))
        );
    }

    #[test]
    fn scene_modifier_target_rejects_invalid_owners() {
        let modifier = NodeId::new("modifier");
        let effect_owner = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Effect(EffectId::new("effect"))),
            modifier_id: modifier.clone(),
        };
        assert!(effect_owner.host_target().is_none());
        assert!(effect_owner.graph_in(&graph("host")).is_none());
        let nested_owner = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::SceneModifier {
                owner: Box::new(GraphTarget::Generator(LayerId::new("layer"))),
                modifier_id: modifier.clone(),
            }),
            modifier_id: modifier.clone(),
        };
        assert!(nested_owner.host_target().is_none());
        assert!(nested_owner.graph_in(&graph("host")).is_none());
        assert!(
            GraphTarget::SceneModifier {
                owner: Box::new(GraphTarget::Generator(LayerId::new("layer"))),
                modifier_id: NodeId::default(),
            }
            .host_target()
            .is_none()
        );
    }

    #[test]
    fn scene_modifier_target_selects_unique_snapshot_by_id() {
        let mut owner = graph("owner");
        owner
            .scene_modifiers
            .push(crate::scene_modifier_preset::SceneModifierInstanceDef {
                id: NodeId::new("same-title"),
                scene: crate::scene_modifier_preset::SceneNodeRef {
                    scope: Vec::new(),
                    node: "scene".into(),
                },
                targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
                mesh_frames: Vec::new(),
                legacy_math_view_carrier: None,
                graph: Box::new(graph("local")),
            });
        let mut other = graph("other");
        other.name = Some("Local".into());
        owner
            .scene_modifiers
            .push(crate::scene_modifier_preset::SceneModifierInstanceDef {
                id: NodeId::new("different-id"),
                scene: crate::scene_modifier_preset::SceneNodeRef {
                    scope: Vec::new(),
                    node: "scene".into(),
                },
                targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
                mesh_frames: Vec::new(),
                legacy_math_view_carrier: None,
                graph: Box::new(other),
            });
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(LayerId::new("layer"))),
            modifier_id: NodeId::new("same-title"),
        };
        assert_eq!(
            target.graph_in(&owner).unwrap().nodes[0].node_id,
            NodeId::new("local")
        );
        owner.scene_modifiers.push(owner.scene_modifiers[0].clone());
        assert!(target.graph_in(&owner).is_none());
        assert!(target.graph_in_mut(&mut owner).is_none());
    }
}
