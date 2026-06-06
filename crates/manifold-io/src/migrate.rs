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

    if is_version_less_than(&version, "1.3.0") {
        migrate_v120_to_v130(&mut root);
        root["projectVersion"] = Value::String("1.3.0".to_string());
    }

    if is_version_less_than(&version, "1.4.0") {
        migrate_v130_to_v140(&mut root);
        root["projectVersion"] = Value::String("1.4.0".to_string());
    }

    serde_json::to_string_pretty(&root)
}

/// v1.3.0 → v1.4.0: binding-storage unification. User-added effect
/// bindings used to live in a parallel `PresetInstance.userParamBindings`
/// array; they now live in the per-instance graph's
/// `preset_metadata.bindings` (with `userAdded: true`), the single
/// binding list generators already use.
///
/// For every effect carrying a non-empty `userParamBindings`, this fold-in
/// (preserving every binding `id` exactly — drivers / envelopes / Ableton
/// / OSC reference them forever):
///   1. ensures the effect has a `graph` with a `presetMetadata` block
///      (a metadata-only **stub** when the effect had no per-instance
///      graph — the renderer's load pass lifts the canonical topology
///      under it, since the JSON layer can't build a preset's nodes);
///   2. appends each legacy binding as a `BindingDef { userAdded: true }`
///      (routing + the card→consumer affine `scale`/`offset`) plus its
///      `ParamSpecDef` carrying the reshape (declared range + invert/curve) —
///      the single reshape home after the per-instance `paramMappings` note
///      was deleted. Identity invert/curve are skipped so a plain expose
///      stays byte-identical to a freshly authored spec;
///   3. deletes the `userParamBindings` field.
///
/// Idempotent: the field is gone after migration, so a re-load finds
/// nothing to fold (and a freshly-saved v1.4 project already carries the
/// bindings in the graph).
fn migrate_v130_to_v140(root: &mut Value) {
    if let Some(effects) = root
        .get_mut("settings")
        .and_then(|s| s.get_mut("masterEffects"))
        .and_then(|e| e.as_array_mut())
    {
        for fx in effects.iter_mut() {
            fold_user_param_bindings(fx);
        }
    }
    if let Some(layers) = root
        .get_mut("timeline")
        .and_then(|t| t.get_mut("layers"))
        .and_then(|l| l.as_array_mut())
    {
        for layer in layers.iter_mut() {
            if let Some(effects) = layer.get_mut("effects").and_then(|e| e.as_array_mut()) {
                for fx in effects.iter_mut() {
                    fold_user_param_bindings(fx);
                }
            }
            if let Some(clips) = layer.get_mut("clips").and_then(|c| c.as_array_mut()) {
                for clip in clips.iter_mut() {
                    if let Some(effects) =
                        clip.get_mut("effects").and_then(|e| e.as_array_mut())
                    {
                        for fx in effects.iter_mut() {
                            fold_user_param_bindings(fx);
                        }
                    }
                }
            }
        }
    }
}

/// Fold one effect's `userParamBindings` into its graph metadata. No-op
/// when the array is absent or empty.
fn fold_user_param_bindings(fx: &mut Value) {
    let Value::Object(map) = fx else {
        return;
    };
    let user_bindings = match map.remove("userParamBindings") {
        Some(Value::Array(b)) if !b.is_empty() => b,
        // Absent / empty / malformed: drop the (empty) key and stop.
        _ => return,
    };

    // Ensure `graph.presetMetadata.{params, bindings}` and `paramMappings`
    // exist as objects/arrays we can push into.
    let graph = map
        .entry("graph")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !graph.is_object() {
        *graph = Value::Object(serde_json::Map::new());
    }
    let graph_obj = graph.as_object_mut().expect("graph is an object");
    // A freshly-minted stub graph needs a version stamp + empty node/wire
    // arrays so it deserializes; the renderer load pass lifts the real
    // topology under the metadata. Don't clobber an existing graph.
    graph_obj.entry("version").or_insert(Value::from(0u32));
    graph_obj
        .entry("nodes")
        .or_insert_with(|| Value::Array(Vec::new()));
    graph_obj
        .entry("wires")
        .or_insert_with(|| Value::Array(Vec::new()));
    let meta = graph_obj
        .entry("presetMetadata")
        .or_insert_with(|| Value::Object(default_preset_metadata()));
    if !meta.is_object() {
        *meta = Value::Object(default_preset_metadata());
    }
    let meta_obj = meta.as_object_mut().expect("presetMetadata is an object");
    let params = meta_obj
        .entry("params")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !params.is_array() {
        *params = Value::Array(Vec::new());
    }
    let mut new_params: Vec<Value> = Vec::new();
    let mut new_bindings: Vec<Value> = Vec::new();

    for ub in &user_bindings {
        let Value::Object(b) = ub else { continue };
        let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let label = b
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let inner_param = b
            .get("innerParam")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let default_value = b.get("defaultValue").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let min = b.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let max = b.get("max").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let scale = b.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let offset = b.get("offset").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let invert = b.get("invert").and_then(|v| v.as_bool()).unwrap_or(false);
        let convert = b.get("convert").cloned();
        let curve = b.get("curve").cloned();
        // Node id (post-node-id projects) or legacy handle (pre-node-id).
        let node_id = b
            .get("nodeId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let legacy_handle = b.get("nodeHandle").and_then(|v| v.as_str()).map(String::from);

        // Param spec — the single reshape home: declared range plus the
        // slider response (invert / curve). scale/offset ride the binding
        // below. There is no per-instance note anymore, so the curve/invert
        // that used to fold into the deleted `paramMappings` note land here.
        let curve_is_linear = curve
            .as_ref()
            .map(|c| c.as_str() == Some("Linear") || c.is_null())
            .unwrap_or(true);
        let mut spec = serde_json::Map::new();
        spec.insert("id".into(), Value::from(id.clone()));
        spec.insert("name".into(), Value::from(label.clone()));
        spec.insert("min".into(), Value::from(min));
        spec.insert("max".into(), Value::from(max));
        spec.insert("defaultValue".into(), Value::from(default_value));
        // Skip identity invert/curve so an un-reshaped binding stays
        // byte-identical to a freshly authored ParamSpecDef.
        if invert {
            spec.insert("invert".into(), Value::Bool(true));
        }
        if !curve_is_linear && let Some(c) = curve.clone() {
            spec.insert("curve".into(), c);
        }
        new_params.push(Value::Object(spec));

        // Binding (routing only). `target` is `Node { nodeId, param }`
        // when we have a node id, else the legacy `HandleNode { handle,
        // param }` form, which `BindingTarget`'s tolerant reader upgrades
        // to `Node { nodeId == handle }` on load.
        let target = if !node_id.is_empty() {
            serde_json::json!({ "kind": "node", "nodeId": node_id, "param": inner_param })
        } else if let Some(handle) = legacy_handle {
            serde_json::json!({ "kind": "handleNode", "handle": handle, "param": inner_param })
        } else {
            // No target at all — fall back to a node target keyed by the
            // inner param name so nothing is silently dropped.
            serde_json::json!({ "kind": "node", "nodeId": inner_param, "param": inner_param })
        };
        let mut binding = serde_json::Map::new();
        binding.insert("id".into(), Value::from(id.clone()));
        binding.insert("label".into(), Value::from(label.clone()));
        binding.insert("defaultValue".into(), Value::from(default_value));
        binding.insert("target".into(), target);
        if let Some(c) = convert {
            binding.insert("convert".into(), c);
        }
        binding.insert("userAdded".into(), Value::Bool(true));
        if scale != 1.0 {
            binding.insert("scale".into(), Value::from(scale));
        }
        if offset != 0.0 {
            binding.insert("offset".into(), Value::from(offset));
        }
        new_bindings.push(Value::Object(binding));
    }

    if let Some(arr) = meta_obj.get_mut("params").and_then(|v| v.as_array_mut()) {
        arr.extend(new_params);
    }
    let bindings = meta_obj
        .entry("bindings")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(arr) = bindings.as_array_mut() {
        arr.extend(new_bindings);
    } else {
        *bindings = Value::Array(new_bindings);
    }
}

/// A minimal `presetMetadata` block for a freshly-minted stub graph. Only
/// `params` / `bindings` are populated by the fold-in; the rest are the
/// schema defaults so the document deserializes. The renderer load pass
/// replaces this whole block when it lifts the canonical topology.
fn default_preset_metadata() -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), Value::from(""));
    m.insert("displayName".into(), Value::from(""));
    m.insert("category".into(), Value::from(""));
    m.insert("oscPrefix".into(), Value::from(""));
    m.insert("params".into(), Value::Array(Vec::new()));
    m.insert("bindings".into(), Value::Array(Vec::new()));
    m
}

/// v1.2.0 → v1.3.0: per-param exposure (`ParamSlot { value, exposed }`)
/// surfaces on `PresetInstance.paramValues`. The on-disk shape changes:
/// V1.2 emitted bare `f32` per slot (positional Array or keyed Map);
/// V1.3 emits `{ value, exposed }` objects.
///
/// No JSON rewriting is required here — `ParamSlot`'s polymorphic
/// `Deserialize` accepts the legacy bare-f32 shape natively (defaulting
/// `exposed` to `true`), and the next save canonicalizes to the V1.3
/// object form. `baseParamValues` continues to use plain f32 (exposure
/// isn't meaningful on the pre-modulation snapshot), so nothing changes
/// there. This migration only bumps the version stamp.
fn migrate_v120_to_v130(_root: &mut Value) {
    // No-op. Polymorphic ParamSlot deserializer handles V1.2 wire shape.
}

/// v1.1.0 → v1.2.0: parameter addressing migration to stable
/// `param_id`. The bidirectional `Deserialize` impls on
/// `ParameterDriver`, `ParamEnvelope`, `AbletonParamMapping`,
/// `MacroMapping`, `PresetInstance`, and `PresetInstance`
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
    fn test_v110_input_bumps_to_v140() {
        // V1.1 project with no legacy quirks → migrate_if_needed
        // chains forward: 1.1 → 1.2 (param-id) → 1.3 (per-param
        // exposure) → 1.4 (binding-storage unification). The
        // bidirectional Deserialize impls handle the shape changes on
        // load — only the 1.4 fold-in rewrites JSON, and only when an
        // effect carries userParamBindings.
        let json = r#"{
            "projectVersion": "1.1.0",
            "projectName": "test"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.4.0")
        );
    }

    #[test]
    fn test_v100_chains_through_to_v140() {
        // V1.0 project should chain all the way to v1.4.
        let json = r#"{
            "projectVersion": "1.0.0",
            "projectName": "test",
            "timeline": {"layers": []}
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.4.0")
        );
    }

    #[test]
    fn test_v120_input_bumps_to_v140() {
        let json = r#"{
            "projectVersion": "1.2.0",
            "projectName": "phase3"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.4.0")
        );
    }

    #[test]
    fn test_v140_input_is_not_remigrated() {
        // Already-current projects pass through unchanged.
        let json = r#"{
            "projectVersion": "1.4.0",
            "projectName": "current"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.4.0")
        );
    }

    /// The load-bearing test for this work: a v1.3 project whose effect
    /// carries a `userParamBindings` array folds those bindings into the
    /// effect's graph (`presetMetadata.bindings`, `userAdded: true`) with
    /// the binding id preserved EXACTLY (drivers / envelopes / Ableton /
    /// OSC reference it forever), the declared range captured as a spec,
    /// any reshape moved to a `paramMappings` note, and the legacy array
    /// removed.
    #[test]
    fn v130_user_param_bindings_fold_into_graph_metadata() {
        // A master effect (graph: None) with one user binding carrying a
        // non-identity reshape (range 0..360, invert) and a node id.
        let json = r#"{
            "projectVersion": "1.3.0",
            "projectName": "fold",
            "settings": {
                "masterEffects": [
                    {
                        "id": "fx-1",
                        "effectType": "bloom",
                        "enabled": true,
                        "collapsed": false,
                        "paramValues": [],
                        "userParamBindings": [
                            {
                                "id": "user.uv.rotation.1",
                                "label": "Spin",
                                "nodeId": "uv_transform",
                                "innerParam": "rotation",
                                "min": 0.0,
                                "max": 360.0,
                                "defaultValue": 90.0,
                                "convert": { "type": "Float" },
                                "invert": true,
                                "scale": 2.0,
                                "offset": 5.0
                            }
                        ]
                    }
                ]
            },
            "timeline": { "layers": [] }
        }"#;

        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();

        let fx = &v["settings"]["masterEffects"][0];
        // Legacy array is gone.
        assert!(
            fx.get("userParamBindings").is_none(),
            "userParamBindings must be removed after fold-in"
        );

        // Binding landed in graph.presetMetadata.bindings, user_added.
        let bindings = fx["graph"]["presetMetadata"]["bindings"]
            .as_array()
            .expect("bindings array present");
        assert_eq!(bindings.len(), 1, "exactly one user binding folded in");
        let b = &bindings[0];
        assert_eq!(
            b["id"].as_str(),
            Some("user.uv.rotation.1"),
            "binding id preserved EXACTLY"
        );
        assert_eq!(b["userAdded"].as_bool(), Some(true));
        assert_eq!(b["target"]["kind"].as_str(), Some("node"));
        assert_eq!(b["target"]["nodeId"].as_str(), Some("uv_transform"));
        assert_eq!(b["target"]["param"].as_str(), Some("rotation"));
        assert_eq!(b["scale"].as_f64(), Some(2.0));
        assert_eq!(b["offset"].as_f64(), Some(5.0));

        // Spec is the single reshape home: declared range + invert/curve.
        let params = fx["graph"]["presetMetadata"]["params"]
            .as_array()
            .expect("params array present");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["id"].as_str(), Some("user.uv.rotation.1"));
        assert_eq!(params[0]["max"].as_f64(), Some(360.0));
        assert_eq!(
            params[0]["invert"].as_bool(),
            Some(true),
            "invert reshape lands on the param spec, not a note",
        );

        // No per-instance note is emitted anymore.
        assert!(
            fx.get("paramMappings").is_none(),
            "the paramMappings note was deleted — reshape lives on the spec",
        );

        // Idempotent: re-running finds nothing to fold (field gone).
        let again = migrate_if_needed(&migrated).unwrap();
        let v2: Value = serde_json::from_str(&again).unwrap();
        let bindings2 = v2["settings"]["masterEffects"][0]["graph"]["presetMetadata"]["bindings"]
            .as_array()
            .expect("bindings still present");
        assert_eq!(bindings2.len(), 1, "no double-fold on re-migration");
    }

    /// A legacy (pre-node-id) user binding addressed by `nodeHandle`
    /// folds into the graph with the tolerant `handleNode` target form,
    /// preserving the id.
    #[test]
    fn v130_legacy_handle_user_binding_folds_with_handle_target() {
        let json = r#"{
            "projectVersion": "1.3.0",
            "projectName": "legacy",
            "settings": {
                "masterEffects": [
                    {
                        "id": "fx-1",
                        "effectType": "bloom",
                        "enabled": true,
                        "collapsed": false,
                        "paramValues": [],
                        "userParamBindings": [
                            {
                                "id": "user.blur.radius.1",
                                "label": "Radius",
                                "nodeHandle": "blur",
                                "innerParam": "radius",
                                "min": 0.0,
                                "max": 1.0,
                                "defaultValue": 0.5,
                                "convert": { "type": "Float" }
                            }
                        ]
                    }
                ]
            },
            "timeline": { "layers": [] }
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        let b = &v["settings"]["masterEffects"][0]["graph"]["presetMetadata"]["bindings"][0];
        assert_eq!(b["id"].as_str(), Some("user.blur.radius.1"));
        assert_eq!(b["target"]["kind"].as_str(), Some("handleNode"));
        assert_eq!(b["target"]["handle"].as_str(), Some("blur"));
        // Identity reshape (0..1, no invert) → spec carries no invert/curve
        // keys and no note is ever emitted.
        assert!(
            v["settings"]["masterEffects"][0].get("paramMappings").is_none(),
            "the paramMappings note was deleted",
        );
        let spec = &v["settings"]["masterEffects"][0]["graph"]["presetMetadata"]["params"][0];
        assert!(
            spec.get("invert").is_none() && spec.get("curve").is_none(),
            "identity reshape skips invert/curve on the spec",
        );
    }
}
