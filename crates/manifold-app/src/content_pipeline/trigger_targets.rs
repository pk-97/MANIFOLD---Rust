//! Validate retained gate identities against the current authored project.

use ahash::{AHashMap, AHashSet};
use manifold_core::audio_trigger::fire_meter_key_for_param;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::{EffectId, LayerId};
use manifold_playback::modulation::TriggerPulse;

struct Owner {
    layer: Option<LayerId>,
    gates: AHashSet<u64>,
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
                self.visit(instance, None);
            }
            for layer in &project.timeline.layers {
                if let Some(instance) = layer.gen_params() {
                    self.visit(instance, Some(&layer.layer_id));
                }
                if let Some(effects) = &layer.effects {
                    for instance in effects {
                        self.visit(instance, Some(&layer.layer_id));
                    }
                }
            }
        }
        self.owners.retain(|_, owner| owner.seen);
        self.version = Some((version, epoch));
    }

    fn visit(&mut self, instance: &PresetInstance, layer: Option<&LayerId>) {
        if !self.owners.contains_key(&instance.id) {
            self.owners.insert(
                instance.id.clone(),
                Owner {
                    layer: layer.cloned(),
                    gates: AHashSet::with_capacity(instance.params.len()),
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
        owner.gates.clear();
        for param in instance
            .params
            .iter()
            .filter(|param| param.spec.is_trigger_gate)
        {
            owner
                .gates
                .insert(fire_meter_key_for_param("", &param.spec.id));
        }
        owner.seen = true;
    }

    pub(super) fn accepts(&self, pulse: &TriggerPulse) -> bool {
        self.owners.get(&pulse.owner_id).is_some_and(|owner| {
            !owner.ambiguous
                && owner.layer == pulse.layer_id
                && owner.gates.contains(&pulse.param_key)
        })
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
            layer_id: None,
            owner_id: instance.id.clone(),
            param_key: fire_meter_key_for_param("", "gate"),
            audio_stamp: None,
        };
        project.settings.master_effects.push(instance);
        (project, pulse)
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
            .gates
            .capacity();
        targets.refresh(Some(&project), 2, 1);
        assert_eq!(
            targets
                .owners
                .get(&pulse.owner_id)
                .unwrap()
                .gates
                .capacity(),
            gates_capacity
        );
        targets.refresh(Some(&Project::default()), 2, 2);
        assert!(!targets.accepts(&pulse));
    }
}
