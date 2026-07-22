use manifold_core::LayerId;
use manifold_core::audio_clip_detection::AudioClipDetection;
use manifold_core::clip::TimelineClip;
use manifold_core::effects::*;
use manifold_core::layer::Layer;
use manifold_core::percussion_analysis::{PercussionAnalysisData, PercussionTriggerType};
use manifold_core::project::Project;
use manifold_core::types::*;
use manifold_core::units::Bpm;
use manifold_core::{Beats, PresetTypeId, Seconds};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::*;
use manifold_editing::commands::drivers::*;
use manifold_editing::commands::effect_groups::*;
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::*;
use manifold_editing::commands::envelopes::*;
use manifold_editing::commands::layer::*;
use manifold_editing::commands::session_commands::*;
use manifold_editing::commands::settings::*;
use manifold_editing::service::EditingService;
use manifold_core::marker::TimelineMarker;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::SceneId;

// Test-only inventory submissions — manifold-renderer isn't linked in editing tests.
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};

// Step 16: ChangeEffectParamCommand resolves param_id → index via the
// effect registry on each execute/undo. Tests must register at least
// the effects they reference (Bloom is the only one used).
inventory::submit! {
    EffectMetadata {
        id: PresetTypeId::BLOOM,
        display_name: "Bloom",
        category: "Post-Process",
        available: true,
        osc_prefix: "bloom",
        legacy_discriminant: Some(12),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.187, "F2", ""),
            ParamSpec::continuous("threshold", "Threshold", 0.0, 5.0, 1.0, "F2", ""),
        ],
    }
}

inventory::submit! {
    GeneratorMetadata {
        id: PresetTypeId::PLASMA,
        display_name: "Plasma",
        is_line_based: false,
        available: true,
        osc_prefix: "plasma",
        legacy_discriminant: Some(6),
        params: &[
            ParamSpec::whole_labels("pattern", "Pattern", 0.0, 7.0, 0.0, &["Classic", "Rings", "Diamond", "Warp", "Cells", "Noise", "Fractal", "Lattice"], "pattern"),
            ParamSpec::continuous("complexity", "Complexity", 0.0, 1.0, 0.5, "F2", "complexity"),
            ParamSpec::continuous("contrast", "Contrast", 0.0, 1.0, 0.63, "F2", "contrast"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 1.0, "clipTrigger"),
        ],    }
}
inventory::submit! {
    GeneratorMetadata {
        id: PresetTypeId::TESSERACT,
        display_name: "Tesseract",
        is_line_based: true,
        available: true,
        osc_prefix: "tesseract",
        legacy_discriminant: Some(4),
        params: &[
            ParamSpec::continuous("xy", "XY", 0.0, 2.0, 0.6, "F2", "rotXY"),
            ParamSpec::continuous("zw", "ZW", 0.0, 2.0, 0.4, "F2", "rotZW"),
            ParamSpec::continuous("xw", "XW", 0.0, 2.0, 0.25, "F2", "rotXW"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::continuous("dist", "Dist", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],    }
}

fn slot(id: &str, value: f32, exposed: bool) -> manifold_core::params::Param {
    let mut p = manifold_core::params::Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
        id: id.into(),
        name: id.into(),
        min: 0.0,
        max: 1.0,
        default_value: value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: vec![],
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
        card_visible: true,
    });
    p.value = value;
    p.base = value;
    p.exposed = exposed;
    p
}

fn fixture_path(name: &str) -> std::path::PathBuf {
    // The `.manifold` fixtures are gitignored (large personal projects), so a
    // `git worktree` checkout doesn't contain them. Resolve to the MAIN working
    // tree: `--git-common-dir` points at the primary repo's `.git`, whose parent
    // is the main checkout where the fixtures live — so these tests RUN from a
    // worktree instead of panicking with a confusing file-not-found. Falls back
    // to the crate-relative path (the main checkout, or if git isn't reachable).
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        && out.status.success()
        && let Ok(common) =
            std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(name);
        if candidate.exists() {
            return candidate;
        }
    }
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
    project.settings.bpm = manifold_core::units::Bpm(120.0);
    project.settings.time_signature_numerator = 4;

    // Add 2 layers
    project
        .timeline
        .insert_layer(0, Layer::new("Layer 1".into(), LayerType::Video, 0));
    project
        .timeline
        .insert_layer(1, Layer::new("Layer 2".into(), LayerType::Generator, 1));

    // Add clips to layer 0
    let clip1 = TimelineClip {
        start_beat: Beats(0.0),
        duration_beats: Beats(4.0),
        ..Default::default()
    };
    let clip2 = TimelineClip {
        start_beat: Beats(4.0),
        duration_beats: Beats(4.0),
        ..Default::default()
    };
    project.timeline.layers[0].restore_clip(clip1);
    project.timeline.layers[0].restore_clip(clip2);

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
        "old_video".into(),
        "new_video".into(),
        Seconds(0.0),
        Seconds(1.5),
        Beats(4.0),
        Beats(8.0),
    );

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "new_video");
    assert!((clip.in_point - Seconds(1.5)).abs() < Seconds(0.001));
    assert!((clip.duration_beats - Beats(8.0)).abs() < Beats(0.001));

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "old_video");
    assert!((clip.in_point - Seconds(0.0)).abs() < Seconds(0.001));
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

/// D6: replace swaps path/duration, resets in_point + recorded_bpm, keeps the
/// detection config, clears the cached analysis + counts — and the composite
/// (mirroring the inspector gesture) deletes every clip this audio clip
/// generated (`detection_source == clip`) in the same undoable step. One undo
/// restores the old file, in_point, BPM, cached analysis, AND the deleted
/// generated clips.
#[test]
fn replace_audio_file_undo_restores_source_state_and_generated_clips() {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);

    // Song lane: an audio clip with a populated detection config + cached
    // analysis + counts, as a real Detect run would leave it.
    project
        .timeline
        .insert_layer(0, Layer::new("Song".into(), LayerType::Audio, 0));
    let mut song = TimelineClip::new_audio(
        "old_song.wav".into(),
        Beats(0.0),
        Beats(16.0),
        Seconds(1.5),
        Seconds(30.0),
    );
    song.recorded_bpm = 120.0;
    let mut detection = AudioClipDetection::new();
    detection.config.quantize_on = false;
    detection.analysis = Some(PercussionAnalysisData::new(
        "old_song",
        Bpm(120.0),
        Vec::new(),
        0.9,
        None,
        None,
    ));
    detection.last_counts.insert(PercussionTriggerType::Kick, 8);
    song.audio_detection = Some(detection);
    let song_id = song.id.clone();
    project.timeline.layers[0].restore_clip(song);

    // A trigger lane with two clips this audio clip produced.
    project
        .timeline
        .insert_layer(1, Layer::new("Kick".into(), LayerType::Generator, 1));
    let mut trig1 = TimelineClip {
        start_beat: Beats(0.0),
        duration_beats: Beats(0.25),
        ..Default::default()
    };
    trig1.detection_source = Some(song_id.clone());
    let mut trig2 = TimelineClip {
        start_beat: Beats(4.0),
        duration_beats: Beats(0.25),
        ..Default::default()
    };
    trig2.detection_source = Some(song_id.clone());
    project.timeline.layers[1].restore_clip(trig1);
    project.timeline.layers[1].restore_clip(trig2);
    project.timeline.rebuild_clip_lookup();
    assert_eq!(project.timeline.layers[1].clips.len(), 2);

    // Snapshot old state and build the command exactly as the inspector
    // gesture does (ReplaceAudioFileCommand + one DeleteClipCommand per
    // generated clip, same tag walk as the orchestrator's clear_clip_triggers).
    let old_clip = project.timeline.find_clip_by_id(&song_id).unwrap().clone();
    let replace = ReplaceAudioFileCommand::new(
        song_id.clone(),
        old_clip.audio_file_path.clone(),
        "new_song.wav".into(),
        old_clip.source_duration,
        Seconds(42.0),
        old_clip.in_point,
        old_clip.recorded_bpm,
        old_clip.audio_detection.clone(),
    );
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(replace)];
    for layer in project.timeline.layers.iter() {
        let layer_id = layer.layer_id.clone();
        for c in layer
            .clips
            .iter()
            .filter(|c| c.detection_source.as_ref() == Some(&song_id))
        {
            commands.push(Box::new(DeleteClipCommand::new(c.clone(), layer_id.clone())));
        }
    }
    assert_eq!(commands.len(), 3, "replace + 2 generated-clip deletes");

    let mut cmd = CompositeCommand::new(commands, "Replace Audio File".to_string());
    cmd.execute(&mut project);

    let clip = project.timeline.find_clip_by_id(&song_id).unwrap();
    assert_eq!(clip.audio_file_path, "new_song.wav");
    assert_eq!(clip.source_duration, Seconds(42.0));
    assert_eq!(clip.in_point, Seconds::ZERO);
    assert_eq!(clip.recorded_bpm, 0.0);
    // start_beat / duration_beats untouched by the replace.
    assert_eq!(clip.start_beat, Beats(0.0));
    assert_eq!(clip.duration_beats, Beats(16.0));
    let det = clip.audio_detection.as_ref().expect("config survives replace");
    assert!(!det.config.quantize_on, "config kept as-is");
    assert!(det.analysis.is_none(), "stale analysis cleared");
    assert!(det.last_counts.is_empty(), "stale counts cleared");
    assert!(
        project.timeline.layers[1].clips.is_empty(),
        "generated clips deleted"
    );

    cmd.undo(&mut project);

    let clip = project.timeline.find_clip_by_id(&song_id).unwrap();
    assert_eq!(clip.audio_file_path, "old_song.wav");
    assert_eq!(clip.source_duration, Seconds(30.0));
    assert_eq!(clip.in_point, Seconds(1.5));
    assert_eq!(clip.recorded_bpm, 120.0);
    let det = clip.audio_detection.as_ref().expect("detection restored");
    assert!(!det.config.quantize_on);
    assert!(det.analysis.is_some(), "cached analysis restored");
    assert_eq!(det.last_counts.get(&PercussionTriggerType::Kick), Some(&8));
    assert_eq!(
        project.timeline.layers[1].clips.len(),
        2,
        "deleted generated clips restored"
    );
}

#[test]
fn slip_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = SlipClipCommand::new(clip_id.clone(), Seconds(0.0), Seconds(2.5));

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - Seconds(2.5)).abs() < Seconds(0.001));

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - Seconds(0.0)).abs() < Seconds(0.001));
}

#[test]
fn clip_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let old = ClipEffectsSnapshot {
        is_looping: false,
        loop_duration_beats: Beats(0.0),
        translate_x: 0.0,
        translate_y: 0.0,
        scale: 1.0,
        rotation: 0.0,
    };
    let new = ClipEffectsSnapshot {
        is_looping: true,
        loop_duration_beats: Beats(2.0),
        translate_x: 0.5,
        translate_y: -0.3,
        scale: 2.0,
        rotation: 45.0,
    };

    let mut cmd = ClipEffectsCommand::new(clip_id.clone(), old, new);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.is_looping);
    assert!((clip.scale - 2.0).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.is_looping);
    assert!((clip.scale - 1.0).abs() < 0.001);
}

#[test]
fn change_clip_loop_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = ChangeClipLoopCommand::new(clip_id.clone(), false, true, Beats(0.0), Beats(2.0));

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.is_looping);
    assert!((clip.loop_duration_beats - Beats(2.0)).abs() < Beats(0.001));

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
fn change_clip_recorded_bpm_rescales_audio_clip_length() {
    // Audio clip: 4 beats at the project's 120 BPM, warp off. Declaring it a
    // 60-BPM clip holds the played audio (2 s) constant, so it now occupies
    // 2 beats. Undo restores the original length. A non-audio clip is unaffected
    // (covered by the roundtrip test above).
    let mut project = make_test_project();
    let audio = TimelineClip::new_audio(
        "/x.wav".into(),
        Beats(0.0),
        Beats(4.0),
        manifold_core::units::Seconds(0.0),
        manifold_core::units::Seconds(2.0),
    );
    let clip_id = audio.id.clone();
    project.timeline.layers[1].restore_clip(audio);
    project.timeline.rebuild_clip_lookup();

    let mut cmd = ChangeClipRecordedBpmCommand::new(clip_id.clone(), 0.0, 60.0);
    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats.0 - 2.0).abs() < 1e-6, "60 BPM → 2 beats");

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats.0 - 4.0).abs() < 1e-6, "undo restores 4 beats");
}

#[test]
fn split_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let initial_count = project.timeline.layers[0].clips.len();

    let mut tail = project.timeline.layers[0].clips[0].clone_with_new_id();
    tail.start_beat = Beats(2.0);
    tail.duration_beats = Beats(2.0);

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = SplitClipCommand::new(clip_id.clone(), layer_id, Beats(4.0), Beats(2.0), tail);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count + 1);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - Beats(2.0)).abs() < Beats(0.001));

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

// ─── Layer Commands ───

#[test]
fn add_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();

    let mut cmd = AddLayerCommand::new(
        "New Layer".into(),
        LayerType::Video,
        PresetTypeId::NONE,
        0,
        None,
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

    let mut cmd = DeleteLayerCommand::new(layer);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count - 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
}

#[test]
fn delete_group_clears_children_parent_ids() {
    let mut project = make_test_project();
    // Create a group with children
    let group = Layer::new("Group".into(), LayerType::Group, 0);
    let group_id = group.layer_id.clone();
    project.timeline.insert_layer(0, group);

    // Set children's parent
    for i in 1..project.timeline.layers.len() {
        project.timeline.layers[i].parent_layer_id = Some(group_id.clone());
    }
    let child_count = project.timeline.layers.len() - 1;

    let group_layer = project.timeline.layers[0].clone();
    let mut cmd = DeleteLayerCommand::new(group_layer);

    cmd.execute(&mut project);
    // Children should have parent cleared
    for layer in &project.timeline.layers {
        assert!(
            layer.parent_layer_id.is_none(),
            "child {} still has parent",
            layer.name
        );
    }

    cmd.undo(&mut project);
    // Group restored, children re-parented
    assert!(project.timeline.layers[0].is_group());
    let reparented = project
        .timeline
        .layers
        .iter()
        .filter(|l| l.parent_layer_id.as_ref() == Some(&group_id))
        .count();
    assert_eq!(reparented, child_count);
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
    let layer_ids: Vec<LayerId> = project
        .timeline
        .layers
        .iter()
        .map(|l| l.layer_id.clone())
        .collect();
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

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = ChangeLayerMidiNoteCommand::new(layer_id, -1, 60);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, 60);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, -1);
}

#[test]
fn change_layer_blend_mode_undo_roundtrip() {
    let mut project = make_test_project();

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd =
        ChangeLayerBlendModeCommand::new(layer_id, BlendMode::Normal, BlendMode::Additive);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[0].default_blend_mode,
        BlendMode::Additive
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.timeline.layers[0].default_blend_mode,
        BlendMode::Normal
    );
}

#[test]
fn change_layer_opacity_undo_roundtrip() {
    let mut project = make_test_project();

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = ChangeLayerOpacityCommand::new(layer_id, 1.0, 0.5);

    cmd.execute(&mut project);
    assert!((project.timeline.layers[0].opacity - 0.5).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.timeline.layers[0].opacity - 1.0).abs() < 0.001);
}

#[test]
fn change_generator_type_undo_roundtrip() {
    let mut project = make_test_project();
    // Layer 1 is a generator layer
    let gp = project.timeline.layers[1].gen_params_or_init();
    gp.restore(PresetTypeId::PLASMA, vec![0.5, 0.8, 1.0], None, None);

    let old_params = project.timeline.layers[1].snapshot_gen_params();
    let old_drivers = project.timeline.layers[1].snapshot_gen_drivers();
    let old_envelopes = project.timeline.layers[1].snapshot_gen_envelopes();

    let layer_id = project.timeline.layers[1].layer_id.clone();
    let mut cmd = ChangeGeneratorTypeCommand::new(
        layer_id,
        PresetTypeId::PLASMA,
        PresetTypeId::TESSERACT,
        old_params,
        old_drivers,
        old_envelopes,
    );

    cmd.execute(&mut project);
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::TESSERACT
    );
    // After type change, params are filled with Tesseract's definition defaults (11 params)
    assert_eq!(
        project.timeline.layers[1].snapshot_gen_params().len(),
        manifold_core::preset_definition_registry::get(&PresetTypeId::TESSERACT)
            .param_defs
            .len()
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::PLASMA
    );
    assert_eq!(project.timeline.layers[1].snapshot_gen_params().len(), 3);
}

/// Regression for the "Lissajous renders as wireframe / BasicShapes
/// shows a huge white blob after switching from another generator"
/// bug class. The renderer's per-frame override-version sweep was
/// rebuilding the new-type generator from the previous type's stale
/// `Layer::generator_graph` — so a Plasma → Tesseract type swap kept
/// rendering the Plasma graph, and a Wireframe → BasicShapes swap
/// rendered wireframe polyhedra with BasicShapes' four outer-card
/// values jammed into the wireframe bindings (rotate_x_speed=0.015,
/// line=1.0 → a massive white shape covering the screen).
///
/// `Layer::change_generator_type` now clears the per-layer graph
/// override and bumps `generator_graph_version`, and
/// `ChangeGeneratorTypeCommand` snapshots the cleared graph so undo
/// reinstates whatever the user had edited before the type swap.
#[test]
fn change_generator_type_clears_and_restores_graph_override() {
    use manifold_core::effect_graph_def::EffectGraphDef;

    let mut project = make_test_project();
    // Layer 1 is a generator layer in make_test_project.
    let gp = project.timeline.layers[1].gen_params_or_init();
    gp.restore(PresetTypeId::PLASMA, vec![0.5, 0.8, 1.0], None, None);

    // Plant a non-empty per-layer graph override + version, simulating
    // a user who edited the Plasma graph through the editor.
    let stale_graph = EffectGraphDef {
        version: 2,
        name: Some("stale-plasma".into()),
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    project.timeline.layers[1].gen_params_or_init().graph = Some(stale_graph.clone());
    project.timeline.layers[1].gen_params_or_init().graph_version = 7;

    let old_params = project.timeline.layers[1].snapshot_gen_params();
    let old_drivers = project.timeline.layers[1].snapshot_gen_drivers();
    let old_envelopes = project.timeline.layers[1].snapshot_gen_envelopes();
    let layer_id = project.timeline.layers[1].layer_id.clone();

    let mut cmd = ChangeGeneratorTypeCommand::new(
        layer_id,
        PresetTypeId::PLASMA,
        PresetTypeId::TESSERACT,
        old_params,
        old_drivers,
        old_envelopes,
    );

    cmd.execute(&mut project);

    // After execute: type swapped + graph override cleared + version
    // bumped so the renderer's per-frame sweep notices and rebuilds
    // against the new type's bundled JSON instead of the stale graph.
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::TESSERACT
    );
    assert!(
        project.timeline.layers[1].generator_graph().is_none(),
        "stale per-layer graph override must be cleared on type change \
         (otherwise the renderer keeps drawing the previous generator)",
    );
    assert_ne!(
        project.timeline.layers[1].generator_graph_version(), 7,
        "generator_graph_version must bump on clear so the renderer's \
         per-frame override-version sweep rebuilds the generator",
    );

    cmd.undo(&mut project);

    // After undo: original graph + version-bump restored. The exact
    // version value isn't load-bearing (the renderer compares against
    // its own cached snapshot), but it must be different from the
    // post-execute value so the sweep detects the change.
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::PLASMA
    );
    assert_eq!(
        project.timeline.layers[1].generator_graph().and_then(|g| g.name.as_deref()),
        Some("stale-plasma"),
        "undo must restore the pre-change graph override verbatim",
    );
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
    project
        .tempo_map
        .add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
    project
        .tempo_map
        .add_or_replace_point(Beats(4.0), Bpm(140.0), TempoPointSource::Manual, 0.001);

    let old_points = project.tempo_map.clone_points();
    assert_eq!(old_points.len(), 2);

    let mut cmd = ClearTempoMapCommand::new(old_points, Bpm(120.0));

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1); // just the beat-zero point

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
}

#[test]
fn restore_tempo_lane_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .tempo_map
        .add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
    let old_points = project.tempo_map.clone_points();

    let new_points = vec![
        manifold_core::tempo::TempoPoint {
            beat: Beats(0.0),
            bpm: Bpm(130.0),
            source: TempoPointSource::Recorded,
            recorded_at_seconds: Seconds(0.0),
        },
        manifold_core::tempo::TempoPoint {
            beat: Beats(4.0),
            bpm: Bpm(140.0),
            source: TempoPointSource::Recorded,
            recorded_at_seconds: Seconds(2.0),
        },
    ];

    let mut cmd = RestoreRecordedTempoLaneCommand::new(Bpm(120.0), old_points, new_points);

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
    assert!((project.settings.bpm.0 - 130.0).abs() < 0.01);

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1);
    assert!((project.settings.bpm.0 - 120.0).abs() < 0.01);
}

// ─── Effect Commands ───

#[test]
fn add_effect_undo_roundtrip() {
    let mut project = make_test_project();
    let target = EffectTarget::Master;

    let mut effect = PresetInstance::new(PresetTypeId::BLOOM);
    effect.params = manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);

    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects.len(), 1);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects.len(), 0);
}

#[test]
fn remove_effect_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = manifold_core::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
        project.settings.master_effects.push(fx);
    }

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
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectCommand::new(effect_id, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].enabled);
}

/// `docs/DEPTH_RELIGHT_DESIGN.md` P5: the "3D Shading" toggle + its D3 knobs,
/// addressed by `GraphTarget` — proven here on an effect instance; the same
/// commands address a generator's `gen_params` identically since both are
/// `PresetInstance` (the `GraphTarget::Generator` case is exercised
/// elsewhere in this file for the ordinary graph commands, same resolver).
#[test]
fn toggle_relight_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));
    let effect_id = project.settings.master_effects[0].id.clone();
    let target = manifold_core::GraphTarget::Effect(effect_id);

    let mut cmd = ToggleRelightCommand::new(target, false, true);
    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].relight);

    cmd.undo(&mut project);
    assert!(!project.settings.master_effects[0].relight);
}

#[test]
fn set_relight_param_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));
    let effect_id = project.settings.master_effects[0].id.clone();
    let target = manifold_core::GraphTarget::Effect(effect_id);
    let default_relief = project.settings.master_effects[0].relight_params.relief;

    let mut cmd = SetRelightParamCommand::new(target, RelightField::Relief, default_relief, 0.9);
    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects[0].relight_params.relief, 0.9);

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0].relight_params.relief,
        default_relief
    );
}

#[test]
fn set_relight_height_from_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));
    let effect_id = project.settings.master_effects[0].id.clone();
    let target = manifold_core::GraphTarget::Effect(effect_id);

    let mut cmd = SetRelightHeightFromCommand::new(
        target,
        RelightHeightFrom::Auto,
        RelightHeightFrom::InvertedLuminance,
    );
    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effects[0].relight_params.height_from,
        RelightHeightFrom::InvertedLuminance
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0].relight_params.height_from,
        RelightHeightFrom::Auto
    );
}

#[test]
fn change_effect_param_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = manifold_core::params::ParamManifest::from_params(vec![
            slot("amount", 0.5, true),
            slot("threshold", 0.3, true),
        ]);
        fx.base_tracked = true; // slots already carry base = value (fork #16)
        project.settings.master_effects.push(fx);
    }

    // Step 16: id-keyed addressing (was positional `0`).
    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id.clone()),
        "amount",
        0.5,
        0.9,
    );

    cmd.execute(&mut project);
    assert!(
        (project.settings.master_effects[0].params.get("amount").unwrap().value - 0.9).abs()
            < 0.001
    );

    cmd.undo(&mut project);
    assert!(
        (project.settings.master_effects[0].params.get("amount").unwrap().value - 0.5).abs()
            < 0.001
    );

    // Targets `threshold` (index 1) — confirm id-based addressing
    // routes to the right slot, not just index 0.
    let mut cmd2 = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        "threshold",
        0.3,
        0.7,
    );
    cmd2.execute(&mut project);
    assert!(
        (project.settings.master_effects[0].params.get("threshold").unwrap().value - 0.7).abs()
            < 0.001
    );
    cmd2.undo(&mut project);
    assert!(
        (project.settings.master_effects[0].params.get("threshold").unwrap().value - 0.3).abs()
            < 0.001
    );
}

#[test]
fn change_effect_param_unknown_id_is_no_op() {
    // An undo entry that targets a param id which has been dropped
    // from the schema since the entry was recorded must NOT panic
    // and must NOT scribble random indices. It silently no-ops.
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 0.3, true),
    ]);
    fx.base_tracked = true; // slots already carry base = value (fork #16)
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        "phantom_param",
        0.5,
        0.9,
    );

    let expected: Vec<manifold_core::params::Param> =
        vec![slot("amount", 0.5, true), slot("threshold", 0.3, true)];

    cmd.execute(&mut project);
    // Unchanged — no slot was matched.
    assert_eq!(
        project.settings.master_effects[0]
            .params
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0]
            .params
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn change_effect_param_undo_roundtrip_on_user_tail_binding() {
    // Regression: a user-exposed inner-node param is addressed by an id like
    // `user.<handle>.<param>.<n>`, appended to the instance's id-keyed
    // manifest alongside the static (bundled) entries. The command must
    // resolve that id directly through the manifest — the earlier
    // registry-only lookup returned `None` for user-added ids, making
    // undo/redo a silent no-op for any slider exposed from an inner node.
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 0.3, true),
    ]);
    fx.base_tracked = true; // slots already carry base = value (fork #16)
    fx.append_user_binding(UserParamBinding {
        id: "user.uv.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    project.settings.master_effects.push(fx);

    let user_id = "user.uv.translate.1";
    assert!(
        project.settings.master_effects[0].params.get(user_id).is_some(),
        "user binding resolves to a manifest entry"
    );

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        user_id,
        0.0,
        0.42,
    );
    cmd.execute(&mut project);
    let v = project.settings.master_effects[0].params.get(user_id).unwrap().value;
    assert!((v - 0.42).abs() < 0.001, "execute writes the user-tail slot");

    cmd.undo(&mut project);
    let v = project.settings.master_effects[0].params.get(user_id).unwrap().value;
    assert!((v - 0.0).abs() < 0.001, "undo restores the user-tail slot");
}

#[test]
fn reorder_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::GLITCH));

    let target = EffectTarget::Master;
    // to_index uses pre-removal indexing: to=2 means "insert after the last element"
    // Unity: remove at 0 -> [Feedback], insertAt = 2-1 = 1, insert Bloom at 1 -> [Feedback, Bloom]
    let mut cmd = ReorderEffectCommand::new(target, 0, 2);

    cmd.execute(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::GLITCH
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::BLOOM
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::GLITCH
    );
}

#[test]
fn effect_on_layer_undo_roundtrip() {
    let mut project = make_test_project();

    let effect = PresetInstance::new(PresetTypeId::MIRROR);
    let target = EffectTarget::Layer {
        layer_id: project.timeline.layers[0].layer_id.clone(),
    };
    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[0].effects.as_ref().unwrap().len(),
        1
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.timeline.layers[0].effects.as_ref().unwrap().len(),
        0
    );
}

// ─── Effect Group Commands ───

#[test]
fn group_effects_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::GLITCH));

    let target = EffectTarget::Master;
    let mut cmd = GroupEffectsCommand::new(target, vec![0, 1], "My Group".into());

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(
        project.settings.master_effects[0].group_id,
        project.settings.master_effects[1].group_id
    );
    assert_eq!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(project.settings.master_effects[1].group_id.is_none());
    assert!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ungroup_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.group_id = Some(gid.clone());
        project.settings.master_effects.push(fx);
    }
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = UngroupEffectsCommand::new(target, gid);

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .is_empty()
    );

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .len(),
        1
    );
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
    assert_eq!(
        project.settings.master_effect_groups.as_ref().unwrap()[0].name,
        "New Name"
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effect_groups.as_ref().unwrap()[0].name,
        "Old Name"
    );
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
    assert!(
        (project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 0.5).abs() < 0.001
    );

    cmd.undo(&mut project);
    assert!(
        (project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 1.0).abs() < 0.001
    );
}

// ─── Driver Commands ───

#[test]
fn add_driver_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let driver = ParameterDriver {
        param_id: std::borrow::Cow::Borrowed("amount"),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        free_period_beats: None,
        legacy_param_index: None,
        is_paused_by_user: false,
    };

    let mut cmd = AddDriverCommand::new(target, driver);

    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effects[0]
            .drivers
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    cmd.undo(&mut project);
    assert!(
        project.settings.master_effects[0]
            .drivers
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn toggle_driver_enabled_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            enabled: true,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let mut cmd = ToggleDriverEnabledCommand::new(target, 0, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);
}

#[test]
fn change_driver_waveform_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            waveform: DriverWaveform::Sine,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let mut cmd =
        ChangeDriverWaveformCommand::new(target, 0, DriverWaveform::Sine, DriverWaveform::Square);

    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform,
        DriverWaveform::Square
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform,
        DriverWaveform::Sine
    );
}

#[test]
fn change_trim_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            trim_min: 0.0,
            trim_max: 1.0,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
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
fn add_envelope_undo_roundtrip() {
    // Envelope-home unification: envelopes live on the instance and are
    // addressed by GraphTarget. Layer 1 is a generator, so target it.
    let mut project = make_test_project();

    let envelope = ParamEnvelope {
        param_id: std::borrow::Cow::Borrowed("x"),
        enabled: true,
        ..make_envelope()
    };

    let layer_id = project.timeline.layers[1].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = AddEnvelopeCommand::new(target, envelope);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[1]
            .gen_params()
            .unwrap()
            .envelopes
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    cmd.undo(&mut project);
    assert!(
        project.timeline.layers[1]
            .gen_params()
            .unwrap()
            .envelopes
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

// ─── Test with real project fixtures ───

#[test]
fn commands_work_on_loaded_project() {
    let mut project = load_project("Burn V5.manifold");
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_beat = project.timeline.layers[0].clips[0].start_beat;

    // Chain several commands
    let mut cmd1 = SlipClipCommand::new(clip_id.clone(), Seconds(0.0), Seconds(1.0));
    cmd1.execute(&mut project);
    assert!(
        (project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - Seconds(1.0)).abs()
            < Seconds(0.001)
    );

    let mut cmd2 = ChangeClipLoopCommand::new(clip_id.clone(), false, true, Beats(0.0), Beats(2.0));
    cmd2.execute(&mut project);
    assert!(
        project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .is_looping
    );

    // Undo both
    cmd2.undo(&mut project);
    assert!(
        !project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .is_looping
    );

    cmd1.undo(&mut project);
    assert!(
        (project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - Seconds(0.0)).abs()
            < Seconds(0.001)
    );
    assert!(
        (project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .start_beat
            - original_beat)
            .abs()
            < Beats(0.001)
    );
}

fn make_effect(effect_type: &PresetTypeId) -> PresetInstance {
    PresetInstance::new(effect_type.clone())
}

fn make_driver() -> ParameterDriver {
    ParameterDriver {
        param_id: std::borrow::Cow::Borrowed("amount"),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        free_period_beats: None,
        legacy_param_index: None,
        is_paused_by_user: false,
    }
}

fn make_envelope() -> ParamEnvelope {
    let mut env = ParamEnvelope::new("x");
    env.target_normalized = 0.75; // the card's "Amount" (depth)
    env
}

// ─── Undo-blind fix commands (invariant audit 2026-03-23) ───

#[test]
fn toggle_export_hdr_undo_roundtrip() {
    let mut project = make_test_project();
    assert!(!project.settings.export_hdr);

    let mut cmd = ToggleExportHdrCommand::new(false);
    cmd.execute(&mut project);
    assert!(project.settings.export_hdr);

    cmd.undo(&mut project);
    assert!(!project.settings.export_hdr);
}

#[test]
fn change_midi_channel_undo_roundtrip() {
    let mut project = make_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let old_channel = project.timeline.layers[0].midi_channel;

    let mut cmd = ChangeLayerMidiChannelCommand::new(layer_id, old_channel, 5);
    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].midi_channel, 5);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].midi_channel, old_channel);
}

#[test]
fn set_display_dimensions_undo_roundtrip() {
    let mut project = make_test_project();
    let old_w = project.settings.output_width;
    let old_h = project.settings.output_height;

    let mut cmd = SetDisplayDimensionsCommand::new(old_w, old_h, 3840, 2160);
    cmd.execute(&mut project);
    assert_eq!(project.settings.output_width, 3840);
    assert_eq!(project.settings.output_height, 2160);

    cmd.undo(&mut project);
    assert_eq!(project.settings.output_width, old_w);
    assert_eq!(project.settings.output_height, old_h);
}

#[test]
fn reorder_effect_group_undo_roundtrip() {
    let mut project = make_test_project();
    // Add 3 master effects
    let fx_a = PresetInstance::new(PresetTypeId::BLOOM);
    let fx_b = PresetInstance::new(PresetTypeId::HALATION);
    let fx_c = PresetInstance::new(PresetTypeId::GLITCH);
    project.settings.master_effects = vec![fx_a.clone(), fx_b.clone(), fx_c.clone()];

    let old_effects = project.settings.master_effects.clone();
    // Reorder: move [Bloom, CRT, Glitch] → [CRT, Glitch, Bloom]
    let new_effects = vec![fx_b.clone(), fx_c.clone(), fx_a.clone()];

    let mut cmd =
        ReorderEffectGroupCommand::new(EffectTarget::Master, old_effects.clone(), new_effects);
    cmd.execute(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::HALATION
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::GLITCH
    );
    assert_eq!(
        *project.settings.master_effects[2].effect_type(),
        PresetTypeId::BLOOM
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::HALATION
    );
    assert_eq!(
        *project.settings.master_effects[2].effect_type(),
        PresetTypeId::GLITCH
    );
}

// ─── Project load → cache verification ───

#[test]
fn project_load_verifies_caches() {
    // Default project with layers and clips
    let mut project = make_test_project();

    // Simulate deserialization: call on_after_deserialize
    project.on_after_deserialize();

    // Verify clip lookup cache is populated
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    assert!(
        project.timeline.find_clip_by_id(&clip_id).is_some(),
        "clip_lookup cache must be populated after on_after_deserialize",
    );

    // Verify layer index cache is populated
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let layer_id = &layer_id;
    assert!(
        project.timeline.find_layer_index_by_id(layer_id).is_some(),
        "layer_id_to_index cache must be populated after on_after_deserialize",
    );

    // Verify layer indices are synced
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        assert_eq!(
            layer.index, i as i32,
            "layer.index must match position after reindex"
        );
    }

    // clip.layer_id is now a legacy deserialization-only field — no longer synced
}

#[test]
fn project_load_strips_unknown_effects() {
    let mut project = make_test_project();
    // Add a valid and an unknown effect to master
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::UNKNOWN));
    assert_eq!(project.settings.master_effects.len(), 2);

    project.strip_unknown_effects();
    assert_eq!(project.settings.master_effects.len(), 1);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
}

// ─── ToggleEffectParamExposeCommand (Phase 3 Commit 3) ────────────

fn meta_default() -> InnerParamMeta {
    InnerParamMeta {
        label: "Translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
    }
}

#[test]
fn expose_effect_param_command_undo_roundtrip() {
    // Build a project with a Bloom master effect that has 2 static
    // params (amount + threshold per the test inventory at the top
    // of this file). Expose UVTransform.translate as a user binding;
    // assert state, undo, assert state.
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.base_tracked = true; // slots already carry base = value (fork #16)
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        true, // expose
        meta_default(),
    );

    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    let binding = &ub[0];
    assert_eq!(binding.id, "user.uv_transform.translate.1");
    assert_eq!(binding.node_id, "uv_transform");
    assert_eq!(binding.inner_param, "translate");
    assert_eq!(binding.label, "Translate");
    // params: [0.5 (amount), 1.0 (threshold), 0.0 (user binding default)].
    assert_eq!(fx.params.len(), 3);
    assert_eq!(fx.params.get("amount").unwrap().value, 0.5);
    assert_eq!(fx.params.get("threshold").unwrap().value, 1.0);
    assert_eq!(
        fx.params.get("user.uv_transform.translate.1").unwrap().value,
        0.0
    );
    assert_eq!(
        fx.params.iter().map(|p| p.base).collect::<Vec<_>>(),
        vec![0.5, 1.0, 0.0]
    );

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(fx.user_param_bindings().is_empty());
    assert_eq!(fx.params.len(), 2);
    assert_eq!(fx.params.get("amount").unwrap().value, 0.5);
    assert_eq!(fx.params.get("threshold").unwrap().value, 1.0);
    assert_eq!(
        fx.params.iter().map(|p| p.base).collect::<Vec<_>>(),
        vec![0.5, 1.0]
    );

    // Re-execute (redo) yields the same binding id (deterministic
    // generator: binding list is empty after undo, so we land on
    // `.1` again).
    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    assert_eq!(ub[0].id, "user.uv_transform.translate.1");
}

#[test]
fn expose_already_exposed_is_idempotent_noop() {
    // Ticking an already-on checkbox is a no-op. The command must
    // not panic, and undo must also be a no-op (not remove the
    // pre-existing binding).
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.base_tracked = true; // slots already carry base = value (fork #16)
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_default(),
    );
    cmd.execute(&mut project);
    // Still 1 binding (not 2 — execute is idempotent).
    assert_eq!(
        project.settings.master_effects[0].user_param_bindings().len(),
        1
    );
    cmd.undo(&mut project);
    // Pre-existing binding is preserved (the no-op execute recorded
    // ReverseState::None, so undo did nothing).
    assert_eq!(
        project.settings.master_effects[0].user_param_bindings().len(),
        1
    );
}

#[test]
fn unexpose_effect_param_command_undo_roundtrip() {
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.base_tracked = true; // slots already carry base = value (fork #16)
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    // Drag the slider — the user-tail entry changed.
    {
        let p = fx.params.get_mut("user.uv_transform.translate.1").unwrap();
        p.value = 0.42;
        p.base = 0.42;
    }
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false, // unexpose
        meta_default(),
    );

    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(fx.user_param_bindings().is_empty());
    assert_eq!(fx.params.len(), 2);
    assert_eq!(fx.params.get("amount").unwrap().value, 0.5);
    assert_eq!(fx.params.get("threshold").unwrap().value, 1.0);

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    assert_eq!(ub[0].id, "user.uv_transform.translate.1");
    // Slot value restored — including the dragged 0.42, NOT the binding default.
    let restored = fx.params.get("user.uv_transform.translate.1").unwrap();
    assert!((restored.value - 0.42).abs() < f32::EPSILON);
    assert!(
        (restored.base - 0.42).abs() < f32::EPSILON,
        "base value also restored"
    );
}

#[test]
fn unexpose_when_not_exposed_is_noop() {
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    cmd.undo(&mut project);
    assert!(
        project.settings.master_effects[0]
            .user_param_bindings()
            .is_empty()
    );
    let fx = &project.settings.master_effects[0];
    assert_eq!(fx.params.len(), 2);
    assert_eq!(fx.params.get("amount").unwrap().value, 0.5);
    assert_eq!(fx.params.get("threshold").unwrap().value, 1.0);
}

#[test]
fn generate_user_param_id_collision_probe() {
    // Linear probe lands on the smallest free .n suffix.
    let existing = [
        UserParamBinding {
            id: "user.uv_transform.translate.1".to_string(),
            label: String::new(),
            node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        },
        UserParamBinding {
            id: "user.uv_transform.translate.2".to_string(),
            label: String::new(),
            node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        },
    ];
    let existing_ids: Vec<String> = existing.iter().map(|b| b.id.clone()).collect();
    let id = generate_user_param_id("uv_transform", "translate", &existing_ids);
    assert_eq!(id, "user.uv_transform.translate.3");
    // Different inner param under same handle gets a fresh prefix.
    let id2 = generate_user_param_id("uv_transform", "scale", &existing_ids);
    assert_eq!(id2, "user.uv_transform.scale.1");
}

/// Regression for the post-Phase-5 follow-up: un-exposing a
/// user-bound param must prune any drivers / Ableton mappings /
/// layer envelopes that referenced that binding's `param_id`. The
/// old behaviour left them sitting on the data model forever — never
/// matched by `find_driver` / the modulation evaluators / the Ableton
/// router, never applied, never editable. The fix captures them on
/// the reverse state so re-exposing restores every modulation
/// surface verbatim.
#[test]
fn unexpose_prunes_orphan_drivers_and_undo_restores_them() {
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    // Attach a driver keyed to the user binding's id. Plus a driver
    // for the static `amount` param — that one must survive the
    // un-expose untouched.
    fx.drivers = Some(vec![
        ParameterDriver {
            param_id: std::borrow::Cow::Owned("user.uv_transform.translate.1".to_string()),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        },
        ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            beat_division: BeatDivision::Eighth,
            waveform: DriverWaveform::Triangle,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        },
    ]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false, // unexpose
        meta_default(),
    );
    cmd.execute(&mut project);

    let fx = &project.settings.master_effects[0];
    let drivers = fx.drivers.as_ref().expect("drivers still present");
    assert_eq!(
        drivers.len(),
        1,
        "the orphan driver targeting the unexposed binding must be pruned",
    );
    assert_eq!(
        drivers[0].param_id, "amount",
        "the static-param driver must survive untouched",
    );

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    let drivers = fx.drivers.as_ref().expect("drivers vec restored");
    assert_eq!(drivers.len(), 2, "undo must restore the pruned driver");
    assert!(
        drivers
            .iter()
            .any(|d| d.param_id == "user.uv_transform.translate.1"),
        "the previously pruned driver must come back with its original param_id",
    );
    // Re-execute: prune + restore is idempotent across redo.
    cmd.execute(&mut project);
    let drivers = &project.settings.master_effects[0]
        .drivers
        .as_ref()
        .expect("drivers vec retained for static row");
    assert_eq!(drivers.len(), 1);
}

#[test]
fn unexpose_prunes_orphan_ableton_mappings_and_undo_restores_them() {
    use manifold_core::ableton_mapping::{
        AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
    };
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    let address = AbletonMacroAddress {
        track_id: 0,
        device_id: 0,
        param_id: 0,
        device_identity: AbletonDeviceIdentity {
            device_class_name: "InstrumentGroupDevice".to_string(),
        },
        track_name: "Bass".into(),
        device_name: "Rack".into(),
        macro_name: "Macro 1".into(),
    };
    fx.ableton_mappings = Some(vec![AbletonParamMapping {
        param_id: std::borrow::Cow::Owned("user.uv_transform.translate.1".to_string()),
        address: address.clone(),
        range_min: 0.0,
        range_max: 1.0,
        inverted: false,
        legacy_param_index: None,
        last_value: 0.0,
        status: AbletonMappingStatus::Active,
    }]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(
        fx.ableton_mappings.is_none(),
        "the only mapping targeted the unexposed binding — vec should collapse to None",
    );

    cmd.undo(&mut project);
    let mappings = project.settings.master_effects[0]
        .ableton_mappings
        .as_ref()
        .expect("ableton mappings restored");
    assert_eq!(mappings.len(), 1);
    assert_eq!(
        mappings[0].param_id,
        "user.uv_transform.translate.1",
        "undo must reinstate the pruned mapping with its original param_id",
    );
}

#[test]
fn unexpose_prunes_orphan_envelopes_and_undo_restores_them() {
    let mut project = make_test_project();
    let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
    fx.params = manifold_core::params::ParamManifest::from_params(vec![
        slot("amount", 0.5, true),
        slot("threshold", 1.0, true),
    ]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    // Envelope-home unification: envelopes ride on the instance, keyed by
    // param_id. Plant one on the unexposed binding (pruned) and one on an
    // unrelated param (kept).
    fx.envelopes_mut().push(ParamEnvelope::new(std::borrow::Cow::Owned(
        "user.uv_transform.translate.1".to_string(),
    )));
    fx.envelopes_mut()
        .push(ParamEnvelope::new(std::borrow::Cow::Borrowed("amount")));
    project.timeline.layers[0].effects = Some(vec![fx]);

    let effect_id = project.timeline.layers[0].effects.as_ref().unwrap()[0]
        .id
        .clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
    let envs = fx.envelopes.as_ref().expect("instance envelopes vec retained");
    assert_eq!(
        envs.len(),
        1,
        "the orphan envelope must be pruned; the unrelated one survives",
    );
    assert_eq!(envs[0].param_id, "amount");

    cmd.undo(&mut project);
    let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
    let envs = fx.envelopes.as_ref().expect("instance envelopes vec restored");
    assert_eq!(envs.len(), 2);
    assert!(
        envs.iter().any(|e| e.param_id == "user.uv_transform.translate.1"),
        "undo must reinstate the pruned envelope",
    );
}

// ─── Session Commands (P3) ───
// docs/SESSION_MODE_DESIGN.md §7 — grid editing + timeline<->session capture/paste.

fn make_session_test_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = manifold_core::units::Bpm(120.0);
    project.settings.time_signature_numerator = 4;

    project
        .timeline
        .insert_layer(0, Layer::new("Video Layer".into(), LayerType::Video, 0));
    project
        .timeline
        .insert_layer(1, Layer::new("Gen Layer".into(), LayerType::Generator, 1));

    // A video clip spanning [0, 8) beats, in_point offset so head-trim math
    // is observable (advancing in_point off zero is the failure-prone case).
    let video_clip = TimelineClip {
        video_clip_id: "vid1".into(),
        start_beat: Beats(0.0),
        duration_beats: Beats(8.0),
        in_point: Seconds(1.0),
        ..Default::default()
    };
    project.timeline.layers[0].restore_clip(video_clip);

    project.timeline.rebuild_clip_lookup();
    project
}

#[test]
fn add_scene_undo_roundtrip() {
    let mut project = make_session_test_project();
    let scene = Scene {
        id: SceneId::new("scene-1"),
        name: "Intro".into(),
        color: None,
    };
    let mut cmd = AddSceneCommand::new(scene.clone(), 0);

    cmd.execute(&mut project);
    assert_eq!(project.session.scenes.len(), 1);
    assert_eq!(project.session.scenes[0].name, "Intro");

    cmd.undo(&mut project);
    assert!(project.session.scenes.is_empty());
}

#[test]
fn remove_scene_removes_its_slots_and_undo_restores_both() {
    let mut project = make_session_test_project();
    let scene_id = SceneId::new("scene-1");
    project.session.scenes.push(Scene {
        id: scene_id.clone(),
        name: "Drop".into(),
        color: None,
    });
    let layer_id = project.timeline.layers[0].layer_id.clone();
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence::default(),
        name: String::new(),
        color: None,
    });

    let mut cmd = RemoveSceneCommand::new(scene_id.clone());
    cmd.execute(&mut project);
    assert!(project.session.scenes.is_empty());
    assert!(project.session.slots.is_empty());

    cmd.undo(&mut project);
    assert_eq!(project.session.scenes.len(), 1);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].scene_id, scene_id);
}

#[test]
fn rename_scene_undo_roundtrip() {
    let mut project = make_session_test_project();
    let scene_id = SceneId::new("scene-1");
    project.session.scenes.push(Scene {
        id: scene_id.clone(),
        name: "Old".into(),
        color: None,
    });

    let mut cmd = RenameSceneCommand::new(scene_id.clone(), "Old".into(), "New".into());
    cmd.execute(&mut project);
    assert_eq!(project.session.scenes[0].name, "New");

    cmd.undo(&mut project);
    assert_eq!(project.session.scenes[0].name, "Old");
}

#[test]
fn reorder_scene_undo_roundtrip() {
    let mut project = make_session_test_project();
    let a = Scene {
        id: SceneId::new("a"),
        name: "A".into(),
        color: None,
    };
    let b = Scene {
        id: SceneId::new("b"),
        name: "B".into(),
        color: None,
    };
    project.session.scenes = vec![a.clone(), b.clone()];

    let old_order = project.session.scenes.clone();
    let new_order = vec![b.clone(), a.clone()];
    let mut cmd = ReorderSceneCommand::new(old_order, new_order);

    cmd.execute(&mut project);
    assert_eq!(project.session.scenes[0].id, b.id);

    cmd.undo(&mut project);
    assert_eq!(project.session.scenes[0].id, a.id);
}

#[test]
fn set_slot_command_set_replace_clear_undo_roundtrip() {
    let mut project = make_session_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-1");

    let slot_a = SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence {
            length_beats: Beats(4.0),
            clips: Vec::new(),
        },
        name: "A".into(),
        color: None,
    };

    // Set on an empty cell.
    let mut set_cmd = SetSlotCommand::new(layer_id.clone(), scene_id.clone(), Some(slot_a.clone()));
    set_cmd.execute(&mut project);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].name, "A");

    set_cmd.undo(&mut project);
    assert!(project.session.slots.is_empty());

    // Replace.
    set_cmd.execute(&mut project);
    let slot_b = SessionSlot {
        name: "B".into(),
        ..slot_a.clone()
    };
    let mut replace_cmd = SetSlotCommand::new(layer_id.clone(), scene_id.clone(), Some(slot_b));
    replace_cmd.execute(&mut project);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].name, "B");

    replace_cmd.undo(&mut project);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].name, "A");

    // Clear.
    let mut clear_cmd = SetSlotCommand::new(layer_id.clone(), scene_id.clone(), None);
    clear_cmd.execute(&mut project);
    assert!(project.session.slots.is_empty());

    clear_cmd.undo(&mut project);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].name, "A");
}

#[test]
fn capture_range_creates_scene_and_slot_undo_roundtrip() {
    let mut project = make_session_test_project();

    let mut cmd = CaptureRangeToSceneCommand::new(Beats(2.0), Beats(6.0), None);
    cmd.execute(&mut project);

    assert_eq!(project.session.scenes.len(), 1);
    assert_eq!(project.session.scenes[0].name, "Scene 1");
    // Only the video layer has a clip in range; the (empty) generator layer
    // gets no slot.
    assert_eq!(project.session.slots.len(), 1);
    let slot = &project.session.slots[0];
    assert_eq!(slot.sequence.length_beats, Beats(4.0));
    assert_eq!(slot.sequence.clips.len(), 1);
    assert_eq!(slot.sequence.clips[0].start_beat, Beats(0.0)); // rebased to sequence-relative
    assert_eq!(slot.sequence.clips[0].duration_beats, Beats(4.0));

    cmd.undo(&mut project);
    assert!(project.session.scenes.is_empty());
    assert!(project.session.slots.is_empty());
}

#[test]
fn capture_range_names_scene_from_nearest_marker_at_or_before() {
    let mut project = make_session_test_project();
    let mut early = TimelineMarker::new(Beats(0.0));
    early.name = "Verse".into();
    project.timeline.markers.push(early);
    let mut late = TimelineMarker::new(Beats(10.0));
    late.name = "Should Not Match".into();
    project.timeline.markers.push(late);

    let mut cmd = CaptureRangeToSceneCommand::new(Beats(2.0), Beats(6.0), None);
    cmd.execute(&mut project);

    assert_eq!(project.session.scenes[0].name, "Verse");
}

/// The load-bearing parity check (§7 + P3 gate): `CaptureRangeToSceneCommand`
/// must reuse the existing split path's head-trim math, not reimplement it.
/// This runs the REAL `SplitClipCommand` (via
/// `EditingService::split_clip_at_beat`) on one project and the REAL
/// `CaptureRangeToSceneCommand` on an identical project, then compares the
/// resulting `in_point` values byte-for-byte — not a reimplementation of the
/// formula in the test, an execution of both commands.
#[test]
fn capture_range_trim_matches_split_command_in_point_math() {
    let mut split_project = make_session_test_project();
    let clip_id = split_project.timeline.layers[0].clips[0].id.clone();
    let spb = split_project.settings.seconds_per_beat();

    let mut split_cmd =
        EditingService::split_clip_at_beat(&split_project, clip_id.as_str(), Beats(2.0), spb)
            .expect("split at beat 2.0 should be valid (strictly inside the clip)");
    let tail_id = split_cmd.tail_clip_id().clone();
    split_cmd.execute(&mut split_project);
    let split_tail_in_point = split_project
        .timeline
        .find_clip_by_id_mut(tail_id.as_str())
        .expect("tail clip exists after split executes")
        .in_point;

    let mut capture_project = make_session_test_project();
    let mut capture_cmd = CaptureRangeToSceneCommand::new(Beats(2.0), Beats(6.0), None);
    capture_cmd.execute(&mut capture_project);
    let captured_in_point = capture_project.session.slots[0].sequence.clips[0].in_point;

    assert!(
        (captured_in_point - split_tail_in_point).abs() < Seconds(1e-9),
        "capture trim in_point {captured_in_point:?} != split command's tail in_point {split_tail_in_point:?} — \
         the capture path must reuse the split path's head-trim math exactly",
    );
}

#[test]
fn paste_slot_to_timeline_undo_roundtrip() {
    let mut project = make_session_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-1");

    let seq_clip = TimelineClip {
        video_clip_id: "seq-vid".into(),
        start_beat: Beats(0.0),
        duration_beats: Beats(2.0),
        ..Default::default()
    };
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence {
            length_beats: Beats(2.0),
            clips: vec![seq_clip],
        },
        name: String::new(),
        color: None,
    });

    let initial_clip_count = project.timeline.layers[0].clips.len();
    let mut cmd = PasteSlotToTimelineCommand::new(layer_id.clone(), scene_id.clone(), Beats(20.0));
    cmd.execute(&mut project);

    assert_eq!(project.timeline.layers[0].clips.len(), initial_clip_count + 1);
    let pasted = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.video_clip_id == "seq-vid")
        .expect("pasted clip present");
    assert_eq!(pasted.start_beat, Beats(20.0));
    // Fresh id both directions — never shares a ClipId with the slot's stored sequence.
    assert_ne!(pasted.id, project.session.slots[0].sequence.clips[0].id);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_clip_count);
}

#[test]
fn paste_slot_to_timeline_handles_collision_and_undo_restores_original() {
    let mut project = make_session_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-1");

    let seq_clip = TimelineClip {
        video_clip_id: "seq-vid".into(),
        start_beat: Beats(0.0),
        duration_beats: Beats(2.0),
        ..Default::default()
    };
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence {
            length_beats: Beats(2.0),
            clips: vec![seq_clip],
        },
        name: String::new(),
        color: None,
    });

    // The existing "vid1" clip occupies [0, 8). Pasting at beat 0 collides
    // head-on; `enforce_non_overlap_for` (via `Layer::add_clip`) must trim it.
    let mut cmd = PasteSlotToTimelineCommand::new(layer_id.clone(), scene_id.clone(), Beats(0.0));
    cmd.execute(&mut project);

    let existing = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.video_clip_id == "vid1")
        .expect("original clip still present, trimmed");
    assert_eq!(existing.start_beat, Beats(2.0));

    cmd.undo(&mut project);
    let restored = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.video_clip_id == "vid1")
        .expect("original clip restored");
    assert_eq!(restored.start_beat, Beats(0.0));
    assert_eq!(restored.duration_beats, Beats(8.0));
}

#[test]
fn delete_layer_removes_its_session_slots_and_undo_restores() {
    let mut project = make_session_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-1");
    project.session.scenes.push(Scene {
        id: scene_id.clone(),
        name: "Drop".into(),
        color: None,
    });
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence::default(),
        name: String::new(),
        color: None,
    });

    let layer = project.timeline.layers[0].clone();
    let mut cmd = DeleteLayerCommand::new(layer);
    cmd.execute(&mut project);

    assert!(project.session.slots.is_empty());

    cmd.undo(&mut project);
    assert_eq!(project.session.slots.len(), 1);
    assert_eq!(project.session.slots[0].layer_id, layer_id);
}
