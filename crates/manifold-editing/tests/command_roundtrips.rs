use manifold_editing::command::Command;
use manifold_editing::commands::clip::*;
use manifold_editing::commands::layer::*;
use manifold_editing::commands::settings::*;
use manifold_editing::commands::effects::*;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effect_groups::*;
use manifold_editing::commands::drivers::*;
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::envelopes::*;
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::layer::Layer;
use manifold_core::LayerId;
use manifold_core::types::*;
use manifold_core::effects::*;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

fn load_project(name: &str) -> Project {
    let path = fixture_path(name);
    manifold_io::loader::load_project(&path)
        .unwrap_or_else(|e| panic!("Failed to load {name}: {e}"))
}

fn make_test_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = 120.0;
    project.settings.time_signature_numerator = 4;

    // Add 2 layers
    project.timeline.insert_layer(0, Layer::new("Layer 1".into(), LayerType::Video, 0));
    project.timeline.insert_layer(1, Layer::new("Layer 2".into(), LayerType::Generator, 1));

    // Add clips to layer 0
    let clip1 = TimelineClip {
        start_beat: 0.0,
        duration_beats: 4.0,
        layer_index: 0,
        ..Default::default()
    };
    let clip2 = TimelineClip {
        start_beat: 4.0,
        duration_beats: 4.0,
        layer_index: 0,
        ..Default::default()
    };
    project.timeline.layers[0].add_clip(clip1);
    project.timeline.layers[0].add_clip(clip2);

    project.timeline.rebuild_clip_lookup();
    project
}

// ─── Clip Commands ───

#[test]
fn swap_video_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = SwapVideoCommand::new(
        clip_id.clone(),
        "old_video".into(), "new_video".into(),
        0.0, 1.5,
        4.0, 8.0,
    );

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "new_video");
    assert!((clip.in_point - 1.5).abs() < 0.001);
    assert!((clip.duration_beats - 8.0).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "old_video");
    assert!((clip.in_point - 0.0).abs() < 0.001);
    assert!((clip.duration_beats - 4.0).abs() < 0.001);
}

#[test]
fn slip_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = SlipClipCommand::new(clip_id.clone(), 0.0, 2.5);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - 2.5).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - 0.0).abs() < 0.001);
}

#[test]
fn clip_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let old = ClipEffectsSnapshot {
        invert_colors: false, is_looping: false, loop_duration_beats: 0.0,
        translate_x: 0.0, translate_y: 0.0, scale: 1.0, rotation: 0.0,
    };
    let new = ClipEffectsSnapshot {
        invert_colors: true, is_looping: true, loop_duration_beats: 2.0,
        translate_x: 0.5, translate_y: -0.3, scale: 2.0, rotation: 45.0,
    };

    let mut cmd = ClipEffectsCommand::new(clip_id.clone(), old, new);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.invert_colors);
    assert!(clip.is_looping);
    assert!((clip.scale - 2.0).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.invert_colors);
    assert!(!clip.is_looping);
    assert!((clip.scale - 1.0).abs() < 0.001);
}

#[test]
fn change_clip_loop_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = ChangeClipLoopCommand::new(clip_id.clone(), false, true, 0.0, 2.0);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.is_looping);
    assert!((clip.loop_duration_beats - 2.0).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.is_looping);
}

#[test]
fn change_clip_recorded_bpm_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = ChangeClipRecordedBpmCommand::new(clip_id.clone(), 0.0, 130.0);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.recorded_bpm - 130.0).abs() < 0.01);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.recorded_bpm - 0.0).abs() < 0.01);
}

#[test]
fn split_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let initial_count = project.timeline.layers[0].clips.len();

    let mut tail = project.timeline.layers[0].clips[0].clone_with_new_id();
    tail.start_beat = 2.0;
    tail.duration_beats = 2.0;

    let mut cmd = SplitClipCommand::new(clip_id.clone(), 0, 4.0, 2.0, tail);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count + 1);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - 2.0).abs() < 0.001);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - 4.0).abs() < 0.001);
}

// ─── Layer Commands ───

#[test]
fn add_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();

    let mut cmd = AddLayerCommand::new(
        "New Layer".into(), LayerType::Video, GeneratorType::None, 0, None,
    );

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count + 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
}

#[test]
fn delete_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();
    let layer = project.timeline.layers[0].clone();

    let mut cmd = DeleteLayerCommand::new(layer, 0);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count - 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
}

#[test]
fn reorder_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let old_order = project.timeline.layers.clone();
    let old_names: Vec<String> = old_order.iter().map(|l| l.name.clone()).collect();

    let mut new_order = old_order.clone();
    new_order.reverse();

    let empty_map = std::collections::HashMap::new();
    let mut cmd = ReorderLayerCommand::new(old_order, new_order, empty_map.clone(), empty_map);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].name, old_names[1]);
    assert_eq!(project.timeline.layers[1].name, old_names[0]);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].name, old_names[0]);
    assert_eq!(project.timeline.layers[1].name, old_names[1]);
}

#[test]
fn group_layers_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();
    let layer_ids: Vec<LayerId> = project.timeline.layers.iter().map(|l| l.layer_id.clone()).collect();
    let original_order = project.timeline.layers.clone();

    let mut cmd = GroupLayersCommand::new(layer_ids, original_order);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count + 1); // group added
    assert!(project.timeline.layers.iter().any(|l| l.is_group()));

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
    assert!(!project.timeline.layers.iter().any(|l| l.is_group()));
}

// ─── Settings Commands ───

#[test]
fn change_quantize_mode_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeQuantizeModeCommand::new(QuantizeMode::Off, QuantizeMode::Beat);

    cmd.execute(&mut project);
    assert_eq!(project.settings.quantize_mode, QuantizeMode::Beat);

    cmd.undo(&mut project);
    assert_eq!(project.settings.quantize_mode, QuantizeMode::Off);
}

#[test]
fn change_frame_rate_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeFrameRateCommand::new(60.0, 30.0);

    cmd.execute(&mut project);
    assert!((project.settings.frame_rate - 30.0).abs() < 0.01);

    cmd.undo(&mut project);
    assert!((project.settings.frame_rate - 60.0).abs() < 0.01);
}

#[test]
fn change_layer_midi_note_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeLayerMidiNoteCommand::new(0, -1, 60);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, 60);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, -1);
}

#[test]
fn change_layer_blend_mode_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeLayerBlendModeCommand::new(0, BlendMode::Normal, BlendMode::Additive);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].default_blend_mode, BlendMode::Additive);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].default_blend_mode, BlendMode::Normal);
}

#[test]
fn change_layer_opacity_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeLayerOpacityCommand::new(0, 1.0, 0.5);

    cmd.execute(&mut project);
    assert!((project.timeline.layers[0].opacity - 0.5).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.timeline.layers[0].opacity - 1.0).abs() < 0.001);
}

#[test]
fn change_generator_type_undo_roundtrip() {
    let mut project = make_test_project();
    // Layer 1 is a generator layer
    let gp = project.timeline.layers[1].gen_params.get_or_insert_with(Default::default);
    gp.generator_type = GeneratorType::Plasma;
    gp.param_values = vec![0.5, 0.8, 1.0];
    gp.base_param_values = Some(vec![0.5, 0.8, 1.0]);

    let old_params = project.timeline.layers[1].snapshot_gen_params();
    let old_drivers = project.timeline.layers[1].snapshot_gen_drivers();
    let old_envelopes = project.timeline.layers[1].snapshot_gen_envelopes();

    let mut cmd = ChangeGeneratorTypeCommand::new(
        1, GeneratorType::Plasma, GeneratorType::Tesseract,
        old_params, old_drivers, old_envelopes,
    );

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[1].generator_type(), GeneratorType::Tesseract);
    // After type change, params are filled with Tesseract's definition defaults (11 params)
    assert_eq!(project.timeline.layers[1].snapshot_gen_params().len(),
               manifold_core::generator_definition_registry::get(GeneratorType::Tesseract).param_count);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[1].generator_type(), GeneratorType::Plasma);
    assert_eq!(project.timeline.layers[1].snapshot_gen_params().len(), 3);
}

#[test]
fn change_master_opacity_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeMasterOpacityCommand::new(1.0, 0.7);

    cmd.execute(&mut project);
    assert!((project.settings.master_opacity - 0.7).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.settings.master_opacity - 1.0).abs() < 0.001);
}

#[test]
fn clear_tempo_map_undo_roundtrip() {
    let mut project = make_test_project();
    project.tempo_map.add_or_replace_point(0.0, 120.0, TempoPointSource::Manual, 0.001);
    project.tempo_map.add_or_replace_point(4.0, 140.0, TempoPointSource::Manual, 0.001);

    let old_points = project.tempo_map.clone_points();
    assert_eq!(old_points.len(), 2);

    let mut cmd = ClearTempoMapCommand::new(old_points, 120.0);

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1); // just the beat-zero point

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
}

#[test]
fn restore_tempo_lane_undo_roundtrip() {
    let mut project = make_test_project();
    project.tempo_map.add_or_replace_point(0.0, 120.0, TempoPointSource::Manual, 0.001);
    let old_points = project.tempo_map.clone_points();

    let new_points = vec![
        manifold_core::tempo::TempoPoint { beat: 0.0, bpm: 130.0, source: TempoPointSource::Recorded, recorded_at_seconds: 0.0 },
        manifold_core::tempo::TempoPoint { beat: 4.0, bpm: 140.0, source: TempoPointSource::Recorded, recorded_at_seconds: 2.0 },
    ];

    let mut cmd = RestoreRecordedTempoLaneCommand::new(120.0, old_points, new_points);

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
    assert!((project.settings.bpm - 130.0).abs() < 0.01);

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1);
    assert!((project.settings.bpm - 120.0).abs() < 0.01);
}

// ─── Effect Commands ───

#[test]
fn add_effect_undo_roundtrip() {
    let mut project = make_test_project();
    let target = EffectTarget::Master;

    let effect = EffectInstance {
        effect_type: EffectType::Bloom,
        enabled: true,
        param_values: vec![0.5],
        ..make_effect(EffectType::Bloom)
    };

    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects.len(), 1);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects.len(), 0);
}

#[test]
fn remove_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        enabled: true,
        param_values: vec![0.5],
        ..make_effect(EffectType::Bloom)
    });

    let effect = project.settings.master_effects[0].clone();
    let target = EffectTarget::Master;
    let mut cmd = RemoveEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects.len(), 0);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects.len(), 1);
}

#[test]
fn toggle_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        enabled: true,
        ..make_effect(EffectType::Bloom)
    });

    let target = EffectTarget::Master;
    let mut cmd = ToggleEffectCommand::new(target, 0, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].enabled);
}

#[test]
fn change_effect_param_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        enabled: true,
        param_values: vec![0.5, 0.3],
        base_param_values: Some(vec![0.5, 0.3]),
        ..make_effect(EffectType::Bloom)
    });

    let target = EffectTarget::Master;
    let mut cmd = ChangeEffectParamCommand::new(target, 0, 0, 0.5, 0.9);

    cmd.execute(&mut project);
    assert!((project.settings.master_effects[0].param_values[0] - 0.9).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.settings.master_effects[0].param_values[0] - 0.5).abs() < 0.001);
}

#[test]
fn reorder_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(make_effect(EffectType::Bloom));
    project.settings.master_effects.push(make_effect(EffectType::Feedback));

    let target = EffectTarget::Master;
    // to_index uses pre-removal indexing: to=2 means "insert after the last element"
    // Unity: remove at 0 -> [Feedback], insertAt = 2-1 = 1, insert Bloom at 1 -> [Feedback, Bloom]
    let mut cmd = ReorderEffectCommand::new(target, 0, 2);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects[0].effect_type, EffectType::Feedback);
    assert_eq!(project.settings.master_effects[1].effect_type, EffectType::Bloom);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects[0].effect_type, EffectType::Bloom);
    assert_eq!(project.settings.master_effects[1].effect_type, EffectType::Feedback);
}

#[test]
fn effect_on_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let effect = EffectInstance {
        effect_type: EffectType::Kaleidoscope,
        enabled: true,
        ..make_effect(EffectType::Kaleidoscope)
    };
    let target = EffectTarget::Clip { clip_id: clip_id.clone() };
    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.effects.len(), 1);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.effects.len(), 0);
}

#[test]
fn effect_on_layer_undo_roundtrip() {
    let mut project = make_test_project();

    let effect = EffectInstance {
        effect_type: EffectType::Mirror,
        enabled: true,
        ..make_effect(EffectType::Mirror)
    };
    let target = EffectTarget::Layer { layer_id: project.timeline.layers[0].layer_id.clone() };
    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].effects.as_ref().unwrap().len(), 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].effects.as_ref().unwrap().len(), 0);
}

// ─── Effect Group Commands ───

#[test]
fn group_effects_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(make_effect(EffectType::Bloom));
    project.settings.master_effects.push(make_effect(EffectType::Feedback));

    let target = EffectTarget::Master;
    let mut cmd = GroupEffectsCommand::new(target, vec![0, 1], "My Group".into());

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(project.settings.master_effects[0].group_id, project.settings.master_effects[1].group_id);
    assert_eq!(project.settings.master_effect_groups.as_ref().unwrap().len(), 1);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(project.settings.master_effects[1].group_id.is_none());
    assert!(project.settings.master_effect_groups.as_ref().unwrap().is_empty());
}

#[test]
fn ungroup_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    project.settings.master_effects.push(EffectInstance { effect_type: EffectType::Bloom, group_id: Some(gid.clone()), ..make_effect(EffectType::Bloom) });
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = UngroupEffectsCommand::new(target, gid);

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(project.settings.master_effect_groups.as_ref().unwrap().is_empty());

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(project.settings.master_effect_groups.as_ref().unwrap().len(), 1);
}

#[test]
fn toggle_group_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = ToggleGroupCommand::new(target, gid, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effect_groups.as_ref().unwrap()[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effect_groups.as_ref().unwrap()[0].enabled);
}

#[test]
fn rename_group_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Old Name".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = RenameGroupCommand::new(target, gid, "Old Name".into(), "New Name".into());

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].name, "New Name");

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effect_groups.as_ref().unwrap()[0].name, "Old Name");
}

#[test]
fn change_group_wet_dry_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = ChangeGroupWetDryCommand::new(target, gid, 1.0, 0.5);

    cmd.execute(&mut project);
    assert!((project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 0.5).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 1.0).abs() < 0.001);
}

// ─── Driver Commands ───

#[test]
fn add_driver_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(make_effect(EffectType::Bloom));

    let target = DriverTarget::Effect {
        effect_target: EffectTarget::Master,
        effect_index: 0,
    };
    let driver = ParameterDriver {
        param_index: 0,
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        is_paused_by_user: false,
    };

    let mut cmd = AddDriverCommand::new(target, driver);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects[0].drivers.as_ref().unwrap().len(), 1);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].drivers.as_ref().unwrap().is_empty());
}

#[test]
fn toggle_driver_enabled_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        drivers: Some(vec![ParameterDriver {
            param_index: 0, enabled: true,
            ..make_driver()
        }]),
        ..make_effect(EffectType::Bloom)
    });

    let target = DriverTarget::Effect {
        effect_target: EffectTarget::Master,
        effect_index: 0,
    };
    let mut cmd = ToggleDriverEnabledCommand::new(target, 0, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);
}

#[test]
fn change_driver_waveform_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        drivers: Some(vec![ParameterDriver {
            param_index: 0, waveform: DriverWaveform::Sine,
            ..make_driver()
        }]),
        ..make_effect(EffectType::Bloom)
    });

    let target = DriverTarget::Effect {
        effect_target: EffectTarget::Master,
        effect_index: 0,
    };
    let mut cmd = ChangeDriverWaveformCommand::new(target, 0, DriverWaveform::Sine, DriverWaveform::Square);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform, DriverWaveform::Square);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform, DriverWaveform::Sine);
}

#[test]
fn change_trim_undo_roundtrip() {
    let mut project = make_test_project();
    project.settings.master_effects.push(EffectInstance {
        effect_type: EffectType::Bloom,
        drivers: Some(vec![ParameterDriver {
            param_index: 0, trim_min: 0.0, trim_max: 1.0,
            ..make_driver()
        }]),
        ..make_effect(EffectType::Bloom)
    });

    let target = DriverTarget::Effect {
        effect_target: EffectTarget::Master,
        effect_index: 0,
    };
    let mut cmd = ChangeTrimCommand::new(target, 0, 0.0, 1.0, 0.2, 0.8);

    cmd.execute(&mut project);
    let d = &project.settings.master_effects[0].drivers.as_ref().unwrap()[0];
    assert!((d.trim_min - 0.2).abs() < 0.001);
    assert!((d.trim_max - 0.8).abs() < 0.001);

    cmd.undo(&mut project);
    let d = &project.settings.master_effects[0].drivers.as_ref().unwrap()[0];
    assert!((d.trim_min - 0.0).abs() < 0.001);
    assert!((d.trim_max - 1.0).abs() < 0.001);
}

// ─── Envelope Commands ───

#[test]
fn add_param_envelope_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let envelope = ParamEnvelope {
        target_effect_type: EffectType::Bloom,
        param_index: 0,
        enabled: true,
        attack_beats: 0.25,
        decay_beats: 0.25,
        sustain_level: 0.8,
        release_beats: 0.5,
        target_normalized: 1.0,
        current_level: 0.0,
    };

    let mut cmd = AddParamEnvelopeCommand::new(clip_id.clone(), envelope);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.has_envelopes());

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.has_envelopes());
}

#[test]
fn change_envelope_adsr_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    // Add an envelope first
    let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
    clip.envelopes_mut().push(ParamEnvelope {
        attack_beats: 0.25, decay_beats: 0.25, sustain_level: 1.0, release_beats: 0.25,
        ..make_envelope()
    });

    let mut cmd = ChangeEnvelopeADSRCommand::new(
        clip_id.clone(), 0,
        0.25, 0.25, 1.0, 0.25,
        0.5, 0.5, 0.7, 1.0,
    );

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    let env = &clip.envelopes.as_ref().unwrap()[0];
    assert!((env.attack_beats - 0.5).abs() < 0.001);
    assert!((env.sustain_level - 0.7).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    let env = &clip.envelopes.as_ref().unwrap()[0];
    assert!((env.attack_beats - 0.25).abs() < 0.001);
    assert!((env.sustain_level - 1.0).abs() < 0.001);
}

#[test]
fn add_layer_envelope_undo_roundtrip() {
    let mut project = make_test_project();

    let envelope = ParamEnvelope {
        target_effect_type: EffectType::Transform,
        param_index: 0,
        enabled: true,
        ..make_envelope()
    };

    let mut cmd = AddLayerEnvelopeCommand::new(0, envelope);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].envelopes.as_ref().unwrap().len(), 1);

    cmd.undo(&mut project);
    assert!(project.timeline.layers[0].envelopes.as_ref().unwrap().is_empty());
}

// ─── Test with real project fixtures ───

#[test]
fn commands_work_on_loaded_project() {
    let mut project = load_project("Burn V5.manifold");
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_beat = project.timeline.layers[0].clips[0].start_beat;

    // Chain several commands
    let mut cmd1 = SlipClipCommand::new(clip_id.clone(), 0.0, 1.0);
    cmd1.execute(&mut project);
    assert!((project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - 1.0).abs() < 0.001);

    let mut cmd2 = ChangeClipLoopCommand::new(clip_id.clone(), false, true, 0.0, 2.0);
    cmd2.execute(&mut project);
    assert!(project.timeline.find_clip_by_id(&clip_id).unwrap().is_looping);

    // Undo both
    cmd2.undo(&mut project);
    assert!(!project.timeline.find_clip_by_id(&clip_id).unwrap().is_looping);

    cmd1.undo(&mut project);
    assert!((project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - 0.0).abs() < 0.001);
    assert!((project.timeline.find_clip_by_id(&clip_id).unwrap().start_beat - original_beat).abs() < 0.001);
}

fn make_effect(effect_type: EffectType) -> EffectInstance {
    EffectInstance {
        effect_type,
        enabled: true,
        collapsed: false,
        param_values: Vec::new(),
        base_param_values: None,
        drivers: None,
        group_id: None,
        legacy_param0: None,
        legacy_param1: None,
        legacy_param2: None,
        legacy_param3: None,
    }
}

fn make_driver() -> ParameterDriver {
    ParameterDriver {
        param_index: 0,
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        is_paused_by_user: false,
    }
}

fn make_envelope() -> ParamEnvelope {
    ParamEnvelope {
        target_effect_type: EffectType::Transform,
        param_index: 0,
        enabled: true,
        attack_beats: 0.25,
        decay_beats: 0.25,
        sustain_level: 1.0,
        release_beats: 0.25,
        target_normalized: 1.0,
        current_level: 0.0,
    }
}
