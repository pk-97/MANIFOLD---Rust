use super::*;
use super::super::test_support::*;
use crate::PresetTypeId;
use crate::effects::{ParamId, PresetInstance};

fn amplitude_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
    crate::audio_mod::ParameterAudioMod::new(
        ParamId::from("amount"),
        send_id,
        crate::audio_mod::AudioFeature::new(
            crate::audio_mod::AudioFeatureKind::Amplitude,
            crate::audio_mod::AudioBand::Full,
        ),
    )
}

#[test]
fn analysis_consumed_sends_includes_only_the_send_with_an_enabled_mod() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    let send_b = send_with_id("B", "send-b");
    p.audio_setup.sends.push(send_a.clone());
    p.audio_setup.sends.push(send_b.clone());

    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
    p.settings.master_effects.push(fx);

    let consumed = p.analysis_consumed_sends();
    assert_eq!(consumed.len(), 1);
    assert!(consumed.contains(&send_a.id));
    assert!(!consumed.contains(&send_b.id));
}

#[test]
fn analysis_consumed_sends_is_empty_when_the_only_mod_is_disabled() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());

    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    let m = fx.audio_mods_mut();
    m.push(amplitude_mod(send_a.id.clone()));
    m[0].enabled = false;
    p.settings.master_effects.push(fx);

    assert!(p.analysis_consumed_sends().is_empty());
}

#[test]
fn analysis_consumed_sends_includes_send_with_enabled_layer_clip_trigger_and_no_mod() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());
    p.timeline.layers.push(layer_with_clip_trigger(
        send_a.id.clone(),
        crate::audio_mod::AudioBand::Low,
        true,
    ));

    let consumed = p.analysis_consumed_sends();
    assert_eq!(consumed.len(), 1);
    assert!(consumed.contains(&send_a.id));
}

#[test]
fn analysis_consumed_sends_excludes_send_with_disabled_layer_clip_trigger() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());
    // `enabled: false` — layer_with_clip_trigger's third arg.
    p.timeline.layers.push(layer_with_clip_trigger(
        send_a.id.clone(),
        crate::audio_mod::AudioBand::Low,
        false,
    ));

    assert!(p.analysis_consumed_sends().is_empty());
}

#[test]
fn analysis_consumed_sends_ignores_drained_legacy_send_triggers() {
    // section 3.4: `send.triggers` is deserialize-only legacy storage now —
    // even if something hand-populates it (bypassing the load
    // migration), `analysis_consumed_sends` must never read it again.
    let mut p = Project::default();
    let mut send_a = send_with_id("A", "send-a");
    let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Low);
    route.enabled = true;
    send_a.triggers.push(route);
    p.audio_setup.sends.push(send_a);

    assert!(p.analysis_consumed_sends().is_empty());
}

#[test]
fn has_active_clip_triggers_true_only_when_some_layer_has_an_enabled_config() {
    let mut p = Project::default();
    assert!(!p.has_active_clip_triggers());

    p.timeline.layers.push(layer_with_clip_trigger(
        crate::AudioSendId::new("send-a"),
        crate::audio_mod::AudioBand::Low,
        false,
    ));
    assert!(!p.has_active_clip_triggers(), "disabled config doesn't count");

    p.timeline.layers[0].clip_triggers[0].enabled = true;
    assert!(p.has_active_clip_triggers());
}

/// A `clip_trigger`-shaped bundled param, `is_trigger_gate` set — the
/// only thing project.rs's own `slot`-less test module needs to build a
/// trigger-gate card by hand (mirrors `effects::tests::gate_slot`, kept
/// local since that helper is private to `effects.rs`'s own test
/// module).
fn gate_param(id: &str) -> crate::params::Param {
    let mut p = slot(id, 0.0, true);
    p.spec.name = "Clip Trigger".to_string();
    p.spec.is_toggle = true;
    p.spec.is_trigger_gate = true;
    p
}

fn armed_trigger_gate_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
    let mut m = crate::audio_mod::ParameterAudioMod::new(
        "clip_trigger".into(),
        send_id,
        crate::audio_mod::AudioFeature::new(
            crate::audio_mod::AudioFeatureKind::Transients,
            crate::audio_mod::AudioBand::Full,
        ),
    );
    m.trigger_mode = Some(crate::audio_trigger::TriggerFireMode::Transient);
    m
}

#[test]
fn armed_trigger_gate_mod_turns_the_analysis_gate_on_and_claims_its_send() {
    // Regression (2026-07-07, class-collapsed 2026-07-07 per section 9 U1/U4): a
    // project whose ONLY audio consumer is an armed fire-mode mod on a
    // trigger-gate param never started capture (has_active_audio_mods
    // false) and, even with capture running, its send was skipped by the
    // D4 gate (analysis_consumed_sends empty) — so armed audio triggers
    // silently never fired. section 9 deletes the second per-instance config
    // type that caused it; this test is the proof the plain `audio_mods`
    // walk covers a fire-mode mod with zero special-case code.
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());

    let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
    layer.layer_id = crate::LayerId::new("plasma-layer");
    let gp = layer.gen_params_or_init();
    gp.params.push(gate_param("clip_trigger"));
    gp.audio_mods_mut().push(armed_trigger_gate_mod(send_a.id.clone()));
    p.timeline.layers.push(layer);

    assert!(p.has_active_audio_mods(), "armed trigger-gate mod must start capture");
    let consumed = p.analysis_consumed_sends();
    assert!(consumed.contains(&send_a.id), "armed trigger's send must be analyzed");
    assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
    let consumers = p.audio_mod_consumers(&send_a.id);
    assert_eq!(consumers.len(), 1);
    assert!(
        consumers[0].1.contains("Clip Trigger"),
        "consumers list names the param the gate card lives on, not a bespoke 'Trigger' label; got {}",
        consumers[0].1
    );
}

#[test]
fn disarmed_trigger_gate_mod_does_not_gate_analysis_but_still_counts_as_send_usage() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());

    let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
    layer.layer_id = crate::LayerId::new("plasma-layer");
    let gp = layer.gen_params_or_init();
    gp.params.push(gate_param("clip_trigger"));
    let mut m = armed_trigger_gate_mod(send_a.id.clone());
    m.enabled = false;
    gp.audio_mods_mut().push(m);
    p.timeline.layers.push(layer);

    assert!(!p.has_active_audio_mods(), "disarmed mod must not run capture");
    assert!(p.analysis_consumed_sends().is_empty());
    // Usage matches the plain audio-mod semantics: the mod still
    // references the send whether enabled or not, so deleting it should
    // warn.
    assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
    assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "consumers lists armed only");
}

#[test]
fn audio_mod_consumers_resolves_layer_effect_and_param_names() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    p.audio_setup.sends.push(send_a.clone());

    let mut layer = crate::layer::Layer::new("BLOOM LAYER".into(), crate::types::LayerType::Video, 0);
    layer.layer_id = crate::LayerId::new("bloom-layer");
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
    layer.effects = Some(vec![fx]);
    p.timeline.layers.push(layer);

    let consumers = p.audio_mod_consumers(&send_a.id);
    assert_eq!(consumers.len(), 1);
    assert_eq!(consumers[0].0, Some(crate::LayerId::new("bloom-layer")));
    assert!(
        consumers[0].1.starts_with("BLOOM LAYER \u{2022} Bloom \u{2022} "),
        "label should read 'LayerName \u{2022} EffectName \u{2022} ParamName', got {}",
        consumers[0].1
    );
}

#[test]
fn audio_mod_consumers_excludes_disabled_mods_and_other_sends() {
    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    let send_b = send_with_id("B", "send-b");
    p.audio_setup.sends.push(send_a.clone());
    p.audio_setup.sends.push(send_b.clone());

    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    let mods = fx.audio_mods_mut();
    mods.push(amplitude_mod(send_a.id.clone()));
    mods[0].enabled = false;
    mods.push(amplitude_mod(send_b.id.clone()));
    p.settings.master_effects.push(fx);

    assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "disabled mod excluded");
    let b_consumers = p.audio_mod_consumers(&send_b.id);
    assert_eq!(b_consumers.len(), 1);
    assert_eq!(b_consumers[0].0, None, "master-effects mod has no owning layer");
    assert!(b_consumers[0].1.starts_with("Master \u{2022} "));
}

#[test]
fn clip_trigger_consumers_resolves_layer_and_band_label() {
    use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use crate::audio_trigger::LayerClipTrigger;

    let mut p = Project::default();
    let send_a = send_with_id("Kick", "send-a");
    p.audio_setup.sends.push(send_a.clone());

    let mut layer = crate::layer::Layer::new("STROBE".into(), crate::types::LayerType::Video, 0);
    layer.layer_id = crate::LayerId::new("strobe-layer");
    let mut cfg = LayerClipTrigger::new(AudioModSource {
        send_id: send_a.id.clone(),
        feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
    });
    cfg.enabled = true;
    layer.clip_triggers.push(cfg);
    p.timeline.layers.push(layer);

    let consumers = p.clip_trigger_consumers(&send_a.id);
    assert_eq!(consumers.len(), 1);
    assert_eq!(consumers[0].0, Some(crate::LayerId::new("strobe-layer")));
    assert_eq!(
        consumers[0].1,
        "Clip trigger \u{2022} STROBE \u{2022} Low",
        "Transients formats as the bare band label"
    );
}

#[test]
fn clip_trigger_consumers_excludes_disabled_configs_and_other_sends() {
    use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use crate::audio_trigger::LayerClipTrigger;

    let mut p = Project::default();
    let send_a = send_with_id("A", "send-a");
    let send_b = send_with_id("B", "send-b");
    p.audio_setup.sends.push(send_a.clone());
    p.audio_setup.sends.push(send_b.clone());

    let mut layer = crate::layer::Layer::new("L".into(), crate::types::LayerType::Video, 0);
    layer.layer_id = crate::LayerId::new("l1");
    // Disabled — excluded even though it sources send_a.
    let mut disabled = LayerClipTrigger::new(AudioModSource {
        send_id: send_a.id.clone(),
        feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
    });
    disabled.enabled = false;
    layer.clip_triggers.push(disabled);
    // Enabled but sources send_b — excluded from send_a's consumers.
    let mut other_send = LayerClipTrigger::new(AudioModSource {
        send_id: send_b.id.clone(),
        feature: AudioFeature::new(AudioFeatureKind::Centroid, AudioBand::Full),
    });
    other_send.enabled = true;
    layer.clip_triggers.push(other_send);
    p.timeline.layers.push(layer);

    assert!(p.clip_trigger_consumers(&send_a.id).is_empty(), "disabled config excluded");
    let b_consumers = p.clip_trigger_consumers(&send_b.id);
    assert_eq!(b_consumers.len(), 1);
    assert_eq!(
        b_consumers[0].1,
        "Clip trigger \u{2022} L \u{2022} Centroid Full",
        "non-Transients spells out the detector"
    );
}

#[test]
fn scene_modifier_target_reads_and_mutates_only_local_snapshot() {
    let mut project = Project::default();
    let mut host = graph_def_with_id("host", "Host");
    host.scene_modifiers.push(crate::scene_modifier_preset::SceneModifierInstanceDef {
        id: crate::NodeId::new("modifier"),
        scene: crate::scene_modifier_preset::SceneNodeRef {
            scope: Vec::new(),
            node: "scene".into(),
        },
        targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        graph: Box::new(graph_def_with_id("local", "Local")),
    });
    let mut layer = crate::layer::Layer::new_generator(
        "Generator".into(),
        PresetTypeId::new("PLASMA"),
        0,
    );
    layer.layer_id = crate::LayerId::new("layer");
    layer.gen_params_mut().unwrap().graph = Some(host);
    project.timeline.layers.push(layer);

    let target = crate::GraphTarget::SceneModifier {
        owner: Box::new(crate::GraphTarget::Generator(crate::LayerId::new("layer"))),
        modifier_id: crate::NodeId::new("modifier"),
    };
    assert!(project.preset_instance(&target).is_none());
    assert_eq!(project.instance_preset_id(&target).unwrap().as_str(), "local");
    let version = project.graph_target_owner(&target).unwrap().graph_version;
    project
        .with_graph_for_target_mut(&target, None, false, |graph| {
            graph.name = Some("Changed".into());
        })
        .expect("local snapshot resolves");
    assert_eq!(
        project.graph_for_target(&target, None).unwrap().name.as_deref(),
        Some("Changed")
    );
    assert_eq!(project.graph_target_owner(&target).unwrap().graph_version, version + 1);

    let new_id = PresetTypeId::new("local-renamed");
    assert!(project.set_instance_preset_id(&target, new_id.clone()));
    assert_eq!(project.instance_preset_id(&target).unwrap(), new_id);
    assert_eq!(
        project.graph_target_owner(&target).unwrap().generator_type().as_str(),
        "PLASMA"
    );
}

#[test]
fn scene_modifier_target_invalid_lookup_does_not_initialize_generator() {
    let mut project = Project::default();
    let mut layer = crate::layer::Layer::new(
        "Generator".into(),
        crate::types::LayerType::Generator,
        0,
    );
    layer.layer_id = crate::LayerId::new("layer");
    project.timeline.layers.push(layer);
    let target = crate::GraphTarget::SceneModifier {
        owner: Box::new(crate::GraphTarget::Generator(crate::LayerId::new("layer"))),
        modifier_id: crate::NodeId::new("missing"),
    };
    let default = graph_def_with_id("host", "Host");
    assert!(project
        .with_graph_for_target_mut(&target, Some(&default), true, |_| {})
        .is_none());
    assert!(project.timeline.layers[0].gen_params().is_none());
}

#[test]
fn scene_modifier_target_materializes_default_and_bumps_versions() {
    let mut project = Project::default();
    let mut layer = crate::layer::Layer::new(
        "Generator".into(),
        crate::types::LayerType::Generator,
        0,
    );
    layer.layer_id = crate::LayerId::new("layer");
    project.timeline.layers.push(layer);
    let target = crate::GraphTarget::Generator(crate::LayerId::new("layer"));
    let default = graph_def_with_id("default", "Default");

    project
        .with_graph_for_target_mut(&target, Some(&default), false, |graph| {
            graph.name = Some("Edited".into());
        })
        .expect("default host graph materializes");
    let owner = project.graph_target_owner(&target).unwrap();
    assert_eq!(owner.graph_version, 1);
    assert_eq!(owner.graph_structure_version, 0);
    assert_eq!(owner.graph.as_ref().unwrap().name.as_deref(), Some("Edited"));

    project
        .with_graph_for_target_mut(&target, None, true, |_| {})
        .expect("existing graph remains editable without a default");
    let owner = project.graph_target_owner(&target).unwrap();
    assert_eq!(owner.graph_version, 2);
    assert_eq!(owner.graph_structure_version, 1);
}

#[test]
fn scene_modifier_target_duplicate_lookup_is_atomic() {
    let mut project = Project::default();
    let mut host = graph_def_with_id("host", "Host");
    let modifier = crate::scene_modifier_preset::SceneModifierInstanceDef {
        id: crate::NodeId::new("duplicate"),
        scene: crate::scene_modifier_preset::SceneNodeRef {
            scope: Vec::new(),
            node: "scene".into(),
        },
        targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        graph: Box::new(graph_def_with_id("local", "Local")),
    };
    host.scene_modifiers.push(modifier.clone());
    host.scene_modifiers.push(modifier);
    let mut layer = crate::layer::Layer::new_generator(
        "Generator".into(),
        PresetTypeId::new("PLASMA"),
        0,
    );
    layer.layer_id = crate::LayerId::new("layer");
    layer.gen_params_mut().unwrap().graph = Some(host);
    project.timeline.layers.push(layer);
    let target = crate::GraphTarget::SceneModifier {
        owner: Box::new(crate::GraphTarget::Generator(crate::LayerId::new("layer"))),
        modifier_id: crate::NodeId::new("duplicate"),
    };
    let before = project.graph_target_owner(&target).unwrap().graph.clone();
    let before_versions = {
        let owner = project.graph_target_owner(&target).unwrap();
        (owner.graph_version, owner.graph_structure_version)
    };
    assert!(project
        .with_graph_for_target_mut(&target, None, true, |_| {})
        .is_none());
    let owner = project.graph_target_owner(&target).unwrap();
    assert_eq!(owner.graph, before);
    assert_eq!(
        (owner.graph_version, owner.graph_structure_version),
        before_versions
    );
}
