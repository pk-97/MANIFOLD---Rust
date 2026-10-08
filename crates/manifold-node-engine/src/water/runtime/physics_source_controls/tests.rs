use super::digest;
use manifold_core::ableton_mapping::{
    AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
};
use manifold_core::audio_mod::ParameterAudioMod;
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind};
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{
    AutomationLane, AutomationPoint, ParamEnvelope, ParameterDriver, PresetInstance, SegmentShape,
};
use manifold_core::macro_bank::MacroCurve;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{AudioSendId, Beats, PresetTypeId};

fn spec(id: &str, default: f32) -> ParamSpecDef {
    ParamSpecDef {
        id: id.into(),
        name: format!("{id} label"),
        min: 0.0,
        max: 1.0,
        default_value: default,
        ..Default::default()
    }
}

fn instance() -> PresetInstance {
    let mut instance = PresetInstance::new(PresetTypeId::new("TestPhysicsControls"));
    instance.params = ParamManifest::from_params(vec![
        Param::user_added(spec("amount", 0.25)),
        Param::user_added(spec("trigger", 0.0)),
    ]);
    instance.base_tracked = true;
    instance.set_base_param("amount", 0.4);
    instance.set_base_param("trigger", 0.0);
    instance
}

fn ids() -> Vec<String> {
    vec!["amount".into(), "trigger".into()]
}

fn digest_of(instance: &PresetInstance) -> [u8; 32] {
    digest(&ids(), instance).expect("valid physics control identity")
}

fn mapping(param_id: &'static str) -> AbletonParamMapping {
    AbletonParamMapping {
        param_id: param_id.into(),
        address: AbletonMacroAddress {
            track_id: 1,
            device_id: 2,
            param_id: 3,
            device_identity: AbletonDeviceIdentity {
                device_class_name: "InstrumentGroupDevice".into(),
            },
            track_name: "Track".into(),
            device_name: "Device".into(),
            macro_name: "Macro 1".into(),
        },
        range_min: 0.1,
        range_max: 0.9,
        inverted: false,
        legacy_param_index: None,
        last_value: 0.2,
        status: AbletonMappingStatus::Active,
    }
}

#[test]
fn authored_base_and_semantic_spec_change_identity() {
    let original = instance();
    let before = digest_of(&original);

    let mut base = original.clone();
    base.set_base_param("amount", 0.5);
    assert_ne!(digest_of(&base), before);

    let mut effective = original.clone();
    effective.set_param("amount", 0.8);
    assert_eq!(digest_of(&effective), before);

    let mut label = original.clone();
    label.params.get_mut("amount").unwrap().spec.name = "renamed".into();
    assert_eq!(digest_of(&label), before);

    let mut reordered = original.clone();
    let params = vec![
        reordered.params.remove("trigger").unwrap(),
        reordered.params.remove("amount").unwrap(),
    ];
    reordered.params = ParamManifest::from_params(params);
    assert_eq!(digest_of(&reordered), before);

    let mut semantics = original;
    semantics.params.get_mut("amount").unwrap().spec.curve = MacroCurve::Exponential;
    assert_ne!(digest_of(&semantics), before);
}

#[test]
fn missing_and_present_params_are_distinct() {
    let mut missing = instance();
    missing.params.remove("amount");
    assert_ne!(digest_of(&missing), digest_of(&instance()));
}

#[test]
fn unrelated_params_and_control_rows_do_not_invalidate() {
    let original = instance();
    let before = digest_of(&original);

    let mut unrelated_param = original.clone();
    unrelated_param
        .params
        .push(Param::user_added(spec("other", 0.1)));
    unrelated_param.params.get_mut("other").unwrap().base = 0.8;
    assert_eq!(digest_of(&unrelated_param), before);

    let mut unrelated_driver = original.clone();
    let mut driver = ParameterDriver::new("other", BeatDivision::Quarter, DriverWaveform::Sine);
    driver.phase = 0.7;
    unrelated_driver.drivers = Some(vec![driver]);
    assert_eq!(digest_of(&unrelated_driver), before);
}

#[test]
fn untracked_base_uses_effective_value_as_the_fallback() {
    let mut original = instance();
    original.base_tracked = false;
    original.params.get_mut("amount").unwrap().base = 0.1;
    original.params.get_mut("amount").unwrap().value = 0.4;
    let before = digest_of(&original);
    original.params.get_mut("amount").unwrap().value = 0.5;
    assert_ne!(digest_of(&original), before);
}

#[test]
fn all_five_control_kinds_hash_authored_fields_and_ignore_runtime_fields() {
    let mut original = instance();
    original.drivers = Some(vec![ParameterDriver::new(
        "amount",
        BeatDivision::Quarter,
        DriverWaveform::Sine,
    )]);
    original.envelopes = Some(vec![ParamEnvelope::new("amount")]);
    original.audio_mods = Some(vec![ParameterAudioMod::new(
        "amount".into(),
        AudioSendId::new("send"),
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    )]);
    original.automation_lanes = Some(vec![AutomationLane {
        param_id: "amount".into(),
        enabled: true,
        points: vec![AutomationPoint {
            beat: Beats(0.0),
            value: 0.3,
            shape: SegmentShape::Linear,
        }],
    }]);
    original.ableton_mappings = Some(vec![mapping("amount")]);
    let before = digest_of(&original);

    let mut runtime = original.clone();
    runtime.params.get_mut("amount").unwrap().value = 0.99;
    runtime.drivers.as_mut().unwrap()[0].is_paused_by_user = true;
    runtime.envelopes.as_mut().unwrap()[0].fire_count = 4;
    runtime.audio_mods.as_mut().unwrap()[0].fire_count = 7;
    runtime.audio_mods.as_mut().unwrap()[0].smoothed = 0.8;
    runtime.automation_lanes.as_mut().unwrap()[0].points[0].value = 0.6;
    runtime.ableton_mappings.as_mut().unwrap()[0].last_value = 0.8;
    assert_ne!(
        digest_of(&runtime),
        before,
        "automation authored points are semantic"
    );

    let mut runtime_only = original.clone();
    runtime_only.params.get_mut("amount").unwrap().value = 0.99;
    runtime_only.drivers.as_mut().unwrap()[0].is_paused_by_user = true;
    runtime_only.envelopes.as_mut().unwrap()[0].fire_count = 4;
    runtime_only.audio_mods.as_mut().unwrap()[0].fire_count = 7;
    runtime_only.audio_mods.as_mut().unwrap()[0].smoothed = 0.8;
    runtime_only.ableton_mappings.as_mut().unwrap()[0].last_value = 0.8;
    assert_eq!(digest_of(&runtime_only), before);

    let mut driver = original.clone();
    driver.drivers.as_mut().unwrap()[0].phase = 0.2;
    assert_ne!(digest_of(&driver), before);
    let mut envelope = original.clone();
    envelope.envelopes.as_mut().unwrap()[0].decay_beats = 2.0;
    assert_ne!(digest_of(&envelope), before);
    let mut audio = original.clone();
    audio.audio_mods.as_mut().unwrap()[0].shape.invert = true;
    assert_ne!(digest_of(&audio), before);
    let mut lane = original.clone();
    lane.automation_lanes.as_mut().unwrap()[0].points[0].value = 0.6;
    assert_ne!(digest_of(&lane), before);
    let mut ableton = original;
    ableton.ableton_mappings.as_mut().unwrap()[0].range_max = 0.8;
    assert_ne!(digest_of(&ableton), before);
}

#[test]
fn automation_and_ableton_sampled_bases_do_not_invalidate() {
    let mut original = instance();
    original.automation_lanes = Some(vec![AutomationLane {
        param_id: "amount".into(),
        enabled: true,
        points: vec![],
    }]);
    original.ableton_mappings = Some(vec![mapping("trigger")]);
    let before = digest_of(&original);

    original.params.get_mut("amount").unwrap().base = 0.8;
    original.params.get_mut("trigger").unwrap().base = 0.7;
    assert_eq!(digest_of(&original), before);
}

#[test]
fn trigger_counter_and_trigger_effective_value_do_not_invalidate() {
    let mut original = instance();
    original.params.get_mut("trigger").unwrap().spec.is_trigger = true;
    let before = digest_of(&original);
    original.params.get_mut("trigger").unwrap().base = 0.8;
    original.params.get_mut("trigger").unwrap().value = 0.9;
    assert_eq!(digest_of(&original), before);
}

#[test]
fn ableton_display_metadata_and_status_are_not_semantic() {
    let mut original = instance();
    original.ableton_mappings = Some(vec![mapping("amount")]);
    let before = digest_of(&original);
    let row = &mut original.ableton_mappings.as_mut().unwrap()[0];
    row.address.track_name = "Renamed".into();
    row.address.device_name = "Renamed device".into();
    row.address.macro_name = "Renamed macro".into();
    row.status = AbletonMappingStatus::Dormant;
    row.last_value = 0.91;
    assert_eq!(digest_of(&original), before);
}

#[test]
fn valid_serialize_reload_keeps_identity() {
    let original = instance();
    let json = serde_json::to_string(&original).expect("serialize instance");
    let reloaded: PresetInstance = serde_json::from_str(&json).expect("reload instance");
    assert_eq!(digest_of(&reloaded), digest_of(&original));
}

#[test]
fn unresolved_legacy_mapping_is_rejected() {
    let mut original = instance();
    let mut driver = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
    driver.legacy_param_index = Some(2);
    original.drivers = Some(vec![driver]);
    let error = digest(&ids(), &original).expect_err("legacy identity must be explicit");
    assert!(error.contains("legacy driver"));
}

#[test]
fn nonfinite_base_and_spec_values_are_rejected() {
    let mut base = instance();
    base.params.get_mut("amount").unwrap().base = f32::NAN;
    let error = digest(&ids(), &base).expect_err("non-finite base must be rejected");
    assert!(error.contains("non-finite base"));

    let mut spec = instance();
    spec.params.get_mut("amount").unwrap().spec.max = f32::INFINITY;
    let error = digest(&ids(), &spec).expect_err("non-finite spec must be rejected");
    assert!(error.contains("non-finite max"));
}
