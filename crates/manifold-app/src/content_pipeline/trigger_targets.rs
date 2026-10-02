//! Validate retained trigger identities against the current authored project.

use ahash::AHashMap;
use manifold_core::audio_trigger::fire_meter_key_for_param;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::layer::Layer;
use manifold_core::{EffectId, LayerId};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_playback::modulation::{TriggerPulse, TriggerPulseKind};

struct Owner {
    layer: Option<LayerId>,
    parameters: AHashMap<u64, TriggerPulseKind>,
    /// Fire parameters that are scene-modifier impulse aliases on this
    /// generator, keyed like `parameters`. Their events are physics inputs,
    /// not graph counters, so delivery needs the parameter id back.
    impulses: AHashMap<u64, String>,
    seen: bool,
    ambiguous: bool,
}

/// Rebuilt after edits or an input epoch boundary. Ordinary value edits reuse
/// the maps and strings; per-event membership checks allocate nothing.
#[derive(Default)]
pub(super) struct TriggerTargets {
    version: Option<(u64, u64)>,
    owners: AHashMap<EffectId, Owner>,
}

impl TriggerTargets {
    pub(super) fn refresh(&mut self, project: Option<&Project>, version: u64, epoch: u64) {
        if self.version == Some((version, epoch)) {
            return;
        }
        for owner in self.owners.values_mut() {
            owner.seen = false;
            owner.ambiguous = false;
        }
        if let Some(project) = project {
            for instance in &project.settings.master_effects {
                self.visit(instance, None, None);
            }
            for layer in &project.timeline.layers {
                if let Some(instance) = layer.gen_params() {
                    self.visit(instance, Some(&layer.layer_id), Some(layer));
                }
                if let Some(effects) = &layer.effects {
                    for instance in effects {
                        self.visit(instance, Some(&layer.layer_id), None);
                    }
                }
            }
        }
        self.owners.retain(|_, owner| owner.seen);
        self.version = Some((version, epoch));
    }

    /// `generator` is the host layer when `instance` is its generator params;
    /// only generators own scene-modifier impulse aliases.
    fn visit(
        &mut self,
        instance: &PresetInstance,
        layer: Option<&LayerId>,
        generator: Option<&Layer>,
    ) {
        if !self.owners.contains_key(&instance.id) {
            self.owners.insert(
                instance.id.clone(),
                Owner {
                    layer: layer.cloned(),
                    parameters: AHashMap::with_capacity(instance.params.len()),
                    impulses: AHashMap::new(),
                    seen: false,
                    ambiguous: false,
                },
            );
        }
        let owner = self
            .owners
            .get_mut(&instance.id)
            .expect("owner inserted above");
        if owner.seen {
            owner.ambiguous = true;
            return;
        }
        if owner.layer.as_ref() != layer {
            owner.layer = layer.cloned();
        }
        owner.parameters.clear();
        owner.impulses.clear();
        for param in instance.params.iter() {
            let kind = if param.spec.is_trigger_gate {
                TriggerPulseKind::Gate
            } else if param.spec.is_trigger {
                TriggerPulseKind::Parameter
            } else {
                continue;
            };
            let key = fire_meter_key_for_param("", &param.spec.id);
            owner.parameters.insert(key, kind);
            if kind == TriggerPulseKind::Parameter
                && generator.is_some_and(|layer| {
                    GeneratorRenderer::has_scene_impulse(layer, &param.spec.id)
                })
            {
                owner.impulses.insert(key, param.spec.id.clone());
            }
        }
        owner.seen = true;
    }

    pub(super) fn accepts(&self, pulse: &TriggerPulse) -> bool {
        self.owners.get(&pulse.owner_id).is_some_and(|owner| {
            !owner.ambiguous
                && owner.layer == pulse.layer_id
                && owner.parameters.get(&pulse.param_key) == Some(&pulse.kind)
        })
    }

    /// The scene-impulse parameter an accepted Parameter pulse fires, if any.
    /// Every surface's Fire arm lands here: the scene panel and the layer
    /// inspector edit the same generator parameter.
    pub(super) fn scene_impulse(&self, pulse: &TriggerPulse) -> Option<(&LayerId, &str)> {
        if pulse.kind != TriggerPulseKind::Parameter || !self.accepts(pulse) {
            return None;
        }
        let owner = self.owners.get(&pulse.owner_id)?;
        Some((owner.layer.as_ref()?, owner.impulses.get(&pulse.param_key)?.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::params::Param;

    fn fixture() -> (Project, TriggerPulse) {
        let mut project = Project::default();
        let mut instance = PresetInstance::new(PresetTypeId::BLOOM);
        instance.params.push(Param::bundled(ParamSpecDef {
            id: "gate".into(),
            is_trigger_gate: true,
            ..ParamSpecDef::default()
        }));
        let pulse = TriggerPulse {
            kind: manifold_playback::modulation::TriggerPulseKind::Gate,
            layer_id: None,
            owner_id: instance.id.clone(),
            param_key: fire_meter_key_for_param("", "gate"),
            audio_stamp: None,
        };
        project.settings.master_effects.push(instance);
        (project, pulse)
    }

    #[test]
    fn trigger_delivery_parameter_kind_must_match_the_authored_parameter() {
        let (mut project, mut pulse) = fixture();
        project.settings.master_effects[0].params.push(Param::bundled(ParamSpecDef {
            id: "fire".into(), is_trigger: true, ..Default::default()
        }));
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        let gate = pulse.clone();
        pulse.param_key = fire_meter_key_for_param("", "fire");
        pulse.kind = TriggerPulseKind::Parameter;
        assert!(targets.accepts(&pulse));
        assert!(targets.accepts(&gate));
        pulse.kind = TriggerPulseKind::Gate;
        assert!(!targets.accepts(&pulse));
        pulse.kind = TriggerPulseKind::Parameter;
        project.settings.master_effects[0].params.get_mut("fire").unwrap().spec.is_trigger_gate = true;
        targets.refresh(Some(&project), 2, 1);
        assert!(!targets.accepts(&pulse), "old event cannot become a different trigger kind");
        pulse.kind = TriggerPulseKind::Gate;
        assert!(targets.accepts(&pulse));
    }

    #[test]
    fn trigger_delivery_deleted_or_replaced_owner_cannot_receive_an_old_pulse() {
        let (mut project, pulse) = fixture();
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        assert!(targets.accepts(&pulse));
        project.settings.master_effects.clear();
        targets.refresh(Some(&project), 2, 1);
        assert!(!targets.accepts(&pulse));
        let (replacement, _) = fixture();
        targets.refresh(Some(&replacement), 3, 1);
        assert!(!targets.accepts(&pulse));
    }

    #[test]
    fn trigger_delivery_gate_removal_and_scope_mismatch_cancel_old_pulses() {
        let (mut project, mut pulse) = fixture();
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        pulse.layer_id = Some(LayerId::new("unrelated"));
        assert!(!targets.accepts(&pulse));
        pulse.layer_id = None;
        project.settings.master_effects[0]
            .params
            .get_mut("gate")
            .unwrap()
            .spec
            .is_trigger_gate = false;
        targets.refresh(Some(&project), 2, 1);
        assert!(!targets.accepts(&pulse));
    }

    #[test]
    fn trigger_delivery_layer_effect_and_generator_gates_keep_their_own_identities() {
        let (mut project, mut effect_pulse) = fixture();
        let effect = project.settings.master_effects.pop().unwrap();
        let mut layer = manifold_core::layer::Layer::new(
            "Generator".into(),
            manifold_core::types::LayerType::Generator,
            0,
        );
        effect_pulse.layer_id = Some(layer.layer_id.clone());
        layer.effects_mut().push(effect);
        let instance = layer.gen_params_or_init();
        instance.params.push(Param::bundled(ParamSpecDef {
            id: "gate".into(),
            is_trigger_gate: true,
            ..Default::default()
        }));
        let generator_pulse = TriggerPulse {
            owner_id: instance.id.clone(),
            ..effect_pulse.clone()
        };
        project.timeline.layers.push(layer);
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        assert!(targets.accepts(&effect_pulse));
        assert!(targets.accepts(&generator_pulse));
        project.timeline.layers.clear();
        targets.refresh(Some(&project), 2, 1);
        assert!(!targets.accepts(&effect_pulse));
        assert!(!targets.accepts(&generator_pulse));
    }

    /// A scene force's Fire armed to a kick resolves to its physics impulse.
    /// The arm goes through EditingService exactly as the card's audio drawer
    /// does; the kick is a synthetic retained hop on the real modulation walk.
    #[test]
    fn scene_force_fire_audio_kick_resolves_to_the_scene_impulse() {
        use manifold_core::audio_features::{
            AudioFeatureHop, AudioFeatureSnapshot, AudioHopBatch, AudioHopStamp, SendFeatures,
        };
        use manifold_core::audio_mod::{
            AudioBand, AudioFeature, AudioFeatureKind, AudioModShape, ParameterAudioMod,
        };
        use manifold_core::audio_setup::AudioSend;
        use manifold_core::audio_trigger::{FireMeterCapture, TriggerFireMode};
        use manifold_core::effect_graph_def::BindingTarget;
        use manifold_core::{Beats, Seconds};
        use manifold_editing::commands::audio_mod::AddAudioModCommand;
        use manifold_editing::commands::effect_target::DriverTarget;
        use manifold_editing::service::EditingService;

        let (mut project, layer_id) = crate::scene_modifier_edit::tests::project_with_mushroom();
        let mut editing = EditingService::new();
        let modifier = crate::scene_modifier_edit::tests::apply_stock(
            &mut editing, &mut project, &layer_id, "RadialForce",
        );
        let layer = project.timeline.find_layer_by_id(&layer_id).unwrap().1;
        let fire = layer
            .generator_graph()
            .and_then(|graph| graph.preset_metadata.as_ref())
            .unwrap()
            .bindings
            .iter()
            .find_map(|binding| match &binding.target {
                BindingTarget::SceneModifier { modifier_id, param_id }
                    if *modifier_id == modifier && param_id == "fire" => Some(binding.id.clone()),
                _ => None,
            })
            .expect("Fire is bound on the host generator");
        let owner = layer.gen_params().unwrap().id.clone();

        let send = AudioSend::new("Audio 1");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let mut arm = ParameterAudioMod::new(
            fire.clone().into(),
            send_id,
            AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Full),
        );
        arm.trigger_mode = Some(TriggerFireMode::Transient);
        arm.shape = AudioModShape {
            sensitivity: 1.83,
            attack_ms: 0.0,
            release_ms: 819.0,
            ..Default::default()
        };
        editing.execute(
            Box::new(AddAudioModCommand::new(
                DriverTarget::GeneratorParam { layer_id: layer_id.clone() },
                arm,
            )),
            &mut project,
        );
        assert!(editing.take_rejection().is_none());

        let mut features = SendFeatures::default();
        features.bands[AudioBand::Low.index()].kick = 1.0;
        let mut snapshot = AudioFeatureSnapshot { sends: vec![features], ..Default::default() };
        let mut batch = AudioHopBatch::with_capacity(1);
        batch.begin(1);
        batch
            .push(AudioFeatureHop {
                stamp: AudioHopStamp {
                    epoch: 1,
                    end_sample: 512,
                    sample_rate: 48_000,
                    source_time: None,
                    timeline_time: None,
                },
                dt: Seconds(512.0 / 48_000.0),
                features,
            })
            .unwrap();
        snapshot.hop_batches.push(batch);

        let mut pulses: Vec<TriggerPulse> = Vec::new();
        manifold_playback::modulation::evaluate_modulation(
            &mut project,
            Beats(0.0),
            Seconds::ZERO,
            Seconds(1.0 / 60.0),
            &snapshot,
            &mut Vec::new(),
            &mut pulses,
            &[],
            &mut FireMeterCapture::default(),
        );
        assert_eq!(pulses.len(), 1, "one kick fires the armed Fire once");
        let pulse = &pulses[0];
        assert_eq!(pulse.owner_id, owner);

        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        assert_eq!(
            targets.scene_impulse(pulse),
            Some((&layer_id, fire.as_str())),
            "the kick must reach the scene impulse producer, not stop at the counter"
        );
    }

    #[test]
    fn trigger_delivery_ambiguous_owner_ids_do_not_broadcast() {
        let (mut project, pulse) = fixture();
        project
            .settings
            .master_effects
            .push(project.settings.master_effects[0].clone());
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        assert!(!targets.accepts(&pulse));
    }

    #[test]
    fn trigger_delivery_new_epoch_rebuilds_targets_even_at_the_same_edit_version() {
        let (project, pulse) = fixture();
        let mut targets = TriggerTargets::default();
        targets.refresh(Some(&project), 1, 1);
        let gates_capacity = targets
            .owners
            .get(&pulse.owner_id)
            .unwrap()
            .parameters
            .capacity();
        targets.refresh(Some(&project), 2, 1);
        assert_eq!(
            targets
                .owners
                .get(&pulse.owner_id)
                .unwrap()
                .parameters
                .capacity(),
            gates_capacity
        );
        targets.refresh(Some(&Project::default()), 2, 2);
        assert!(!targets.accepts(&pulse));
    }
}
