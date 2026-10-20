//! Content-owned manual event delivery into live generator physics.
use super::*;
use manifold_core::scene_impulse::{ImpulseFire, SceneImpulseDiagnostics};
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::runtime::preset_context::ProjectTempo;

impl GeneratorRenderer {
    pub fn has_scene_impulse(layer: &Layer, param: &str) -> bool {
        layer.generator_graph().is_some_and(|owner| {
            owner.preset_metadata.as_ref().is_some_and(|metadata| {
                metadata.bindings.iter().any(|binding| {
                    if binding.id != param {
                        return false;
                    }
                    let manifold_core::effect_graph_def::BindingTarget::SceneModifier {
                        modifier_id,
                        param_id,
                    } = &binding.target
                    else {
                        return false;
                    };
                    owner
                        .scene_modifiers
                        .iter()
                        .find(|modifier| modifier.id == *modifier_id)
                        .and_then(|modifier| modifier.graph.preset_metadata.as_ref())
                        .and_then(|metadata| metadata.scene_modifier.as_ref())
                        .is_some_and(|recipe| {
                            recipe
                                .impulses
                                .iter()
                                .any(|impulse| impulse.param_id == *param_id)
                        })
                })
            })
        })
    }

    /// Fire one scene impulse; the content thread calls this only after the
    /// ordinary Fire edit succeeds (the counter is UI/undo state, this is the
    /// event producer). Triggers are never skipped: the hit lands now, or it
    /// waits for this scene's next render when its changed setup has not
    /// rendered or its liquid is still preparing, and lands then, once. A
    /// refusal is counted in the diagnostics the stage shows, and returned.
    pub fn fire_scene_impulse(
        &mut self,
        layer: &Layer,
        param: &str,
        source: FrameTime,
        project_tempo: Option<&ProjectTempo>,
    ) -> Result<bool, String> {
        if !Self::has_scene_impulse(layer, param) {
            return Ok(false);
        }
        // Behind an earlier waiting hit of this scene, in firing order.
        let result = if self.waiting_fires.iter().any(|fire| fire.layer == layer.layer_id) {
            self.wait(layer, param, source)
        } else {
            let state = self.layer_generators.get_mut(&layer.layer_id);
            match try_fire(state, &mut self.next_physics_event, layer, param, source, project_tempo) {
                Ok(ImpulseFire::NotReady) => self.wait(layer, param, source),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        };
        result.map(|()| true).inspect_err(|error| self.scene_impulse_diagnostics.refuse(error))
    }

    fn wait(&mut self, layer: &Layer, param: &str, source: FrameTime) -> Result<(), String> {
        if self.waiting_fires.len() >= WAITING_FIRE_CAPACITY {
            return Err(format!(
                "Impulse: {WAITING_FIRE_CAPACITY} hits are already waiting for the scene; this hit is refused"
            ));
        }
        self.waiting_fires.push(WaitingFire { layer: layer.layer_id.clone(), param: param.to_owned(), source });
        Ok(())
    }

    /// Right after `layer` rendered: its waiting hits land in firing order,
    /// each once. The first still not ready keeps itself and the rest waiting.
    pub(super) fn land_waiting_fires(&mut self, layer: &Layer, project_tempo: Option<&ProjectTempo>) {
        let mut index = 0;
        while index < self.waiting_fires.len() {
            if self.waiting_fires[index].layer != layer.layer_id {
                index += 1;
                continue;
            }
            let fire = &self.waiting_fires[index];
            let state = self.layer_generators.get_mut(&layer.layer_id);
            match try_fire(state, &mut self.next_physics_event, layer, &fire.param, fire.source, project_tempo) {
                Ok(ImpulseFire::NotReady) => return,
                Ok(_) => {}
                Err(error) => self.scene_impulse_diagnostics.refuse(&error),
            }
            self.waiting_fires.remove(index);
        }
    }

    /// End of a render pass: a hit whose scene did not render this pass is
    /// refused, so a scene that stops playing never bursts stale hits later.
    pub(super) fn refuse_unrendered_waiting_fires(&mut self) {
        let pass = Some(self.render_pass);
        let layers = &self.layer_generators;
        let diagnostics = &mut self.scene_impulse_diagnostics;
        self.waiting_fires.retain(|fire| {
            let rendered = layers.get(&fire.layer).is_some_and(|state| state.rendered_pass == pass);
            if !rendered {
                diagnostics.refuse("Impulse: the scene stopped rendering before the hit could land; this hit is refused");
            }
            rendered
        });
    }

    pub fn scene_impulse_diagnostics(&self) -> &SceneImpulseDiagnostics {
        &self.scene_impulse_diagnostics
    }
}

/// Hits waiting for their scene at once; one past it is refused.
pub(super) const WAITING_FIRE_CAPACITY: usize = 64;

/// A Fire that arrived before its scene could take it.
pub(super) struct WaitingFire {
    layer: LayerId,
    param: String,
    source: FrameTime,
}

/// One attempt at admitting a Fire. `NotReady` consumes nothing: the scene
/// has not been built or has not rendered its changed setup, or a recipient
/// is still preparing.
fn try_fire(
    state: Option<&mut LayerGeneratorState>,
    next_sequence: &mut u64,
    layer: &Layer,
    param: &str,
    source: FrameTime,
    project_tempo: Option<&ProjectTempo>,
) -> Result<ImpulseFire, String> {
    let Some(state) = state else {
        return Ok(ImpulseFire::NotReady);
    };
    let params = layer
        .gen_params()
        .ok_or("Impulse: generator parameters are unavailable")?;
    if state.override_version
        != layer
            .generator_graph()
            .map(|_| layer.generator_graph_structure_version())
        || state.event_owner.as_ref() != Some(&params.id)
        || state.generator_type != *layer.generator_type()
        || state.generator.awaiting_forced_outputs_rebuild()
        || !state.generator.is_scene_impulse_param(param)
    {
        return Ok(ImpulseFire::NotReady);
    }
    let version = Some(layer.generator_graph_version());
    if state.applied_param_version != version {
        if let Some(def) = layer.generator_graph() {
            state.generator.apply_inner_param_overrides(def);
        }
        state
            .generator
            .apply_manifest_reshape(&params.params, layer.generator_graph());
        state.applied_param_version = version;
    }
    state.generator.apply_param_values(&params.params);
    state.generator.set_source_instance(Some(params));
    state.generator.set_project_tempo(project_tempo);
    state.generator.fire_scene_impulse(param, source, next_sequence)
}
