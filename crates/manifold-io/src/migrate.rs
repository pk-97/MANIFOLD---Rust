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

    if is_version_less_than(&version, "1.5.0") {
        migrate_v140_to_v150(&mut root);
        root["projectVersion"] = Value::String("1.5.0".to_string());
    }

    if is_version_less_than(&version, "1.6.0") {
        migrate_v150_to_v160(&mut root);
        root["projectVersion"] = Value::String("1.6.0".to_string());
    }

    if is_version_less_than(&version, "1.7.0") {
        migrate_v160_to_v170(&mut root);
        root["projectVersion"] = Value::String("1.7.0".to_string());
    }

    serde_json::to_string_pretty(&root)
}

/// v1.6.0 → v1.7.0: the WireframeDepth graph decomposition replaced the legacy
/// Rust impl, and the interim side-by-side type id `WireframeDepthGraph` was
/// retired — both names now mean the one surviving JSON preset, whose type id
/// is `WireframeDepth`. Rewrites `effectType` on every preset instance
/// (master / layer / clip). Param ids are shared between the two surfaces
/// (`amount`, `density`, …), and the preset's own `paramAliases` redirect the
/// retired legacy-only params, so instance `paramValues` carry over untouched.
/// Idempotent: no instance carries the retired id after one pass.
fn migrate_v160_to_v170(root: &mut Value) {
    for_each_preset_instance(root, |fx| {
        if fx.get("effectType").and_then(|v| v.as_str()) == Some("WireframeDepthGraph")
            && let Some(obj) = fx.as_object_mut()
        {
            obj.insert(
                "effectType".to_string(),
                Value::String("WireframeDepth".to_string()),
            );
        }
    });
}

/// v1.5.0 → v1.6.0: envelope-home unification. Effect envelopes used to live on
/// the container (`Layer.envelopes` / `Clip.envelopes`) keyed by
/// `targetEffectType`; they now ride on each effect's `PresetInstance.envelopes`
/// (keyed by `paramId` — the instance the envelope sits on is the target).
///
/// For every layer and clip carrying an `envelopes` array, this distributes
/// each envelope into the **first** effect in that container whose `effectType`
/// matches the envelope's `targetEffectType` (the same first-match the old
/// runtime used), strips the now-redundant `targetEffectType`, and drops the
/// container-level array. An envelope with no matching effect is dropped — it
/// was inert before (the evaluator found no target), so this is behavior-
/// preserving. Generator envelopes already lived on `genParams.envelopes` and
/// are untouched.
///
/// Idempotent: a v1.6 container has no `envelopes` field, so a re-run is a no-op.
fn migrate_v150_to_v160(root: &mut Value) {
    let Some(layers) = root
        .get_mut("timeline")
        .and_then(|t| t.get_mut("layers"))
        .and_then(|l| l.as_array_mut())
    else {
        return;
    };
    for layer in layers {
        let Some(map) = layer.as_object_mut() else {
            continue;
        };
        distribute_container_envelopes(map);
        if let Some(clips) = map.get_mut("clips").and_then(|c| c.as_array_mut()) {
            for clip in clips {
                if let Some(cmap) = clip.as_object_mut() {
                    distribute_container_envelopes(cmap);
                }
            }
        }
    }
}

/// Move a container's (`Layer` or `Clip`) legacy `envelopes` array onto its
/// effects, matching `targetEffectType` → the first effect's `effectType`.
/// Helper for [`migrate_v150_to_v160`].
fn distribute_container_envelopes(container: &mut serde_json::Map<String, Value>) {
    let Some(envs) = container.remove("envelopes") else {
        return;
    };
    let Some(envs) = envs.as_array().cloned().filter(|a| !a.is_empty()) else {
        return;
    };
    for mut env in envs {
        let target = env
            .get("targetEffectType")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(obj) = env.as_object_mut() {
            obj.remove("targetEffectType");
        }
        let Some(effects) = container.get_mut("effects").and_then(|e| e.as_array_mut()) else {
            continue; // no effects → orphan, drop
        };
        if let Some(fx) = effects.iter_mut().find(|fx| {
            fx.get("effectType").and_then(|v| v.as_str()) == target.as_deref()
        }) && let Some(fxobj) = fx.as_object_mut()
        {
            let arr = fxobj
                .entry("envelopes")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = arr.as_array_mut() {
                arr.push(env);
            }
        }
        // No matching effect → orphan, dropped (was inert pre-migration).
    }
}

/// v1.4.0 → v1.5.0: graph-home unification. The generator's per-instance graph
/// override used to live at the layer level (`generatorGraph` + two version
/// counters); it now lives on the generator `PresetInstance` itself
/// (`genParams.graph`), exactly like an effect's `graph`. For each layer
/// carrying a `generatorGraph`, this:
///   1. ensures the layer has a `genParams` object (synthesizing a minimal one
///      from the layer's `generatorType` when a graph existed without param
///      state — the old decoupled state);
///   2. moves `generatorGraph` into `genParams.graph`;
///   3. drops the layer-level `generatorGraph` + the two version counters (the
///      versions are runtime-only now and reset to 0 on load).
///
/// Idempotent: a v1.5 layer has no `generatorGraph` field, so a re-run finds
/// nothing to move.
fn migrate_v140_to_v150(root: &mut Value) {
    let Some(layers) = root
        .get_mut("timeline")
        .and_then(|t| t.get_mut("layers"))
        .and_then(|l| l.as_array_mut())
    else {
        return;
    };
    for layer in layers {
        let Some(map) = layer.as_object_mut() else {
            continue;
        };
        let Some(graph) = map.remove("generatorGraph") else {
            // No override on this layer — just drop any stale version counters.
            map.remove("generatorGraphVersion");
            map.remove("generatorGraphStructureVersion");
            continue;
        };
        map.remove("generatorGraphVersion");
        map.remove("generatorGraphStructureVersion");

        // Derive the generator type before borrowing `genParams` mutably, for
        // the rare graph-without-params state where we must synthesize a host.
        let gen_type = map
            .get("genParams")
            .and_then(|g| g.get("generatorType"))
            .or_else(|| map.get("generatorType"))
            .cloned();

        // Ensure a `genParams` object exists to host the graph. A generator
        // layer normally already has one; synthesize a minimal one otherwise.
        let gen_params = map.entry("genParams").or_insert_with(|| {
            let mut m = serde_json::Map::new();
            m.insert("paramValues".into(), Value::Array(Vec::new()));
            Value::Object(m)
        });
        if let Some(gp) = gen_params.as_object_mut() {
            if !gp.contains_key("generatorType")
                && let Some(t) = gen_type
            {
                gp.insert("generatorType".into(), t);
            }
            gp.insert("graph".into(), graph);
        }
    }
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
    // Effects and generators are the same `PresetInstance` shape now, so this
    // per-instance fold walks all of them through one traversal. Generators
    // never carried `userParamBindings` (they always used the graph binding
    // list), so `fold_user_param_bindings` is a no-op on them — including them
    // keeps the walk kind-agnostic without changing the output.
    for_each_preset_instance(root, fold_user_param_bindings);
}

/// Apply `f` to every `PresetInstance` JSON object in the project — master
/// effects, each layer's effects, each clip's effects, and each layer's
/// generator (`genParams`). The single per-instance traversal for load
/// migrations: effects and generators are one type now, so a per-instance
/// migration walks them here instead of a per-kind hand-rolled loop.
fn for_each_preset_instance(root: &mut Value, mut f: impl FnMut(&mut Value)) {
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
            Some("1.7.0")
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
            Some("1.7.0")
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
            Some("1.7.0")
        );
    }

    #[test]
    fn test_v160_input_is_not_remigrated() {
        // Already-current projects pass through unchanged.
        let json = r#"{
            "projectVersion": "1.6.0",
            "projectName": "current"
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.7.0")
        );
    }

    /// v1.5 → v1.6 envelope-home: a layer's `envelopes` array distributes onto
    /// the matching effect (first by `effectType` == `targetEffectType`), the
    /// `targetEffectType` key is stripped, and the layer-level array is dropped.
    /// An envelope with no matching effect is dropped (it was inert before).
    #[test]
    fn v150_envelopes_relocate_onto_matching_effect() {
        let json = r#"{
            "projectVersion": "1.5.0",
            "timeline": {"layers": [{
                "effects": [
                    {"effectType": "Bloom", "paramValues": []},
                    {"effectType": "Mirror", "paramValues": []}
                ],
                "envelopes": [
                    {"targetEffectType": "Mirror", "paramId": "amount", "enabled": true},
                    {"targetEffectType": "Ghost", "paramId": "x", "enabled": true}
                ]
            }]}
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        let layer = &v["timeline"]["layers"][0];
        assert!(
            layer.get("envelopes").is_none(),
            "layer-level envelopes array removed"
        );
        let effects = layer["effects"].as_array().unwrap();
        // Bloom got nothing.
        assert!(effects[0].get("envelopes").is_none());
        // Mirror got the matching envelope, sans targetEffectType.
        let mirror_envs = effects[1]["envelopes"].as_array().unwrap();
        assert_eq!(mirror_envs.len(), 1);
        assert_eq!(mirror_envs[0]["paramId"].as_str(), Some("amount"));
        assert!(
            mirror_envs[0].get("targetEffectType").is_none(),
            "redundant targetEffectType stripped"
        );
        // The "Ghost" envelope had no matching effect → dropped.
        assert_eq!(
            v.get("projectVersion").and_then(|x| x.as_str()),
            Some("1.7.0")
        );
    }

    /// v1.4 → v1.5 graph-home: a layer-level `generatorGraph` (+ version
    /// counters) relocates into `genParams.graph`; the layer-level fields are
    /// dropped. The generator type is preserved on the host.
    #[test]
    fn v140_generator_graph_relocates_into_gen_params() {
        let json = r#"{
            "projectVersion": "1.4.0",
            "projectName": "gen",
            "timeline": { "layers": [
                {
                    "layerId": "L1",
                    "layerType": "Generator",
                    "genParams": { "generatorType": "Plasma", "paramValues": [] },
                    "generatorGraph": { "version": 2, "name": "edited", "nodes": [], "wires": [] },
                    "generatorGraphVersion": 5,
                    "generatorGraphStructureVersion": 3
                }
            ] }
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        let layer = &v["timeline"]["layers"][0];
        assert!(
            layer.get("generatorGraph").is_none(),
            "layer-level generatorGraph must be removed",
        );
        assert!(layer.get("generatorGraphVersion").is_none());
        assert!(layer.get("generatorGraphStructureVersion").is_none());
        assert_eq!(
            layer["genParams"]["graph"]["name"].as_str(),
            Some("edited"),
            "the override now lives on genParams.graph",
        );
        assert_eq!(
            layer["genParams"]["generatorType"].as_str(),
            Some("Plasma"),
            "the generator type on the host is preserved",
        );
    }

    /// The graph-without-params edge case: a v1.4 layer with a `generatorGraph`
    /// but no `genParams` synthesizes a host carrying the flat `generatorType`.
    #[test]
    fn v140_generator_graph_without_params_synthesizes_host() {
        let json = r#"{
            "projectVersion": "1.4.0",
            "projectName": "gen",
            "timeline": { "layers": [
                {
                    "layerId": "L1",
                    "layerType": "Generator",
                    "generatorType": "Tesseract",
                    "generatorGraph": { "version": 1, "nodes": [], "wires": [] }
                }
            ] }
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        let layer = &v["timeline"]["layers"][0];
        assert!(layer.get("generatorGraph").is_none());
        assert!(
            layer["genParams"]["graph"].is_object(),
            "synthesized genParams hosts the graph",
        );
        assert_eq!(
            layer["genParams"]["generatorType"].as_str(),
            Some("Tesseract"),
            "synthesized host inherits the flat generator type",
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

    /// v1.7.0: the retired side-by-side type id `WireframeDepthGraph` rewrites
    /// to `WireframeDepth` (the surviving JSON preset) everywhere a preset
    /// instance lives; param values ride along untouched. Other type ids are
    /// untouched.
    #[test]
    fn test_v170_renames_wireframe_depth_graph_type() {
        let json = r#"{
            "projectVersion": "1.6.0",
            "settings": {
                "masterEffects": [
                    { "effectType": "WireframeDepthGraph", "enabled": true,
                      "paramValues": [ { "id": "density", "value": 120.0 } ] },
                    { "effectType": "ColorGrade", "enabled": true }
                ]
            },
            "timeline": { "layers": [ {
                "effects": [ { "effectType": "WireframeDepthGraph", "enabled": false } ],
                "clips": [ { "effects": [ { "effectType": "WireframeDepthGraph" } ] } ]
            } ] }
        }"#;
        let migrated = migrate_if_needed(json).unwrap();
        let v: Value = serde_json::from_str(&migrated).unwrap();
        assert_eq!(v["projectVersion"].as_str(), Some("1.7.0"));
        assert_eq!(
            v["settings"]["masterEffects"][0]["effectType"].as_str(),
            Some("WireframeDepth")
        );
        assert_eq!(
            v["settings"]["masterEffects"][0]["paramValues"][0]["value"].as_f64(),
            Some(120.0),
            "param values carry over untouched"
        );
        assert_eq!(
            v["settings"]["masterEffects"][1]["effectType"].as_str(),
            Some("ColorGrade"),
            "other type ids untouched"
        );
        assert_eq!(
            v["timeline"]["layers"][0]["effects"][0]["effectType"].as_str(),
            Some("WireframeDepth")
        );
        assert_eq!(
            v["timeline"]["layers"][0]["clips"][0]["effects"][0]["effectType"].as_str(),
            Some("WireframeDepth")
        );
    }
}
