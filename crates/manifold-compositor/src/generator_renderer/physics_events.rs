//! Content-owned manual event delivery into live generator physics.
use super::*;
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

    /// The content thread calls this only after the ordinary Fire edit succeeds.
    /// The counter is UI/undo state; this explicit call is the event producer.
    pub fn fire_scene_impulse(
        &mut self,
        layer: &Layer,
        param: &str,
        source: manifold_node_engine::exec::effect_node::FrameTime,
        project_tempo: Option<&ProjectTempo>,
    ) -> Result<bool, String> {
        if !Self::has_scene_impulse(layer, param) {
            return Ok(false);
        }
        let state = self
            .layer_generators
            .get_mut(&layer.layer_id)
            .ok_or("Impulse: render this scene before firing")?;
        let params = layer
            .gen_params()
            .ok_or("Impulse: generator parameters are unavailable")?;
        if state.override_version
            != layer
                .generator_graph()
                .map(|_| layer.generator_graph_structure_version())
            || state.event_owner.as_ref() != Some(&params.id)
            || state.generator_type != *layer.generator_type()
            || state.applied_relight != (params.relight_active(), params.relight_params)
            || state.generator.awaiting_forced_outputs_rebuild()
            || !state.generator.is_scene_impulse_param(param)
        {
            return Err("Impulse: render the changed scene before firing".into());
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
        state
            .generator
            .fire_scene_impulse(param, source, &mut self.next_physics_event)
    }

    pub fn scene_impulse_diagnostics(&self) -> manifold_core::scene_impulse::SceneImpulseDiagnostics {
        self.scene_impulse_diagnostics
    }
}
