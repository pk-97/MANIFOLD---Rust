//! Neutral snapshots for the instance state coupled to graph edits.

/// The instance layer pruned by graph edits: the live parameter manifest plus
/// every modulation collection that can target a parameter id. Capturing the
/// whole layer keeps undo byte-for-byte with the pre-edit state.
#[derive(Debug, Clone)]
pub(crate) struct InstanceLayerSnapshot {
    params: manifold_core::params::ParamManifest,
    drivers: Option<Vec<manifold_core::effects::ParameterDriver>>,
    envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    ableton_mappings: Option<Vec<manifold_core::ableton_mapping::AbletonParamMapping>>,
    audio_mods: Option<Vec<manifold_core::audio_mod::ParameterAudioMod>>,
    automation_lanes: Option<Vec<manifold_core::effects::AutomationLane>>,
}

impl InstanceLayerSnapshot {
    pub(crate) fn capture(instance: &manifold_core::effects::PresetInstance) -> Self {
        Self {
            params: instance.params.clone(),
            drivers: instance.drivers.clone(),
            envelopes: instance.envelopes.clone(),
            ableton_mappings: instance.ableton_mappings.clone(),
            audio_mods: instance.audio_mods.clone(),
            automation_lanes: instance.automation_lanes.clone(),
        }
    }

    pub(crate) fn restore(self, instance: &mut manifold_core::effects::PresetInstance) {
        instance.params = self.params;
        instance.drivers = self.drivers;
        instance.envelopes = self.envelopes;
        instance.ableton_mappings = self.ableton_mappings;
        instance.audio_mods = self.audio_mods;
        instance.automation_lanes = self.automation_lanes;
    }
}

/// Drop every entry whose parameter id is in `ids`. Empty option vectors are
/// collapsed back to `None`, preserving the existing storage convention.
pub(crate) fn prune_by_param_id<T>(
    values: &mut Option<Vec<T>>,
    ids: &std::collections::BTreeSet<&str>,
    param_id: impl Fn(&T) -> &str,
) {
    if let Some(entries) = values.as_mut() {
        entries.retain(|entry| !ids.contains(param_id(entry)));
        if entries.is_empty() {
            *values = None;
        }
    }
}

/// Remove only controls whose public addresses disappeared in a prepared edit.
pub(crate) fn prune_instance_params(
    instance: &mut manifold_core::effects::PresetInstance,
    removed: &[String],
) {
    let ids: std::collections::BTreeSet<&str> = removed.iter().map(String::as_str).collect();
    for id in &ids {
        instance.params.remove(id);
    }
    prune_by_param_id(&mut instance.drivers, &ids, |item| &item.param_id);
    prune_by_param_id(&mut instance.envelopes, &ids, |item| &item.param_id);
    prune_by_param_id(&mut instance.ableton_mappings, &ids, |item| &item.param_id);
    prune_by_param_id(&mut instance.audio_mods, &ids, |item| &item.param_id);
    prune_by_param_id(&mut instance.automation_lanes, &ids, |item| &item.param_id);
}
