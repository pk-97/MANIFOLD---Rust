use serde_json::Value;

/// Bring a project at or above `OLDEST_LOADABLE_PROJECT_VERSION` (1.11.0) up
/// to `CURRENT_PROJECT_VERSION` before deserialization. The loader refuses
/// older files before this runs, so the ladder starts at the 1.11 → 1.12 rung;
/// the migrations below the floor live on branch `legacy/pre-1.11-loader`.
///
/// Every rung stamps its own literal target, never `CURRENT_PROJECT_VERSION`:
/// wiring the const into a rung that later becomes intermediate would stamp
/// the newer version, the next rung's gate would read false, and its
/// migration would silently never run. The ladder top equals
/// `CURRENT_PROJECT_VERSION` (pinned by `floor_input_chains_to_current_version`).
pub fn migrate_if_needed(json: &str) -> Result<String, serde_json::Error> {
    if json.trim().is_empty() {
        return Ok(json.to_string());
    }

    // Malformed JSON passes through so the typed deserialize reports it.
    let mut root: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Ok(json.to_string()),
    };

    let version = root
        .get("projectVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0.0")
        .to_string();

    // v1.11.0 -> v1.12.0
    // `node.render_scene` sheds its nine-per-object TRS params for a
    // `transform_n: Transform` port; this rung synthesizes one
    // `node.transform_3d` node per legacy object (inside the object's own
    // group when the importer shape traces `mesh_n` there, else top-level),
    // carries the old values across, and re-points any card binding that
    // targeted the retired params.
    if is_version_less_than(&version, "1.12.0") {
        crate::migrations::scene_transform_v1120::migrate(&mut root);
        root["projectVersion"] = Value::String("1.12.0".to_string());
    }

    // v1.13.0 -> v1.14.0: BUG-bdwd — calibrate stored CoC graphs to their
    // scene scale. `node.coc_from_depth` gained a `world_to_mm` param
    // (imported scenes stamp 1000/scene_radius); this rung carries existing
    // projects across: graphs with a coc node + sceneBounds get
    // world_to_mm = 1000/R and every stored f_stop × R (look-preserving);
    // everything else is a byte-identical passthrough.
    if is_version_less_than(&version, "1.14.0") {
        crate::migrations::scene_scale_coc_v1140::migrate(&mut root);
        root["projectVersion"] = Value::String("1.14.0".to_string());
    }

    // v1.14.0 -> v1.15.0 reserves lossless graph-v3 scene modifier storage.
    // Legacy modifier adoption is a later activation; this rung changes no graph.
    if is_version_less_than(&version, "1.15.0") {
        root["projectVersion"] = Value::String("1.15.0".to_string());
    }

    // Camera device depth now runs near=1, far=0. Physical camera and
    // material parameters are unchanged. Gate older binaries on save and
    // report custom raw-depth consumers instead of guessing shader edits.
    if is_version_less_than(&version, "1.16.0") {
        crate::migrations::reversed_depth_v1160::audit(&root);
        root["projectVersion"] = Value::String("1.16.0".to_string());
    }

    // 2026-10-02: rigid bodies take density; a stored mass was size-blind,
    // so it is dropped along with wires and cards bound to it.
    if is_version_less_than(&version, "1.17.0") {
        crate::migrations::rigid_body_density_v1170::migrate(&mut root);
        root["projectVersion"] = Value::String("1.17.0".to_string());
    }

    // GPU FLIP Domain grew the Solve Level card; stored graphs get the card,
    // the binding and the domain→step wire the bundled def ships.
    if is_version_less_than(&version, "1.18.0") {
        crate::migrations::solve_level_card_v1180::migrate(&mut root);
        root["projectVersion"] = Value::String("1.18.0".to_string());
    }

    // GPU FLIP Domain grew the Max Iterations card (the pressure cap went to 900).
    if is_version_less_than(&version, "1.19.0") {
        crate::migrations::pressure_cap_card_v1190::migrate(&mut root);
        root["projectVersion"] = Value::String("1.19.0".to_string());
    }

    // GPU FLIP Step reads the bodies' touching contacts; stored graphs get the wire.
    if is_version_less_than(&version, "1.20.0") {
        crate::migrations::contacts_wire_v1200::migrate(&mut root);
        root["projectVersion"] = Value::String("1.20.0".to_string());
    }

    // Saved bundled controls are stubs the live lookup fills; no shape change, the bump makes older builds refuse.
    if is_version_less_than(&version, "1.21.0") {
        root["projectVersion"] = Value::String("1.21.0".to_string());
    }

    // Scene cards show only pinned rows; the loader flags older files so the
    // app pins the rows they showed (D-103). No shape change.
    if is_version_less_than(&version, "1.22.0") {
        root["projectVersion"] = Value::String("1.22.0".to_string());
    }

    // Enum settings are stored by name; older files stored numbers.
    if is_version_less_than(&version, "1.23.0") {
        crate::migrations::enum_names_v1230::migrate(&mut root);
        root["projectVersion"] = Value::String("1.23.0".to_string());
    }

    // Scene modifiers address nodes by plain node id, not a scoped
    // `{scope, node}` reference; this rung rewrites the stored references.
    if is_version_less_than(&version, "1.24.0") {
        crate::migrations::scene_node_ids_v1240::migrate(&mut root);
        root["projectVersion"] = Value::String("1.24.0".to_string());
    }

    serde_json::to_string_pretty(&root)
}

/// Apply `f` to every `PresetInstance` JSON object in the project — master
/// effects, each layer's effects, each clip's effects, and each layer's
/// generator (`genParams`). The single per-instance traversal for load
/// migrations: effects and generators are one type now, so a per-instance
/// migration walks them here instead of a per-kind hand-rolled loop.
pub(crate) fn for_each_preset_instance(root: &mut Value, mut f: impl FnMut(&mut Value)) {
    if let Some(effects) = root
        .get_mut("settings")
        .and_then(|s| s.get_mut("masterEffects"))
        .and_then(|e| e.as_array_mut())
    {
        for fx in effects.iter_mut() {
            f(fx);
        }
    }
    let Some(layers) = root
        .get_mut("timeline")
        .and_then(|t| t.get_mut("layers"))
        .and_then(|l| l.as_array_mut())
    else {
        return;
    };
    for layer in layers.iter_mut() {
        if let Some(effects) = layer.get_mut("effects").and_then(|e| e.as_array_mut()) {
            for fx in effects.iter_mut() {
                f(fx);
            }
        }
        if let Some(clips) = layer.get_mut("clips").and_then(|c| c.as_array_mut()) {
            for clip in clips.iter_mut() {
                if let Some(effects) = clip.get_mut("effects").and_then(|e| e.as_array_mut()) {
                    for fx in effects.iter_mut() {
                        f(fx);
                    }
                }
            }
        }
        if let Some(gp) = layer.get_mut("genParams").filter(|g| g.is_object()) {
            f(gp);
        }
    }
}

/// `pub(crate)`: the forward-compat guard in `loader.rs` reuses this same
/// comparator; do not duplicate it.
pub(crate) fn is_version_less_than(version: &str, threshold: &str) -> bool {
    let v_parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();
    let t_parts: Vec<u32> = threshold
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();

    for i in 0..3 {
        let v = v_parts.get(i).copied().unwrap_or(0);
        let t = t_parts.get(i).copied().unwrap_or(0);
        if v < t {
            return true;
        }
        if v > t {
            return false;
        }
    }
    false // equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        assert!(is_version_less_than("1.0.0", "1.1.0"));
        assert!(!is_version_less_than("1.1.0", "1.1.0"));
        assert!(!is_version_less_than("1.2.0", "1.1.0"));
    }

    /// The ladder's top must equal CURRENT_PROJECT_VERSION. Bumping the const
    /// without adding a final rung fails here, so no version ships whose
    /// migration never runs.
    #[test]
    fn floor_input_chains_to_current_version() {
        let json = format!(
            r#"{{"projectVersion": "{}", "projectName": "test", "timeline": {{"layers": []}}}}"#,
            manifold_core::project::OLDEST_LOADABLE_PROJECT_VERSION
        );
        let migrated = migrate_if_needed(&json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some(manifold_core::project::CURRENT_PROJECT_VERSION),
            "migration ladder top must equal CURRENT_PROJECT_VERSION — add a final rung when the const bumps"
        );
    }
}
