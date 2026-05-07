use serde_json::Value;

/// Port of C# ProjectJsonMigrator. Pre-processes JSON before deserialization.
/// Unity: ProjectJsonMigrator.MigrateIfNeeded (lines 16-39)
pub fn migrate_if_needed(json: &str) -> Result<String, serde_json::Error> {
    // Unity line 18: if (string.IsNullOrEmpty(json)) return json;
    if json.trim().is_empty() {
        return Ok(json.to_string());
    }

    // Unity lines 22-29: try { JObject.Parse(json) } catch { return json; }
    let mut root: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Ok(json.to_string()), // let downstream deserializer handle the error
    };

    let version = root
        .get("projectVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("1.0.0")
        .to_string();

    if is_version_less_than(&version, "1.1.0") {
        migrate_v100_to_v110(&mut root);
        root["projectVersion"] = Value::String("1.1.0".to_string());
    }

    if is_version_less_than(&version, "1.2.0") {
        migrate_v110_to_v120(&mut root);
        root["projectVersion"] = Value::String("1.2.0".to_string());
    }

    serde_json::to_string_pretty(&root)
}

/// v1.1.0 → v1.2.0: parameter addressing migration to stable
/// `param_id`. The bidirectional `Deserialize` impls on
/// `ParameterDriver`, `ParamEnvelope`, `AbletonParamMapping`,
/// `MacroMapping`, `EffectInstance`, and `GeneratorParamState`
/// (steps 8–13) accept both V1.1 (`paramIndex` / `Array`) and V1.2
/// (`paramId` / `Map`) shapes natively, so this migration only needs
/// to bump the version stamp — no JSON rewriting required.
///
/// Future legacy quirks (e.g., declarative `legacy_param_aliases` from
/// step 15) may add JSON-level rewrites here.
fn migrate_v110_to_v120(_root: &mut Value) {
    // No-op. All addressing-site migrations are handled by the
    // bidirectional Deserialize impls; the post-load resolver in
    // `Project::resolve_legacy_param_ids` translates parked
    // `legacy_param_index` values to stable `param_id` via the
    // effect/generator registries.
}

/// v1.0.0 → v1.1.0: Nest percussion fields into percussionImport,
/// nest generator fields into genParams on each layer.
fn migrate_v100_to_v110(root: &mut Value) {
    // ── Percussion import state ──
    let mut perc_import = serde_json::Map::new();
    move_field(
        root,
        "importedPercussionAudioPath",
        &mut perc_import,
        "audioPath",
    );
    move_field(
        root,
        "importedPercussionAudioStartBeat",
        &mut perc_import,
        "audioStartBeat",
    );
    move_field(
        root,
        "importedPercussionClipPlacements",
        &mut perc_import,
        "clipPlacements",
    );
    move_field(
        root,
        "percussionEnergyEnvelope",
        &mut perc_import,
        "energyEnvelope",
    );
    move_field(root, "importedStemPaths", &mut perc_import, "stemPaths");
    move_field(
        root,
        "importedPercussionAudioHash",
        &mut perc_import,
        "audioHash",
    );
    if let Value::Object(map) = root {
        map.insert("percussionImport".to_string(), Value::Object(perc_import));
    }

    // ── Generator param state (per layer) ──
    if let Some(layers) = root
        .get_mut("timeline")
        .and_then(|t| t.get_mut("layers"))
        .and_then(|l| l.as_array_mut())
    {
        for layer in layers.iter_mut() {
            let mut gen_params = serde_json::Map::new();
            move_field(layer, "generatorType", &mut gen_params, "generatorType");
            move_field(layer, "genParamValues", &mut gen_params, "paramValues");
            move_field(
                layer,
                "genParamBaseValues",
                &mut gen_params,
                "baseParamValues",
            );
            move_field(layer, "genDrivers", &mut gen_params, "drivers");
            move_field(layer, "genParamEnvelopes", &mut gen_params, "envelopes");
            if let Value::Object(map) = layer {
                map.insert("genParams".to_string(), Value::Object(gen_params));
            }
        }
    }
}

fn move_field(
    source: &mut Value,
    source_key: &str,
    target: &mut serde_json::Map<String, Value>,
    target_key: &str,
) {
    if let Value::Object(map) = source
        && let Some(val) = map.remove(source_key)
    {
        target.insert(target_key.to_string(), val);
    }
}

fn is_version_less_than(version: &str, threshold: &str) -> bool {
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

    #[test]
    fn test_v110_input_bumps_to_v120() {
        // V1.1 project with no legacy quirks → migrate_if_needed
        // bumps the version stamp to 1.2.0 (step 14). The
        // bidirectional Deserialize impls handle the parameter
        // addressing migration on load — no JSON rewriting needed.
        let json = r#"{
            "projectVersion": "1.1.0",
            "projectName": "test"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(v.get("projectVersion").and_then(|x| x.as_str()), Some("1.2.0"));
    }

    #[test]
    fn test_v100_chains_through_v110_to_v120() {
        // V1.0 project should chain: v1.0 → v1.1 (percussion +
        // genParams nesting) → v1.2 (version stamp only).
        let json = r#"{
            "projectVersion": "1.0.0",
            "projectName": "test",
            "timeline": {"layers": []}
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(v.get("projectVersion").and_then(|x| x.as_str()), Some("1.2.0"));
    }

    #[test]
    fn test_v120_input_is_not_remigrated() {
        // Already-current projects should pass through unchanged
        // (no spurious migration attempts).
        let json = r#"{
            "projectVersion": "1.2.0",
            "projectName": "current"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(v.get("projectVersion").and_then(|x| x.as_str()), Some("1.2.0"));
    }
}
