//! Native behavior for the ordered pairs of a water scene: a fluid and the
//! rigid world it couples, that fluid and each other fluid or gas taking its
//! solids, and a free-running world and each gas taking its solids.

use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_gas::primitives::gas_volume::GasVolume;
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, NodeInstanceId};
use manifold_node_engine::exec::node_pairs::NodePairBehavior;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::validation::GraphError;
use manifold_water_gpu_flip::primitives::gpu_flip_domain::GpuFlipDomain;
use manifold_water_gpu_mpm::primitives::matter_domain::MatterDomain;
use manifold_water_liquid::coupling::SharesSolids;
use manifold_water_rigid::node;
use manifold_water_rigid::node::PhysicsNode;
use manifold_water_rigid::primitives::physics_world::PhysicsWorldNode;

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
        step: manifold_node_engine::exec::effect_node::SimStep,
    ) {
        let Some(ctx) = inputs else {
            if let Some(native) = node::get_mut(fluid) {
                native.set_coupled_rigid_inputs(
                    None,
                    self.colliders.clone(),
                    None,
                    step.authored_sample_only,
                );
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
                self.colliders.clone(),
                result.as_ref().err().map(String::as_str),
                step.authored_sample_only,
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

/// One water's solids handed to another in its scene, right after the owner
/// water runs: the other water runs after it and flows round the same
/// Colliders and Bodies.
pub(crate) struct SharedSolidsPair;

fn shares_solids(node: &dyn EffectNode) -> Option<&dyn SharesSolids> {
    let any = node.as_any();
    any.downcast_ref::<GpuFlipDomain>()
        .map(|domain| domain as &dyn SharesSolids)
        .or_else(|| {
            any.downcast_ref::<MatterDomain>()
                .map(|domain| domain as &dyn SharesSolids)
        })
}

fn shares_solids_mut(node: &mut dyn EffectNode) -> Option<&mut dyn SharesSolids> {
    let any = node.as_any_mut();
    if any.is::<GpuFlipDomain>() {
        return any
            .downcast_mut::<GpuFlipDomain>()
            .map(|domain| domain as &mut dyn SharesSolids);
    }
    if any.is::<GasVolume>() {
        return any
            .downcast_mut::<GasVolume>()
            .map(|gas| gas as &mut dyn SharesSolids);
    }
    any.downcast_mut::<MatterDomain>()
        .map(|domain| domain as &mut dyn SharesSolids)
}

impl NodePairBehavior for SharedSolidsPair {
    fn set_enabled(&self, _node: &mut dyn EffectNode, _enabled: bool) {}

    /// Before the owner settles its pending tick, hand it what the other
    /// liquid pushed the bodies over its last frame.
    fn before_first(
        &self,
        owner: &mut dyn EffectNode,
        water: &mut dyn EffectNode,
        _inputs: Option<&mut EffectNodeContext<'_, '_>>,
        _step: manifold_node_engine::exec::effect_node::SimStep,
    ) {
        let Some(push) = shares_solids_mut(water).and_then(SharesSolids::take_observed_push)
        else {
            return;
        };
        if let Some(owner) = shares_solids_mut(owner).and_then(SharesSolids::rigid_owner_mut) {
            owner.add_observed_push(push);
        }
    }

    fn after_first(&self, owner: &dyn EffectNode, water: &mut dyn EffectNode) {
        let owner = shares_solids(owner).and_then(SharesSolids::rigid_owner);
        if let Some(water) = shares_solids_mut(water) {
            let pushes = water.pushes_bodies();
            water.shared_solids_mut().copy_from(owner, pushes);
        }
    }
}

/// A free-running World's solids handed to a Smoke or Fire right after the
/// World runs, when no water owns it.
pub(crate) struct WorldSolidsPair;

impl NodePairBehavior for WorldSolidsPair {
    fn set_enabled(&self, _node: &mut dyn EffectNode, _enabled: bool) {}

    fn before_first(
        &self,
        _world: &mut dyn EffectNode,
        _gas: &mut dyn EffectNode,
        _inputs: Option<&mut EffectNodeContext<'_, '_>>,
        _step: manifold_node_engine::exec::effect_node::SimStep,
    ) {
    }

    fn after_first(&self, world: &dyn EffectNode, gas: &mut dyn EffectNode) {
        let Some(gas) = gas.as_any_mut().downcast_mut::<GasVolume>() else {
            return;
        };
        let solids = gas.shared_solids_mut();
        let world = world.as_any().downcast_ref::<PhysicsWorldNode>();
        match world.and_then(|world| Some((world, world.rigid_scene_observation()?))) {
            Some((world, observation)) => {
                solids.copy_from_world(&world.simulation, &observation.inputs)
            }
            None => solids.copy_from(None, false),
        }
    }
}

/// Hand `world`'s solids to `gas` every frame.
pub(crate) fn add_world_solids(
    graph: &mut Graph,
    world: NodeInstanceId,
    gas: NodeInstanceId,
) -> Result<(), GraphError> {
    if graph.pair_behavior_mut(world, gas).is_some() {
        return Ok(());
    }
    graph.add_node_pair(world, gas, Box::new(WorldSolidsPair))
}

/// Hand `owner`'s solids to `water` every frame.
pub(crate) fn add_shared_solids(
    graph: &mut Graph,
    owner: NodeInstanceId,
    water: NodeInstanceId,
) -> Result<(), GraphError> {
    if graph.pair_behavior_mut(owner, water).is_some() {
        return Ok(());
    }
    graph.add_node_pair(owner, water, Box::new(SharedSolidsPair))
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
        pair.colliders = std::mem::take(&mut pair.colliders).union(colliders);
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
    use manifold_node_engine::scene::boundary_nodes::Source;

    #[test]
    fn repeated_scene_binding_unions_colliders_without_duplicate_pair() {
        let mut graph = Graph::new();
        let fluid = graph.add_node(Box::new(Source::new()));
        let rigid = graph.add_node(Box::new(Source::new()));
        add_coupled_scene(
            &mut graph,
            fluid,
            rigid,
            RigidImpulseTargets::body(0).unwrap(),
        )
        .unwrap();
        add_coupled_scene(
            &mut graph,
            fluid,
            rigid,
            RigidImpulseTargets::body(2)
                .unwrap()
                .union(RigidImpulseTargets::group(0).unwrap()),
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
            RigidImpulseTargets::body(0)
                .unwrap()
                .union(RigidImpulseTargets::body(2).unwrap())
                .union(RigidImpulseTargets::group(0).unwrap())
        );
    }
}
