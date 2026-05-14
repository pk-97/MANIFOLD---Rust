//! Pure derivation: snapshot of mapped Ableton macros for the HUD.
//!
//! Walks the project for every `AbletonParamMapping`, looks the macro up
//! in the cached `AbletonSession` to fetch its display name and current
//! 0–1 value, and dedups so the same macro isn't shown twice when one
//! macro is mapped to multiple MANIFOLD parameters.
//!
//! Returns the macros in a stable, deterministic order: by track index,
//! then device index, then macro index. Frame-to-frame stability matters
//! more than aesthetics — the user must not see the macros visually
//! "shuffle" between frames as their values change.

use std::collections::HashSet;

use manifold_core::project::Project;
use manifold_playback::ableton_bridge::AbletonSession;

/// One macro entry as displayed in the HUD.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MacroDisplay {
    /// Display name from Ableton (the macro's user-facing name on the
    /// rack — e.g. "FILTER", "REVERB").
    pub name: String,
    /// Normalized 0..=1 value (last value received from Ableton).
    pub value: f32,
}

/// Collect every unique mapped macro in the project, look it up in the
/// session to get the display name + current value, and return them in
/// `(track_id, device_id, param_id)` order.
///
/// `limit` truncates the list — for the HUD we cap to a fixed top-N to
/// avoid overflowing the column.
pub(crate) fn snapshot(
    project: &Project,
    session: &AbletonSession,
    limit: usize,
) -> Vec<MacroDisplay> {
    // Step 1: collect every unique (track, device, param) triple from
    // the project's mapping graph. We dedupe via a set so a macro mapped
    // to multiple MANIFOLD parameters only appears once in the HUD.
    let mut seen: HashSet<(i32, i32, i32)> = HashSet::new();
    let mut keys: Vec<(i32, i32, i32)> = Vec::new();

    let mut visit_mappings =
        |mappings: &Vec<manifold_core::ableton_mapping::AbletonParamMapping>| {
            for m in mappings {
                let key = (m.address.track_id, m.address.device_id, m.address.param_id);
                if seen.insert(key) {
                    keys.push(key);
                }
            }
        };

    for fx in &project.settings.master_effects {
        if let Some(ms) = &fx.ableton_mappings {
            visit_mappings(ms);
        }
    }
    for layer in &project.timeline.layers {
        if let Some(effects) = &layer.effects {
            for fx in effects {
                if let Some(ms) = &fx.ableton_mappings {
                    visit_mappings(ms);
                }
            }
        }
        if let Some(gp) = layer.gen_params()
            && let Some(ms) = &gp.ableton_mappings
        {
            visit_mappings(ms);
        }
    }
    for slot in &project.settings.macro_bank.slots {
        if let Some(m) = &slot.ableton_mapping {
            let key = (m.address.track_id, m.address.device_id, m.address.param_id);
            if seen.insert(key) {
                keys.push(key);
            }
        }
    }

    // Step 2: stable order — sort by (track_id, device_id, param_id).
    // Uses the original macro layout in Ableton, not the order we
    // happened to find them in the project graph.
    keys.sort();

    // Step 3: resolve each key against the session, dropping any that
    // can't be found (mapping is dormant / Ableton session changed).
    let mut out: Vec<MacroDisplay> = Vec::with_capacity(keys.len().min(limit));
    for (tid, did, pid) in keys {
        if out.len() >= limit {
            break;
        }
        let Some(track) = session.tracks.iter().find(|t| t.track_id == tid) else {
            continue;
        };
        let Some(device) = track.devices.iter().find(|d| d.device_id == did) else {
            continue;
        };
        let Some(macro_) = device.macros.iter().find(|m| m.param_id == pid) else {
            continue;
        };
        // The session's macro value is in the macro's native [min, max]
        // range. Normalize to 0..=1 for the bar graph.
        let span = (macro_.max - macro_.min).abs();
        let value = if span > 1.0e-6 {
            ((macro_.value - macro_.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(MacroDisplay {
            name: macro_.name.clone(),
            value,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The full unit-test surface here would require constructing a Project
    // and AbletonSession, both of which have many fields. We test the
    // observable behavior we care about (dedup, normalization, sorting)
    // via integration with the bridge during real runtime — these tests
    // cover the easy compile-time invariants.

    #[test]
    fn empty_inputs_yield_empty_output() {
        let project = manifold_core::project::Project::default();
        let session = AbletonSession::default();
        let result = snapshot(&project, &session, 100);
        assert!(result.is_empty());
    }

    #[test]
    fn limit_zero_yields_empty() {
        let project = manifold_core::project::Project::default();
        let session = AbletonSession::default();
        let result = snapshot(&project, &session, 0);
        assert!(result.is_empty());
    }
}
