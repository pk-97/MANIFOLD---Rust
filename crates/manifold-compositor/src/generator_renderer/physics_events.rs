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
    /// rendered or its liquid is still preparing, and lands then, once, within
    /// 100 ms on the content clock, or is refused. A
    /// refusal is counted in the diagnostics the stage shows, and returned.
    pub fn fire_scene_impulse(
        &mut self,
        layer: &Layer,
        param: &str,
        source: FrameTime,
        project_tempo: Option<&ProjectTempo>,
    ) -> Result<bool, String> {
        self.fire_scene_impulse_with_step(layer, param, source, project_tempo, self.sim_step)
    }

    /// [`Self::fire_scene_impulse`] between frames: the step the next frame
    /// will take carries the transport's pending target and its tempo shift,
    /// peeked, so a hit after a seek in the same drain sees the seek.
    pub fn fire_scene_impulse_between_frames(
        &mut self,
        layer: &Layer,
        param: &str,
        source: FrameTime,
        project_tempo: Option<&ProjectTempo>,
        pending_target: Option<manifold_core::PlayedTime>,
        tempo_shift: manifold_core::Seconds,
    ) -> Result<bool, String> {
        let step = self
            .sim_step
            .with_played_target(pending_target)
            .with_tempo_shift(tempo_shift);
        self.fire_scene_impulse_with_step(layer, param, source, project_tempo, step)
    }

    fn fire_scene_impulse_with_step(
        &mut self,
        layer: &Layer,
        param: &str,
        source: FrameTime,
        project_tempo: Option<&ProjectTempo>,
        step: manifold_node_engine::exec::effect_node::SimStep,
    ) -> Result<bool, String> {
        if !Self::has_scene_impulse(layer, param) {
            return Ok(false);
        }
        // The seam: the hit becomes played time here, once, so a tempo edit
        // while it waits never moves it.
        let source = FireSource {
            played: step.to_played(source.seconds),
            beats: source.beats,
        };
        // Behind an earlier waiting hit of this scene, in firing order.
        let result = if self
            .waiting_fires
            .iter()
            .any(|fire| fire.layer == layer.layer_id)
        {
            self.wait(layer, param, source)
        } else {
            let state = self.layer_generators.get_mut(&layer.layer_id);
            match try_fire(
                state,
                &mut self.next_physics_event,
                layer,
                param,
                source,
                project_tempo,
                step,
            ) {
                Ok(ImpulseFire::NotReady) => self.wait(layer, param, source),
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        };
        result
            .map(|()| true)
            .inspect_err(|error| self.scene_impulse_diagnostics.refuse(error))
    }

    fn wait(&mut self, layer: &Layer, param: &str, source: FireSource) -> Result<(), String> {
        if self.waiting_fires.len() >= WAITING_FIRE_CAPACITY {
            return Err(format!(
                "Impulse: {WAITING_FIRE_CAPACITY} hits are already waiting for the scene; this hit is refused"
            ));
        }
        self.waiting_fires.push(WaitingFire {
            layer: layer.layer_id.clone(),
            param: param.to_owned(),
            source,
            since: self.content_clock,
        });
        Ok(())
    }

    /// Right after `layer` rendered: its waiting hits land in firing order,
    /// each once. The first still not ready keeps itself and the rest waiting;
    /// one that waited past [`MAX_WAIT`] is refused instead, so a late hit
    /// never lands off the beat. None lands past `frame`, the played time of
    /// the frame that just rendered: a seek back while it waited never sends
    /// it into time not yet played.
    pub(super) fn land_waiting_fires(
        &mut self,
        layer: &Layer,
        project_tempo: Option<&ProjectTempo>,
        frame: manifold_core::PlayedTime,
    ) {
        let mut index = 0;
        while index < self.waiting_fires.len() {
            if self.waiting_fires[index].layer != layer.layer_id {
                index += 1;
                continue;
            }
            if self.waiting_fires[index].waited_too_long(self.content_clock) {
                self.scene_impulse_diagnostics.refuse(WAITED_TOO_LONG);
                self.waiting_fires.remove(index);
                continue;
            }
            let fire = &self.waiting_fires[index];
            let source = FireSource {
                played: fire.source.played.min(frame),
                ..fire.source
            };
            let state = self.layer_generators.get_mut(&layer.layer_id);
            match try_fire(
                state,
                &mut self.next_physics_event,
                layer,
                &fire.param,
                source,
                project_tempo,
                self.sim_step,
            ) {
                Ok(ImpulseFire::NotReady) => return,
                Ok(_) => {}
                Err(error) => self.scene_impulse_diagnostics.refuse(&error),
            }
            self.waiting_fires.remove(index);
        }
    }

    /// End of a render pass: a hit is refused once its scene stopped (it
    /// neither rendered this pass nor was skipped behind an opaque layer while
    /// still playing), so a stopped scene never bursts stale hits later, or
    /// once it waited past [`MAX_WAIT`].
    pub(super) fn settle_waiting_fires(&mut self, render_skip: &[i32]) {
        let pass = Some(self.render_pass);
        let now = self.content_clock;
        let layers = &self.layer_generators;
        let active_clips = &self.active_clips;
        let diagnostics = &mut self.scene_impulse_diagnostics;
        self.waiting_fires.retain(|fire| {
            let rendered = layers.get(&fire.layer).is_some_and(|state| state.rendered_pass == pass);
            let occluded = active_clips
                .values()
                .any(|clip| clip.layer_id == fire.layer && render_skip.contains(&clip.layer_index));
            if !rendered && !occluded {
                diagnostics.refuse("Impulse: the scene stopped rendering before the hit could land; this hit is refused");
                return false;
            }
            if fire.waited_too_long(now) {
                diagnostics.refuse(WAITED_TOO_LONG);
                return false;
            }
            true
        });
    }

    /// Reset, Stop, project switch and export restart: every waiting hit is
    /// refused, visibly, so none lands in the next take, project or frame.
    pub(super) fn refuse_all_waiting_fires(&mut self) {
        for _ in self.waiting_fires.drain(..) {
            self.scene_impulse_diagnostics.refuse(
                "Impulse: the scene was reset before the hit could land; this hit is refused",
            );
        }
    }

    pub fn scene_impulse_diagnostics(&self) -> &SceneImpulseDiagnostics {
        &self.scene_impulse_diagnostics
    }
}

/// Hits waiting for their scene at once; one past it is refused.
pub(super) const WAITING_FIRE_CAPACITY: usize = 64;

/// The longest a hit waits for its scene, on the content clock: a
/// later hit would land off the beat, and a stuck input could fill the queue.
pub(super) const MAX_WAIT: Seconds = Seconds(0.1);

/// Frame steps arrive as f32 and sum with rounding; a microsecond keeps
/// six 60 fps frames at 100 ms.
const WAIT_TOLERANCE: f64 = 1e-6;

const WAITED_TOO_LONG: &str =
    "Impulse: the hit waited more than 100 ms for its scene; this hit is refused";

/// A Fire's source on the played clock, with the beat it fired on.
#[derive(Clone, Copy)]
pub(super) struct FireSource {
    played: manifold_core::PlayedTime,
    beats: manifold_core::Beats,
}

/// A Fire that arrived before its scene could take it.
pub(super) struct WaitingFire {
    layer: LayerId,
    param: String,
    source: FireSource,
    /// The content clock when the hit began waiting.
    since: Seconds,
}

impl WaitingFire {
    /// Whether the hit has waited past [`MAX_WAIT`] at content clock `now`.
    fn waited_too_long(&self, now: Seconds) -> bool {
        now.0 - self.since.0 > MAX_WAIT.0 + WAIT_TOLERANCE
    }
}

/// One attempt at admitting a Fire. `NotReady` consumes nothing: the scene
/// has not been built or has not rendered its changed setup, or a recipient
/// is still preparing.
fn try_fire(
    state: Option<&mut LayerGeneratorState>,
    next_sequence: &mut u64,
    layer: &Layer,
    param: &str,
    source: FireSource,
    project_tempo: Option<&ProjectTempo>,
    step: manifold_node_engine::exec::effect_node::SimStep,
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
    // This frame's step, so a Fire past a jump its frame has yet to render
    // sees the jump's target, and host hops convert with its tempo shift.
    state.generator.set_sim_step(step);
    state.generator.apply_param_values(&params.params);
    state.generator.set_source_instance(Some(params));
    state.generator.set_project_tempo(project_tempo);
    state
        .generator
        .fire_scene_impulse_played(param, source.played, source.beats, next_sequence)
}
