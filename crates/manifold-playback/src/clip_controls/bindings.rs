//! Runtime event cutoffs for the parameter's authoritative source assignment.

use ahash::AHashMap;
use manifold_core::effects::PresetInstance;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::{Beats, EffectId, LayerId};

use super::{ClipControlFrame, ClipControlStart};

#[derive(Clone, Copy, Debug)]
struct EventCutoff {
    beat: Beats,
    sequence: u64,
}

#[derive(Debug)]
struct Binding {
    source: Option<LayerId>,
    cutoff: Option<EventCutoff>,
    seen: bool,
}

#[derive(Debug, Default)]
pub(super) struct ClipControlBindings {
    instances: AHashMap<EffectId, AHashMap<String, Binding>>,
    initialized: bool,
}

/// A borrowed event view: filtering a new assignment never copies source facts.
#[derive(Clone, Copy, Default)]
pub(crate) struct ClipControlStarts<'a> {
    starts: &'a [ClipControlStart],
    cutoff: Option<EventCutoff>,
}

impl<'a> ClipControlStarts<'a> {
    pub(crate) fn iter(self) -> impl Iterator<Item = &'a ClipControlStart> + Clone {
        self.starts.iter().filter(move |start| {
            self.cutoff
                .is_none_or(|cutoff| start.sequence >= cutoff.sequence && start.beat >= cutoff.beat)
        })
    }

    pub(crate) fn len(self) -> usize {
        self.iter().count()
    }
}

impl ClipControlFrame {
    /// Observe authored routes at the edit boundary, before the transport moves
    /// or another source event is published. Only derived cursors live here;
    /// project serialization and undo continue to own the parameter assignment.
    pub(crate) fn reconcile_bindings(
        &mut self,
        project: &Project,
        beat: Beats,
        mut source_changed: impl FnMut(&EffectId, &str),
    ) {
        for instance in self.bindings.instances.values_mut() {
            for binding in instance.values_mut() {
                binding.seen = false;
            }
        }
        self.retain_sources(|id| project.timeline.layer_index_for_id(id).is_some());
        for layer in &project.timeline.layers {
            self.set_layer_scope(
                layer.layer_id.clone(),
                layer.layer_type,
                layer.parent_layer_id.clone(),
            );
        }
        for layer in &project.timeline.layers {
            if let Some(effects) = &layer.effects {
                for instance in effects {
                    self.reconcile_instance(instance, &layer.layer_id, beat, &mut source_changed);
                }
            }
            if let Some(instance) = layer.gen_params() {
                self.reconcile_instance(instance, &layer.layer_id, beat, &mut source_changed);
            }
        }
        self.bindings.instances.retain(|owner, instance| {
            instance.retain(|param, binding| {
                if !binding.seen {
                    source_changed(owner, param);
                }
                binding.seen
            });
            !instance.is_empty()
        });
        self.bindings.initialized = true;
    }

    fn reconcile_instance(
        &mut self,
        instance: &PresetInstance,
        owner: &LayerId,
        beat: Beats,
        source_changed: &mut impl FnMut(&EffectId, &str),
    ) {
        for param in instance.params.iter() {
            let source = if instance.enabled {
                self.source_layer(&param.clip_trigger_source, Some(owner)).cloned()
            } else { None };
            let cutoff = EventCutoff {
                beat,
                sequence: self.next_sequence,
            };
            let bindings = self
                .bindings
                .instances
                .entry(instance.id.clone())
                .or_default();
            if let Some(binding) = bindings.get_mut(param.id()) {
                if binding.source != source {
                    source_changed(&instance.id, param.id());
                    binding.source = source;
                    binding.cutoff = Some(cutoff);
                }
                binding.seen = true;
            } else {
                bindings.insert(
                    param.id().to_owned(),
                    Binding {
                        source,
                        cutoff: self.bindings.initialized.then_some(cutoff),
                        seen: true,
                    },
                );
            }
        }
    }

    pub(crate) fn clear_binding_cutoffs(&mut self) {
        for instance in self.bindings.instances.values_mut() {
            for binding in instance.values_mut() {
                binding.cutoff = None;
            }
        }
    }

    pub(crate) fn parameter_starts<'a>(
        &'a self,
        instance: &EffectId,
        param: &Param,
        owner: Option<&LayerId>,
    ) -> ClipControlStarts<'a> {
        let cutoff = self
            .bindings
            .instances
            .get(instance)
            .and_then(|bindings| bindings.get(param.id()))
            .and_then(|binding| binding.cutoff);
        ClipControlStarts {
            starts: self.starts(&param.clip_trigger_source, owner),
            cutoff,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::layer::Layer;
    use manifold_core::params::ClipTriggerSource;
    use manifold_core::{ClipId, PresetTypeId};

    #[test]
    fn cutoff_rejects_late_history_but_accepts_new_equal_beat_starts() {
        let mut owner = Layer::new_generator("Owner".into(), PresetTypeId::new("Test"), 0);
        owner
            .gen_params_or_init()
            .params
            .push(Param::bundled(ParamSpecDef {
                id: "value".into(),
                ..Default::default()
            }));
        let lane = Layer::new_trigger("Lane".into(), owner.layer_id.clone(), 1);
        let source = lane.layer_id.clone();
        let mut project = Project::default();
        project.timeline.layers = vec![owner, lane];
        let mut frame = ClipControlFrame::default();
        frame.reconcile_bindings(&project, Beats::ZERO, |_, _| {});
        let event = |id, beat| ClipControlStart {
            clip_id: ClipId::new(id),
            beat: Beats(beat),
            is_muted: false,
            sequence: 0,
        };
        frame.record_start(source.clone(), event("old", 2.0));
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .params
            .get_mut("value")
            .unwrap()
            .clip_trigger_source = ClipTriggerSource::Lane {
            layer_id: source.clone(),
        };
        frame.reconcile_bindings(&project, Beats(2.0), |_, _| {});
        frame.record_start(source.clone(), event("late-history", 1.0));
        frame.record_start(source.clone(), event("new", 2.0));
        frame.record_start(source, event("future", 3.0));
        frame.finish();
        let owner = &project.timeline.layers[0];
        let instance = owner.gen_params().unwrap();
        let param = instance.params.get("value").unwrap();
        let accepted = frame.parameter_starts(&instance.id, param, Some(&owner.layer_id));
        assert_eq!(
            accepted
                .iter()
                .map(|start| start.clip_id.as_str())
                .collect::<Vec<_>>(),
            ["new", "future"]
        );

        project.timeline.layers[0].gen_params_mut().unwrap().enabled = false;
        frame.reconcile_bindings(&project, Beats(3.0), |_, _| {});
        project.timeline.layers[0].gen_params_mut().unwrap().enabled = true;
        frame.reconcile_bindings(&project, Beats(3.0), |_, _| {});
        let owner = &project.timeline.layers[0];
        let instance = owner.gen_params().unwrap();
        let param = instance.params.get("value").unwrap();
        assert_eq!(
            frame
                .parameter_starts(&instance.id, param, Some(&owner.layer_id))
                .len(),
            0
        );
        frame.clear_binding_cutoffs();
        assert_eq!(
            frame
                .parameter_starts(&instance.id, param, Some(&owner.layer_id))
                .len(),
            4
        );
    }
}
