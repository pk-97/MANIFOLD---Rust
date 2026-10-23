//! Neutral snapshots for the instance state coupled to graph edits.

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
