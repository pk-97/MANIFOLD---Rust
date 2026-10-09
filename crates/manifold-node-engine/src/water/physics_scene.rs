//! Native behavior for an ordered fluid/rigid pair.

use crate::exec::effect_node::{EffectNode, EffectNodeContext, NodeInstanceId};
use crate::exec::node_pairs::NodePairBehavior;
use crate::graph::Graph;
use crate::validation::GraphError;
use crate::water::node;
use crate::scene::impulse::RigidImpulseTargets;

pub(crate) struct PhysicsPair {
    colliders: RigidImpulseTargets,
}

impl NodePairBehavior for PhysicsPair {
    fn set_enabled(&self, node: &mut dyn EffectNode, enabled: bool) {
        if let Some(native) = node::get_mut(node) {
            native.set_coupled_physics(enabled);
        }
    }

    fn before_first(
        &self,
        fluid: &mut dyn EffectNode,
        rigid: &mut dyn EffectNode,
        inputs: Option<&mut EffectNodeContext<'_, '_>>,
    ) {
        let Some(ctx) = inputs else {
            if let Some(native) = node::get_mut(fluid) {
                native.set_coupled_rigid_inputs(None, self.colliders, None);
            }
            if let Some(native) = node::get_mut(rigid) {
                native.accept_coupled_rigid_frame(None);
            }
            return;
        };
        let result = match node::get_mut(rigid) {
            Some(native) => native.capture_coupled_rigid(ctx),
            None => Err("Node does not support coupled rigid input capture".into()),
        };
        if let Some(native) = node::get_mut(fluid) {
            native.set_coupled_rigid_inputs(
                node::get(rigid).and_then(|rigid| rigid.rigid_scene_observation()),
                self.colliders,
                result.as_ref().err().map(String::as_str),
            );
        }
    }

    fn after_first(&self, fluid: &dyn EffectNode, rigid: &mut dyn EffectNode) {
        if let Some(native) = node::get_mut(rigid) {
            native.accept_coupled_rigid_frame(
                node::get(fluid).and_then(|fluid| fluid.coupled_rigid_frame()),
            );
        }
    }
}

pub(crate) fn add_coupled_scene(
    graph: &mut Graph,
    fluid: NodeInstanceId,
    rigid: NodeInstanceId,
    colliders: RigidImpulseTargets,
) -> Result<(), GraphError> {
    if let Some(existing) = graph.pair_behavior_mut(fluid, rigid) {
        let pair = existing
            .as_any_mut()
            .downcast_mut::<PhysicsPair>()
            .ok_or_else(|| GraphError::CycleDetected {
                involves: vec![fluid, rigid],
            })?;
        pair.colliders.bodies |= colliders.bodies;
        pair.colliders.copies |= colliders.copies;
        for id in [fluid, rigid] {
            let instance = graph.get_node_mut(id).expect("validated coupled node");
            if let Some(native) = node::get_mut(instance.node.as_mut()) {
                native.set_coupled_physics(true);
            }
        }
        Ok(())
    } else {
        graph.add_node_pair(fluid, rigid, Box::new(PhysicsPair { colliders }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::boundary_nodes::Source;

    #[test]
    fn repeated_scene_binding_unions_colliders_without_duplicate_pair() {
        let mut graph = Graph::new();
        let fluid = graph.add_node(Box::new(Source::new()));
        let rigid = graph.add_node(Box::new(Source::new()));
        add_coupled_scene(
            &mut graph,
            fluid,
            rigid,
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        )
        .unwrap();
        add_coupled_scene(
            &mut graph,
            fluid,
            rigid,
            RigidImpulseTargets {
                bodies: 4,
                copies: true,
            },
        )
        .unwrap();
        assert_eq!(graph.node_pairs().len(), 1);
        let pair = graph.node_pairs()[0]
            .behavior
            .as_ref()
            .as_any()
            .downcast_ref::<PhysicsPair>()
            .expect("native pair");
        assert_eq!(
            pair.colliders,
            RigidImpulseTargets {
                bodies: 5,
                copies: true
            }
        );
    }
}
