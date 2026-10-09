//! Host parameter events backed by authored scene-space field graphs.
use ahash::{AHashMap, AHashSet};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};

use super::{WaterRuntime, WaterRuntimeState};
use crate::{
    exec::effect_node::FrameTime,
    water::runtime::physics_impulses::CapturedSceneImpulse,
    water::runtime::physics_impulses::PreparedSceneImpulse,
};
use crate::load::expand::{SceneModifierExpandError, SceneModifierImpulseRoute};
use crate::{exec::effect_node::NodeInstanceId, exec::effect_node::ParamValues, persistence::PrimitiveRegistry};

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
            let recipients = crate::load::expand::impulse_recipients(
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
    /// Capture one accepted manual event. Host aliases may fan out to multiple
    /// fields. Capture every field before native admission; a failed admission
    /// retains the payload and stops further Fire requests until an explicit reset.
    pub fn fire_scene_impulse(
        &mut self,
        param: &str,
        source: FrameTime,
        next_sequence: &mut u64,
    ) -> Result<bool, String> {
        if !self.water.is_scene_impulse_param(param) {
            return Ok(false);
        }
        let mut state = std::mem::take(&mut self.water.scene_impulses);
        let result = (|| {
            let indices = &state.aliases[param];
            if indices.iter().any(|&index| state.routes[index].is_none()) {
                return Err("Impulse: select a simulated object in Targets before firing".into());
            }
            if state
                .routes
                .iter()
                .flatten()
                .any(|route| route.captured.source_time().is_some())
            {
                return Err(
                    "Impulse: previous admission failed; reset the simulation before firing again"
                        .into(),
                );
            }
            if !state.setup_observed
                || state.setup.iter().any(|(id, values)| {
                    self.graph
                        .get_node(*id)
                        .is_none_or(|node| node.params != *values)
                })
            {
                return Err("Impulse: render the changed scene setup before firing".into());
            }
            let end = next_sequence
                .checked_add(indices.len() as u64)
                .ok_or("Impulse: event sequence is exhausted")?;
            let first = *next_sequence;
            *next_sequence = end;
            for (offset, &index) in indices.iter().enumerate() {
                let route = state.routes[index].as_mut().expect("checked recipients");
                if let Err(error) = self.capture_scene_impulse_at_source(
                    &mut route.prepared,
                    &mut route.captured,
                    source,
                    first + offset as u64,
                ) {
                    for route in state.routes.iter_mut().flatten() {
                        route.captured.clear();
                    }
                    return Err(error);
                }
            }
            for &index in indices {
                let route = state.routes[index].as_mut().expect("checked recipients");
                self.deliver_scene_impulse(&mut route.captured).map_err(|error| {
                    format!("{error}; captured impulse retained, reset the simulation before firing again")
                })?;
            }
            for &index in indices {
                state.routes[index]
                    .as_mut()
                    .expect("checked recipients")
                    .captured
                    .clear();
            }
            Ok(true)
        })();
        self.water.scene_impulses = state;
        result
    }

}

impl super::WaterRuntimeState {
    pub(super) fn observe_impulse_setup(&mut self, graph: &crate::graph::Graph) {
        // A native Reset parameter creates a new epoch without rebuilding
        // this graph. That explicit reset cancels any failed retained capture.
        if self
            .scene_impulses
            .routes
            .iter()
            .flatten()
            .any(|route| route.captured.has_stale_epoch(graph))
        {
            for route in self.scene_impulses.routes.iter_mut().flatten() {
                route.captured.clear();
            }
        }
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
