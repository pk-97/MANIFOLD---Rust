//! Tests for `instance_serde.rs`, split out so the serde file stays under its
//! god-file ceiling.
use super::*;
use super::super::test_support::*;

// ── PresetInstance `params` wire format (V1.4, PARAM_STORAGE_DESIGN.md section 4) ──
//
// The typed (de)serialize understands ONLY the id-keyed `params` map —
// files below the 1.11.0 floor, which carried the older `paramValues`
// shapes, are refused by the loader. These tests cover the V1.4 shape itself, `base` folding, and unregistered-type degradation.
//
// "TestCreateDefaultUntouched" (registered below, single param
// "amount", default 0.42) and "TestTwoParamRoundTrip" (registered
// below, "alpha"/"beta") stand in for a real bundled effect — Rust
// module items are visible regardless of declaration order.

#[test]
fn effect_instance_deserialize_v14_params_map() {
    let json = r#"{
        "id": "abc12345",
        "effectType": "TestCreateDefaultUntouched",
        "enabled": true,
        "collapsed": false,
        "params": { "amount": { "value": 0.75, "exposed": true, "base": 0.5 } }
    }"#;
    let fx: PresetInstance = serde_json::from_str(json).unwrap();
    assert_eq!(fx.params.len(), 1);
    let amount = fx.params.get("amount").unwrap();
    assert!((amount.value - 0.75).abs() < f32::EPSILON);
    assert!(amount.exposed);
    // `base` present on the entry → base_tracked, folded into the entry.
    assert!(fx.base_tracked);
    assert!((amount.base - 0.5).abs() < f32::EPSILON);
}

#[test]
fn effect_instance_deserialize_v14_params_without_base_leaves_base_untracked() {
    let json = r#"{
        "effectType": "TestCreateDefaultUntouched",
        "enabled": true,
        "collapsed": false,
        "params": { "amount": { "value": 0.75 } }
    }"#;
    let fx: PresetInstance = serde_json::from_str(json).unwrap();
    assert!(!fx.base_tracked);
    // exposed defaults to true when the key is absent from the entry.
    assert!(fx.params.get("amount").unwrap().exposed);
}

#[test]
fn effect_instance_deserialize_params_without_registry_keeps_state() {
    // No registry def for this type → the template is UNRESOLVABLE,
    // which is not the same as "this id was deprecated by its template".
    // Dropping here was the BUG-036 class (a project-local preset's
    // template registers after layer deserialize under the wrong load
    // order); the entry is kept on a placeholder spec instead, so no
    // param state is ever lost to a missing template.
    let json = r#"{
        "effectType": "TotallyUnregisteredEffectType",
        "enabled": true,
        "collapsed": false,
        "params": { "amount": { "value": 0.7 } }
    }"#;
    let fx: PresetInstance = serde_json::from_str(json).unwrap();
    assert_eq!(fx.params.len(), 1, "entry kept despite missing template");
    let p = fx.params.get("amount").unwrap();
    assert!((p.value - 0.7).abs() < f32::EPSILON);
    // Placeholder spec carries identity; a later load with the template
    // present reconciles the real descriptor (only state serializes for
    // a bundled-origin param).
    assert_eq!(p.spec.id, "amount");
}

#[test]
fn effect_instance_serialize_omits_params_without_registry() {
    // No registry def and no user-added tail → `params` has nothing to
    // key its entries by, so it serializes empty. This is the honest
    // consequence of deleting the positional fallback (D4): an
    // unregistered type's values are not addressable, so they are not
    // written, rather than dumped into an array nothing can read back
    // by id. In production this path is unreachable (every shipping
    // effect is registered).
    let fx = PresetInstance {
        kind: crate::preset_def::PresetKind::Effect,
        id: EffectId::new("abc12345"),
        effect_type: PresetTypeId::from_string("TotallyUnregisteredEffectType".to_string()),
        enabled: true,
        collapsed: false,
        // Post-manifest (D4): there is no "unaddressable positional values"
        // failure mode — every `Param` is self-describing by id. An
        // unregistered type seeds an EMPTY manifest (no template), so
        // `params` serializes empty; the instance is never lost.
        params: crate::params::ParamManifest::default(),
        base_tracked: false,
        pending_wire: None,
        drivers: None,
        envelopes: None,
        ableton_mappings: None,
        audio_mods: None,
        automation_lanes: None,
        group_id: None,
        graph: None,
        graph_version: 0,
        graph_structure_version: 0,
        folded_sections: Vec::new(),
        unfolded_sections: Vec::new(),
        scene: None,
    };
    let json = serde_json::to_string(&fx).unwrap();
    assert!(
        json.contains("\"params\":{}"),
        "unregistered type must serialize an empty params map, not lose the instance; got: {json}"
    );
}

#[test]
fn effect_instance_serialize_round_trips_hidden_and_visible_params() {
    let fx = PresetInstance {
        kind: crate::preset_def::PresetKind::Effect,
        id: EffectId::new("abc12345"),
        effect_type: PresetTypeId::from_string("TestTwoParamRoundTrip".to_string()),
        enabled: true,
        collapsed: false,
        params: crate::params::ParamManifest::from_params(vec![
            slot("alpha", 0.1, true),
            slot("beta", 0.2, false),
        ]),
        base_tracked: false,
        pending_wire: None,
        drivers: None,
        envelopes: None,
        ableton_mappings: None,
        audio_mods: None,
        automation_lanes: None,
        group_id: None,
        graph: None,
        graph_version: 0,
        graph_structure_version: 0,
        folded_sections: Vec::new(),
        unfolded_sections: Vec::new(),
        scene: None,
    };
    let json = serde_json::to_string(&fx).unwrap();
    assert!(json.contains("\"alpha\":{\"value\":0.1,\"exposed\":true}"));
    assert!(json.contains("\"beta\":{\"value\":0.2,\"exposed\":false}"));
    let back: PresetInstance = serde_json::from_str(&json).unwrap();
    assert_eq!(back.params.len(), 2);
    let a = back.params.get("alpha").unwrap();
    assert_eq!(a.value, 0.1);
    assert!(a.exposed);
    let b = back.params.get("beta").unwrap();
    assert_eq!(b.value, 0.2);
    assert!(!b.exposed);
}

#[test]
fn trigger_source_effect_roundtrip_reconcile_preserves_disabled_and_missing_lane() {
    let missing_layer = crate::LayerId::new("layer-that-is-not-present");
    let own = slot("own", 0.1, true);
    let mut disabled = slot("disabled", 0.2, true);
    let mut lane = slot("lane", 0.3, true);
    disabled.clip_trigger_source = crate::params::ClipTriggerSource::Disabled;
    lane.clip_trigger_source = crate::params::ClipTriggerSource::Lane {
        layer_id: missing_layer.clone(),
    };

    let mut fx = PresetInstance::new(PresetTypeId::from_string(
        "UnregisteredTriggerSourceEffect".to_string(),
    ));
    fx.params = crate::params::ParamManifest::from_params(vec![own, disabled, lane]);
    let json = serde_json::to_string(&fx).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(wire["params"]["own"]["clipTriggerSource"].is_null());
    assert_eq!(
        wire["params"]["disabled"]["clipTriggerSource"]["kind"],
        "disabled"
    );
    assert_eq!(
        wire["params"]["lane"]["clipTriggerSource"],
        serde_json::json!({"kind": "lane", "layerId": "layer-that-is-not-present"})
    );

    let mut back: PresetInstance = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.params.get("own").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::OwnLayer
    );
    assert_eq!(
        back.params.get("disabled").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Disabled
    );
    assert_eq!(
        back.params.get("lane").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Lane {
            layer_id: missing_layer.clone()
        }
    );

    back.reconcile_manifest();
    assert_eq!(
        back.params.get("lane").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Lane {
            layer_id: missing_layer
        }
    );
}

#[test]
fn trigger_source_generator_roundtrip_preserves_disabled_and_missing_lane() {
    let missing_layer = crate::LayerId::new("missing-generator-lane");
    let own = slot("own", 0.1, true);
    let mut disabled = slot("disabled", 0.2, true);
    let mut lane = slot("lane", 0.3, true);
    disabled.clip_trigger_source = crate::params::ClipTriggerSource::Disabled;
    lane.clip_trigger_source = crate::params::ClipTriggerSource::Lane {
        layer_id: missing_layer.clone(),
    };

    let mut generator = PresetInstance::new_generator(PresetTypeId::from_string(
        "UnregisteredTriggerSourceGenerator".to_string(),
    ));
    generator.params = crate::params::ParamManifest::from_params(vec![own, disabled, lane]);
    let json = serde_json::to_string(&generator).unwrap();
    let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(wire["params"]["own"]["clipTriggerSource"].is_null());
    assert_eq!(
        wire["params"]["disabled"]["clipTriggerSource"]["kind"],
        "disabled"
    );
    assert_eq!(
        wire["params"]["lane"]["clipTriggerSource"],
        serde_json::json!({"kind": "lane", "layerId": "missing-generator-lane"})
    );

    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let mut back = deserialize_generator_instance(&mut deserializer).unwrap();
    assert_eq!(
        back.params.get("own").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::OwnLayer
    );
    assert_eq!(
        back.params.get("disabled").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Disabled
    );
    assert_eq!(
        back.params.get("lane").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Lane {
            layer_id: missing_layer.clone()
        }
    );

    back.reconcile_manifest();
    assert_eq!(
        back.params.get("lane").unwrap().clip_trigger_source,
        crate::params::ClipTriggerSource::Lane {
            layer_id: missing_layer
        }
    );
}

#[test]
fn trigger_source_survives_graph_manifest_refresh() {
    use crate::params::ClipTriggerSource;

    let mut generator = PresetInstance::new_generator(PresetTypeId::new("TriggerSourceGraph"));
    let mut param = slot("force", 0.4, true);
    param.clip_trigger_source = ClipTriggerSource::Lane {
        layer_id: crate::LayerId::new("force-pattern"),
    };
    generator.graph = Some(serde_json::from_value(serde_json::json!({
        "version": 2,
        "nodes": [],
        "wires": [],
        "presetMetadata": {
            "id": "TriggerSourceGraph", "displayName": "Test", "category": "Test",
            "oscPrefix": "test", "params": [param.spec.clone()], "bindings": []
        }
    })).unwrap());
    generator.params = crate::params::ParamManifest::from_params(vec![param.clone()]);

    generator.refresh_manifest_from_graph();
    assert_eq!(generator.params.get("force").unwrap().clip_trigger_source, param.clip_trigger_source);
    let json = serde_json::to_string(&generator).unwrap();
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let mut loaded = deserialize_generator_instance(&mut deserializer).unwrap();
    loaded.reconcile_manifest();
    assert!(!loaded.template_unresolved());
    assert_eq!(loaded.params.get("force").unwrap().clip_trigger_source, param.clip_trigger_source);
}

#[test]
fn effect_instance_skip_serializing_optional_none() {
    let fx = PresetInstance::new(PresetTypeId::from_string("TestEffect".to_string()));
    let json = serde_json::to_string(&fx).unwrap();
    // `params` always emits (even empty); `base` never appears on any
    // entry for a fresh, untouched instance.
    assert!(json.contains("\"params\":{}"));
    assert!(!json.contains("\"base\":"));
    assert!(!json.contains("\"drivers\""));
    assert!(!json.contains("\"abletonMappings\""));
    assert!(!json.contains("\"groupId\""));
    assert!(!json.contains("\"param0\""));
    // After the binding-storage unification there is no separate
    // `userParamBindings` field at all — user bindings live in the
    // graph. A fresh effect has no graph, so nothing extra emits and
    // existing fixtures round-trip byte-identically.
    assert!(!json.contains("\"userParamBindings\""));
}

// ── Map deserialize alias-aware path (step 15) ────────────────

#[test]
fn params_map_deserialize_drops_unknown_id() {
    // Without any alias entries, an unknown id is silently dropped.
    // This is the orphan policy — same as drivers/envelopes/Ableton.
    let json = r#"{
        "id": "abc12345",
        "effectType": "TestCreateDefaultUntouched",
        "enabled": true,
        "collapsed": false,
        "params": { "amount": { "value": 0.7 }, "old_phantom_param": { "value": 0.5 } }
    }"#;
    let fx: PresetInstance = serde_json::from_str(json).unwrap();
    // amount resolves via the registry; old_phantom_param has nowhere
    // to go (not static, not a user-added tail id) and is dropped.
    assert_eq!(fx.params.len(), 1);
    assert!((fx.params.get("amount").unwrap().value - 0.7).abs() < f32::EPSILON);
}

#[test]
fn effect_instance_round_trip_with_user_bindings_against_bloom() {
    // Bloom is registered in this crate's tests with one param
    // `amount`. Add two user bindings and verify the whole
    // PresetInstance round-trips through JSON, including the
    // user-binding tail values landing in the right param_values
    // slots regardless of JSON key ordering.
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]); // static prefix
    fx.append_user_binding(sample_user_binding(
        "user.uv_transform.translate.1",
        "uv_transform",
        "translate",
    ));
    fx.append_user_binding(sample_user_binding("user.mix.amount.1", "mix", "amount"));
    // After append, the manifest should carry [amount=0.7, translate=0.25, mix.amount=0.25].
    assert_eq!(fx.params.len(), 3);
    assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
    assert_eq!(fx.params.get("user.uv_transform.translate.1").unwrap().value, 0.25);
    assert_eq!(fx.params.get("user.mix.amount.1").unwrap().value, 0.25);
    // Tweak the user-tail values to verify they round-trip.
    fx.params.get_mut("user.uv_transform.translate.1").unwrap().value = 0.42;
    fx.params.get_mut("user.mix.amount.1").unwrap().value = 0.91;

    let json = serde_json::to_string(&fx).unwrap();
    // User bindings now ride out inside the per-instance `graph`
    // (preset_metadata.bindings, userAdded), not a separate array.
    assert!(json.contains("\"graph\""));
    assert!(json.contains("\"userAdded\":true"));
    // V1.3 wire emits {value, exposed} objects per entry; the
    // param_values tail is keyed by the user-binding id.
    assert!(json.contains("\"amount\":{\"value\":0.7,\"exposed\":true}"));
    assert!(json.contains("\"user.uv_transform.translate.1\":{\"value\":0.42"));
    assert!(json.contains("\"user.mix.amount.1\":{\"value\":0.91"));

    let back: PresetInstance = serde_json::from_str(&json).unwrap();
    let back_bindings = back.user_param_bindings();
    assert_eq!(back_bindings.len(), 2);
    assert_eq!(back_bindings[0].id, "user.uv_transform.translate.1");
    assert_eq!(back_bindings[1].id, "user.mix.amount.1");
    assert_eq!(back.params.len(), 3);
    assert_eq!(back.params.get("amount").unwrap().value, 0.7);
    assert_eq!(back.params.get("user.uv_transform.translate.1").unwrap().value, 0.42);
    assert_eq!(back.params.get("user.mix.amount.1").unwrap().value, 0.91);
}

#[test]
fn graph_field_skipped_when_none() {
    // Existing fixtures (Liveschool, Burn) must
    // continue to round-trip byte-identically — the new field
    // must not appear in their JSON unless explicitly set.
    let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
    let json = serde_json::to_string(&fx).unwrap();
    assert!(
        !json.contains("\"graph\""),
        "graph field must be skipped when None — got: {json}"
    );
}

#[test]
fn graph_field_round_trips_when_present() {
    use crate::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
        SerializedParamValue,
    };

    let mut params = std::collections::BTreeMap::new();
    params.insert("mode".to_string(), SerializedParamValue::Enum { value: 7 });

    let def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![
            EffectGraphNode {
                id: 0,
                node_id: crate::NodeId::default(),
                type_id: "system.source".to_string(),
                handle: Some("source".to_string()),
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: std::collections::BTreeMap::new(),
                output_canvas_scales: std::collections::BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 1,
                node_id: crate::NodeId::default(),
                type_id: "node.transform".to_string(),
                handle: Some("uv_transform".to_string()),
                params,
                exposed_params: Default::default(),
                editor_pos: Some((100.0, 200.0)),
                wgsl_source: None,
                title: None,
                output_formats: std::collections::BTreeMap::new(),
                output_canvas_scales: std::collections::BTreeMap::new(),
                group: None,
            },
        ],
        wires: vec![EffectGraphWire {
            from_node: 0,
            from_port: "out".to_string(),
            to_node: 1,
            to_port: "source".to_string(),
        }],
    };

    let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
    fx.graph = Some(def.clone());

    let json = serde_json::to_string(&fx).unwrap();
    assert!(json.contains("\"graph\""));

    let back: PresetInstance = serde_json::from_str(&json).unwrap();
    assert_eq!(back.graph, Some(def));
    // `graph_version` is not serialized — it resets on load.
    assert_eq!(back.graph_version, 0);
}

#[test]
fn legacy_fixture_without_graph_field_still_loads() {
    // Pre-Phase-1 fixtures have no `graph` field at all. Loading
    // them must succeed with `graph: None`.
    let json = r#"{
        "id": "abc12345",
        "effectType": "Mirror",
        "enabled": true,
        "collapsed": false,
        "params": { "amount": { "value": 1.0, "exposed": true } }
    }"#;
    let fx: PresetInstance = serde_json::from_str(json).unwrap();
    assert!(fx.graph.is_none());
}

// ─── PresetInstance.automation_lanes serde (skip-when-empty) ───

#[test]
fn preset_instance_without_automation_lanes_serializes_byte_identical() {
    // No lanes → no `automationLanes` key at all, and round-tripping a
    // fixture that never had lanes must not introduce one. Same
    // skip-when-empty convention as `drivers`/`envelopes`/`audioMods`.
    let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
    assert!(fx.automation_lanes.is_none());
    let json = serde_json::to_string(&fx).unwrap();
    assert!(
        !json.contains("automationLanes"),
        "no automation_lanes → no key on the wire; got: {json}"
    );
    let back: PresetInstance = serde_json::from_str(&json).unwrap();
    assert!(back.automation_lanes.is_none());
}

#[test]
fn preset_instance_automation_lanes_roundtrip_when_present() {
    let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
    fx.automation_lanes = Some(vec![AutomationLane {
        param_id: ParamId::from("amount"),
        enabled: true,
        points: vec![pt(0.0, 0.25, SegmentShape::Curved(0.5))],
    }]);
    let json = serde_json::to_string(&fx).unwrap();
    assert!(json.contains("automationLanes"));
    let back: PresetInstance = serde_json::from_str(&json).unwrap();
    let lanes = back.automation_lanes.expect("lanes round-trip");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].param_id, ParamId::from("amount"));
    assert!(lanes[0].enabled);
    assert_eq!(lanes[0].points.len(), 1);
    assert_eq!(lanes[0].points[0].value, 0.25);
    assert_eq!(lanes[0].points[0].shape, SegmentShape::Curved(0.5));
}

#[test]
fn legacy_audio_trigger_migrates_onto_a_parameter_audio_mod_on_the_gate_param() {
    // The exact `audioTrigger` shape a project saved during the one day
    // section 8's `AudioTriggerMod` shipped (see
    // `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` section 9 U5). A generator-kind
    // instance's `graph.presetMetadata.params` is the only route to an
    // `is_trigger_gate` param outside the JSON preset path (the
    // compile-time `ParamSpec` inventory format has no field for it —
    // see `generator_registration::ParamSpec::to_param_def`), so the
    // fixture carries its own minimal per-instance graph.
    let json = r#"{
        "generatorType": "TestGenTrig",
        "graph": {
            "version": 2,
            "presetMetadata": {
                "id": "TestGenTrig",
                "displayName": "Test Gen Trig",
                "category": "Test",
                "oscPrefix": "testGenTrig",
                "params": [
                    {
                        "id": "clip_trigger",
                        "name": "Clip Trigger",
                        "min": 0.0,
                        "max": 1.0,
                        "defaultValue": 0.0,
                        "isToggle": true,
                        "isTriggerGate": true
                    }
                ],
                "bindings": []
            },
            "nodes": [],
            "wires": []
        },
        "audioTrigger": {
            "enabled": false,
            "source": {
                "sendId": "e14b42f8",
                "feature": { "kind": "transients", "band": "full" }
            },
            "sensitivity": 1.0,
            "mode": "transient"
        }
    }"#;

    let mut de = serde_json::Deserializer::from_str(json);
    let inst = deserialize_generator_instance(&mut de).unwrap();

    assert_eq!(inst.kind, crate::preset_def::PresetKind::Generator);
    let mods = inst
        .audio_mods
        .as_ref()
        .expect("legacy audioTrigger must migrate onto audio_mods");
    assert_eq!(mods.len(), 1);
    let m = &mods[0];
    assert_eq!(m.param_id.as_ref(), "clip_trigger", "targets the gate param");
    assert!(!m.enabled, "legacy enabled=false carries over");
    assert_eq!(m.source.send_id, crate::id::AudioSendId::new("e14b42f8"));
    assert_eq!(
        m.source.feature,
        crate::audio_mod::AudioFeature::new(
            crate::audio_mod::AudioFeatureKind::Transients,
            crate::audio_mod::AudioBand::Full
        )
    );
    assert_eq!(
        m.trigger_mode,
        Some(crate::audio_trigger::TriggerFireMode::Transient)
    );
    assert_eq!(m.shape.sensitivity, 1.0, "sensitivity approximates onto Amount (U5)");
}

#[test]
fn legacy_audio_trigger_with_no_gate_param_is_dropped_not_guessed() {
    // No `isTriggerGate` param anywhere on the instance → the migration
    // has no target to attach to and must drop the config rather than
    // guess one (a hand-edited file, or an instance saved before the
    // flag existed).
    let json = r#"{
        "generatorType": "TestGenTrigNoGate",
        "audioTrigger": {
            "enabled": true,
            "source": {
                "sendId": "send-1",
                "feature": { "kind": "transients", "band": "full" }
            },
            "sensitivity": 0.5,
            "mode": "both"
        }
    }"#;

    let mut de = serde_json::Deserializer::from_str(json);
    let inst = deserialize_generator_instance(&mut de).unwrap();
    assert!(inst.audio_mods.is_none(), "no gate param means nothing to migrate onto");
}

/// BUG-jxy0p (duplicate param ids at load): a saved graph that declares one
/// card id twice, and a renamed entry (`old` to `amount`) landing on that id,
/// load as one param holding the saved value. A debug build used to panic in
/// `ParamManifest::from_params`; a release build kept two params.
#[test]
fn a_rename_landing_on_a_repeated_id_loads_one_param() {
    let spec = r#"{"id": "amount", "name": "Amount", "min": 0.0, "max": 10.0, "defaultValue": 1.0}"#;
    let json = format!(
        r#"{{
        "generatorType": "TestGenDuplicateIds",
        "graph": {{
            "version": 2,
            "presetMetadata": {{
                "id": "TestGenDuplicateIds", "displayName": "Dup", "category": "Test", "oscPrefix": "dup",
                "params": [{spec}, {spec}],
                "bindings": [],
                "paramAliases": [{{"old": "old", "new": "amount"}}]
            }},
            "nodes": [],
            "wires": []
        }},
        "params": {{"old": {{"value": 7.0, "exposed": true}}}}
    }}"#
    );
    let mut de = serde_json::Deserializer::from_str(&json);
    let inst = deserialize_generator_instance(&mut de).unwrap();
    let amounts: Vec<f32> = inst.params.iter().filter(|p| p.id() == "amount").map(|p| p.value).collect();
    assert_eq!(amounts, vec![7.0]);
}
