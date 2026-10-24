//! Host parameter events backed by authored scene-space field graphs.
use ahash::{AHashMap, AHashSet};
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::scene_impulse::ImpulseFire;
use manifold_core::{Beats, NodeId, PlayedTime};

use super::{WaterRuntime, WaterRuntimeState};
use manifold_node_engine::load::expand::{SceneModifierExpandError, SceneModifierImpulseRoute};
use manifold_node_engine::{
    exec::effect_node::NodeInstanceId, exec::effect_node::ParamValues,
    persistence::PrimitiveRegistry,
};
use {
    crate::runtime::physics_impulses::CapturedSceneImpulse,
    crate::runtime::physics_impulses::PreparedSceneImpulse,
};

struct Route {
    prepared: PreparedSceneImpulse,
    captured: CapturedSceneImpulse,
}

#[derive(Default)]
pub(crate) struct SceneImpulses {
    routes: Vec<Option<Route>>,
    aliases: AHashMap<String, Vec<usize>>,
    // Inputs excluded from historical CPU sampling need a completed full frame.
    setup: Vec<(NodeInstanceId, ParamValues)>,
    setup_observed: bool,
}

fn invalid(id: &NodeId, detail: String) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidRecipe {
        path: format!("{id}.impulses"),
        detail,
    }
}

impl WaterRuntime<'_> {
    pub(crate) fn prepare_modifier_impulses(
        &mut self,
        owner: &EffectGraphDef,
        routes: &[SceneModifierImpulseRoute],
        registry: &PrimitiveRegistry,
    ) -> Result<(), SceneModifierExpandError> {
        let mut state = SceneImpulses::default();
        for (index, route) in routes.iter().enumerate() {
            let modifier = owner
                .scene_modifiers
                .iter()
                .find(|modifier| modifier.id == route.modifier_id)
                .expect("expanded modifier exists");
            let recipients = manifold_node_engine::load::expand::impulse_recipients(
                owner,
                &modifier.scene,
                &modifier.targets,
                registry,
            )?;
            state.routes.push(if recipients.is_empty() {
                None
            } else {
                let prepared = self
                    .as_ref()
                    .prepare_scene_impulse(
                        owner,
                        &modifier.scene,
                        &modifier.targets,
                        &route.field_node,
                        &route.field_port,
                        registry,
                    )
                    .map_err(|error| invalid(&modifier.id, error))?;
                let captured = prepared.new_capture();
                Some(Route { prepared, captured })
            });
            if let Some(metadata) = &owner.preset_metadata {
                for binding in &metadata.bindings {
                    if matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
                        if *modifier_id == route.modifier_id && *param_id == route.param_id)
                    {
                        let indices = state.aliases.entry(binding.id.clone()).or_default();
                        if !indices.contains(&index) {
                            indices.push(index);
                        }
                    }
                }
            }
        }
        if !routes.is_empty() {
            let sampled: AHashSet<_> = self
                .plan
                .steps()
                .iter()
                .zip(self.water.sample_steps.iter().flatten())
                .filter_map(|(step, &enabled)| enabled.then_some(step.node))
                .collect();
            let mut ancestry = AHashSet::default();
            let mut pending: Vec<_> = sampled
                .iter()
                .flat_map(|&id| self.graph.wires_into(id))
                .filter(|wire| !sampled.contains(&wire.from.0))
                .map(|wire| wire.from.0)
                .collect();
            while let Some(id) = pending.pop() {
                if !ancestry.insert(id) {
                    continue;
                }
                pending.extend(self.graph.wires_into(id).map(|wire| wire.from.0));
            }
            state.setup = ancestry
                .into_iter()
                .map(|id| {
                    (
                        id,
                        self.graph
                            .get_node(id)
                            .expect("setup ancestor exists")
                            .params
                            .clone(),
                    )
                })
                .collect();
        }
        self.water.scene_impulses = state;
        Ok(())
    }
}

impl WaterRuntimeState {
    pub(super) fn is_scene_impulse_param(&self, param: &str) -> bool {
        self.scene_impulses.aliases.contains_key(param)
    }
}

impl WaterRuntime<'_> {
    /// Capture and admit one Fire. A host alias may fan out to several
    /// fields; each is admitted or refused on its own, and nothing is kept
    /// from a refusal, so the next Fire is tried afresh. Before the scene's
    /// changed setup has rendered, or while a recipient is still preparing,
    /// nothing is consumed: the Fire answers `NotReady` and the host fires it
    /// again after the next render.
    pub fn fire_scene_impulse(
        &mut self,
        param: &str,
        source: PlayedTime,
        beats: Beats,
        next_sequence: &mut u64,
    ) -> Result<ImpulseFire, String> {
        if !self.water.is_scene_impulse_param(param) {
            return Ok(ImpulseFire::NotImpulse);
        }
        let mut state = std::mem::take(&mut self.water.scene_impulses);
        let result = (|| {
            let indices = &state.aliases[param];
            if indices.iter().any(|&index| state.routes[index].is_none()) {
                return Err("Impulse: select a simulated object in Targets before firing".into());
            }
            if !state.setup_observed
                || state.setup.iter().any(|(id, values)| {
                    self.graph
                        .get_node(*id)
                        .is_none_or(|node| node.params != *values)
                })
            {
                return Ok(ImpulseFire::NotReady);
            }
            for &index in indices {
                let route = state.routes[index].as_ref().expect("checked recipients");
                if matches!(self.impulse_recipients_ready(&route.prepared), Ok(false)) {
                    return Ok(ImpulseFire::NotReady);
                }
            }
            let end = next_sequence
                .checked_add(indices.len() as u64)
                .ok_or("Impulse: event sequence is exhausted")?;
            let first = *next_sequence;
            *next_sequence = end;
            let mut refusal = None;
            for (offset, &index) in indices.iter().enumerate() {
                let route = state.routes[index].as_mut().expect("checked recipients");
                let admitted = self
                    .impulse_recipients_ready(&route.prepared)
                    .and_then(|_| {
                        self.capture_scene_impulse_at_played(
                            &mut route.prepared,
                            &mut route.captured,
                            source,
                            beats,
                            first + offset as u64,
                        )
                    })
                    .and_then(|()| self.deliver_scene_impulse(&mut route.captured));
                route.captured.clear();
                if let Err(error) = admitted {
                    refusal.get_or_insert(error);
                }
            }
            refusal.map_or(Ok(ImpulseFire::Admitted), Err)
        })();
        self.water.scene_impulses = state;
        result
    }
}

impl super::WaterRuntimeState {
    pub(super) fn observe_impulse_setup(&mut self, graph: &manifold_node_engine::graph::Graph) {
        for (id, values) in &mut self.scene_impulses.setup {
            let node = graph.get_node(*id).expect("prepared setup ancestor");
            for (name, value) in values {
                value.clone_from(
                    node.params
                        .get(name.as_ref())
                        .expect("prepared setup parameter"),
                );
            }
        }
        self.scene_impulses.setup_observed = true;
    }

    pub(super) fn reset_modifier_impulses(&mut self) {
        self.scene_impulses.setup_observed = false;
        for route in self.scene_impulses.routes.iter_mut().flatten() {
            route
                .prepared
                .rearm(&self.impulse_identity, &mut route.captured);
        }
    }
}
