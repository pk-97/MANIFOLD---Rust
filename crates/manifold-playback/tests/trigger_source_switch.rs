use manifold_core::GraphTarget;
use manifold_core::audio_mod::{TriggerAction, WrapMode};
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{ParamEnvelope, PresetInstance};
use manifold_core::layer::Layer;
use manifold_core::params::{ClipTriggerSource, Param};
use manifold_core::project::Project;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::{LayerType, PlaybackState};
use manifold_core::{Beats, Bpm, LayerId, PresetTypeId, SceneId, Seconds};
use manifold_editing::command::Command;
use manifold_editing::commands::trigger_source::SetParamClipTriggerSourceCommand;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::external_follow::FollowProfile;

fn add_level_param(instance: &mut PresetInstance) {
    instance.params.push(Param::bundled(ParamSpecDef {
        id: "level".into(),
        name: "Level".into(),
        min: 0.0,
        max: 8.0,
        default_value: 0.0,
        whole_numbers: true,
        ..ParamSpecDef::default()
    }));
}

fn step_envelope() -> ParamEnvelope {
    step_envelope_for("level")
}

fn step_envelope_for(param_id: &'static str) -> ParamEnvelope {
    let mut envelope = ParamEnvelope::new(param_id);
    envelope.action = TriggerAction::Step {
        amount: 1.0,
        wrap: WrapMode::Clamp,
    };
    envelope
}

fn trigger_source_project() -> (Project, LayerId, LayerId, LayerId, SceneId) {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);

    let group = Layer::new("Group".into(), LayerType::Group, 0);
    let group_id = group.layer_id.clone();

    let mut owner = Layer::new_generator("TestGen".into(), PresetTypeId::new("TestGen"), 1);
    owner.parent_layer_id = Some(group_id.clone());
    owner.is_muted = true;
    add_level_param(owner.gen_params_mut().unwrap());
    owner.gen_params_mut().unwrap().envelopes = Some(vec![step_envelope()]);
    let owner_id = owner.layer_id.clone();

    let mut source_a = Layer::new_trigger("A".into(), owner_id.clone(), 2);
    source_a
        .clips
        .push(TimelineClip::new_trigger(Beats::ZERO, Beats(1.0)));
    let source_a_id = source_a.layer_id.clone();

    let mut source_b = Layer::new_trigger("B".into(), group_id, 3);
    source_b
        .clips
        .push(TimelineClip::new_trigger(Beats::ZERO, Beats(4.0)));
    source_b
        .clips
        .push(TimelineClip::new_trigger(Beats(4.0), Beats(1.0)));
    let source_b_id = source_b.layer_id.clone();

    let scene_id = SceneId::new("switch-scene");
    project.session.scenes.push(Scene {
        id: scene_id.clone(),
        name: "Switch".into(),
        color: None,
    });
    project.session.slots.push(SessionSlot {
        layer_id: source_b_id.clone(),
        scene_id: scene_id.clone(),
        name: "B session".into(),
        color: None,
        sequence: ClipSequence {
            length_beats: Beats(4.0),
            clips: vec![TimelineClip::new_trigger(Beats::ZERO, Beats(1.0))],
        },
    });

    owner
        .gen_params_mut().unwrap()
        .params
        .get_mut("level")
        .unwrap()
        .clip_trigger_source = ClipTriggerSource::Lane {
        layer_id: source_a_id.clone(),
    };
    project.timeline.layers = vec![group, owner, source_a, source_b];
    (project, owner_id, source_a_id, source_b_id, scene_id)
}

/// Playing under an external clock (D6, D8): the follow segment is the
/// converter, and ticks of zero frames never move the grid.
fn play_on_external_clock(engine: &mut PlaybackEngine) {
    engine.set_follow_authority(Some(FollowProfile::FAST));
    engine.set_state(PlaybackState::Playing);
}

/// The external clock jumps to `beat` with no seek side effects: the frame
/// at the beat's time, no hold, the follow segment re-anchored there (D4,
/// D6). No tick runs, so a start the jump registers stays unevaluated.
fn clock_jumps_to(engine: &mut PlaybackEngine, beat: Beats) {
    let t = engine.beat_to_timeline_time(beat);
    engine.locate(t);
}

fn tick(engine: &mut PlaybackEngine) {
    // No step: these tests place the playhead themselves.
    let _ = engine.tick(TickContext {
        frames: 0,
        realtime_now: Seconds(0.0),
        frame_count: 0,
    });
}

fn step_value(engine: &PlaybackEngine, owner_id: &LayerId) -> Option<f32> {
    step_value_for(engine, owner_id, "level")
}

fn step_value_for(engine: &PlaybackEngine, owner_id: &LayerId, param_id: &str) -> Option<f32> {
    engine
        .project()
        .unwrap()
        .timeline
        .find_layer_by_id(owner_id)
        .unwrap()
        .1
        .gen_params()
        .unwrap()
        .envelopes
        .as_ref()
        .unwrap()
        .iter()
        .find(|envelope| envelope.param_id.as_ref() == param_id)
        .and_then(|envelope| envelope.step_value)
}

#[test]
fn source_switch_cuts_old_starts_preserves_phase_and_accepts_new_same_beat_launch() {
    let (project, owner_id, source_a_id, source_b_id, scene_id) = trigger_source_project();
    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);
    engine.locate(Seconds::ZERO);
    engine.sync_clips_to_time();

    let target = GraphTarget::Generator(owner_id.clone());
    let mut command = SetParamClipTriggerSourceCommand::for_assignment(
        target.clone(),
        "level",
        ClipTriggerSource::Lane {
            layer_id: source_b_id.clone(),
        },
    );
    run(&mut engine, &mut command, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();

    // A stepped value counts the assigned source's arrangement starts at the
    // beat, so an undo shows A's count once and never replays a queued start.
    run(&mut engine, &mut command, true);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(1.0));

    // Re-apply B: B's start at beat 0 counts once.
    run(&mut engine, &mut command, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(1.0));

    // Phase is available without a new edge; the muted owner does not mute
    // its trigger children.
    engine.locate(Seconds(0.5));
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(1.0));
    assert_eq!(
        engine.clip_controls().elapsed(
            &ClipTriggerSource::Lane {
                layer_id: source_b_id.clone(),
            },
            Some(&owner_id),
            Beats(1.0),
        ),
        Some(Beats(1.0))
    );

    // A session launch is live input: it adds one step on top of the
    // arrangement count, even though starts queued before the binding are cut off.
    run(&mut engine, &mut command, true);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();
    run(&mut engine, &mut command, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();
    engine.session_set_quantize(Beats::ZERO);
    engine.session_launch_slot(source_b_id.clone(), scene_id);
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(2.0));

    // Back on the arrangement, B's start at beat 4 counts; the live step stays.
    engine.session_back_to_arrangement(Some(source_b_id.clone()));
    engine.locate(Seconds(2.0));
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(3.0));

    // A seek drops the live step: beat 0 shows what it showed the first time.
    engine.seek_to(Seconds::ZERO);
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(1.0), "seek returns to the timeline's count");

    // Keep the command target live and prove the stable IDs survived engine
    // initialization and all source edits.
    assert_eq!(
        engine
            .project()
            .unwrap()
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get("level")
            .unwrap()
            .clip_trigger_source,
        ClipTriggerSource::Lane {
            layer_id: source_b_id
        }
    );
    assert!(
        engine
            .project()
            .unwrap()
            .timeline
            .find_layer_by_id(&source_a_id)
            .is_some()
    );
}

#[test]
fn source_switch_cancels_pending_fire_delivery_and_keeps_future_source_events() {
    use manifold_core::AudioSendId;
    use manifold_core::audio_mod::{AudioFeature, ParameterAudioMod};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_playback::modulation::TriggerSourceStamp;
    let (mut project, owner_id, _, source_b_id, _) = trigger_source_project();
    let instance = project
        .graph_target_owner_mut(&GraphTarget::Generator(owner_id.clone()))
        .unwrap();
    instance.envelopes = None;
    instance.params.get_mut("level").unwrap().spec.is_trigger = true;
    let mut response = ParameterAudioMod::new(
        "level".into(),
        AudioSendId::new("unused"),
        AudioFeature::default(),
    );
    response.trigger_mode = Some(TriggerFireMode::ClipEdge);
    instance.audio_mods_mut().push(response);
    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);
    tick(&mut engine);
    let target = GraphTarget::Generator(owner_id);
    assert_eq!(
        engine
            .project()
            .unwrap()
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get("level")
            .unwrap()
            .value,
        1.0
    );
    let mut command = SetParamClipTriggerSourceCommand::for_assignment(
        target,
        "level",
        ClipTriggerSource::Lane {
            layer_id: source_b_id.clone(),
        },
    );
    run(&mut engine, &mut command, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    assert_eq!(
        engine.with_trigger_pulses(|pulses, _, _| pulses.len()),
        Some(0)
    );
    engine.locate(Seconds(2.0));
    tick(&mut engine);
    engine
        .with_trigger_pulses(|pulses, _, _| {
            assert_eq!(pulses.len(), 1);
            assert!(matches!(&pulses[0].pulse.source_stamp,
            TriggerSourceStamp::Clip { layer_id, beat, .. }
            if *layer_id == source_b_id && *beat == Beats(4.0)));
        })
        .unwrap();
}

#[test]
fn direct_backward_clock_clears_pending_fire_without_retriggering_active_clip() {
    use manifold_core::AudioSendId;
    use manifold_core::audio_mod::{AudioFeature, ParameterAudioMod};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_playback::modulation::TriggerSourceStamp;

    for use_set_time in [false, true] {
        let (mut project, owner_id, _, source_b_id, _) = trigger_source_project();
        let instance = project
            .graph_target_owner_mut(&GraphTarget::Generator(owner_id.clone()))
            .unwrap();
        instance.envelopes = None;
        instance.params.get_mut("level").unwrap().spec.is_trigger = true;
        let mut response = ParameterAudioMod::new(
            "level".into(),
            AudioSendId::new("unused"),
            AudioFeature::default(),
        );
        response.trigger_mode = Some(TriggerFireMode::ClipEdge);
        instance.audio_mods_mut().push(response);
        instance.params.get_mut("level").unwrap().clip_trigger_source =
            ClipTriggerSource::Lane { layer_id: source_b_id.clone() };

        let mut engine = PlaybackEngine::new(Vec::new());
        engine.initialize(project);
        play_on_external_clock(&mut engine);
        clock_jumps_to(&mut engine, Beats::ZERO);
        engine.sync_clips_to_time();
        tick(&mut engine);
        assert_eq!(engine.project().unwrap().graph_target_owner(&GraphTarget::Generator(owner_id.clone())).unwrap()
            .params.get("level").unwrap().value, 1.0);
        let transport_epoch = engine.transport_epoch();

        clock_jumps_to(&mut engine, Beats(2.0));
        engine.sync_clips_to_time();
        tick(&mut engine);
        if use_set_time {
            engine.locate(Seconds::ZERO);
        } else {
            clock_jumps_to(&mut engine, Beats::ZERO);
        }
        engine.sync_clips_to_time();
        tick(&mut engine);

        assert_eq!(engine.with_trigger_pulses(|pulses, _, _| pulses.len()), Some(0));
        assert_eq!(engine.transport_epoch(), transport_epoch);
        assert_eq!(engine.project().unwrap().graph_target_owner(&GraphTarget::Generator(owner_id.clone())).unwrap()
            .params.get("level").unwrap().value, 1.0);

        clock_jumps_to(&mut engine, Beats(4.0));
        engine.sync_clips_to_time();
        tick(&mut engine);
        assert_eq!(engine.project().unwrap().graph_target_owner(&GraphTarget::Generator(owner_id)).unwrap()
            .params.get("level").unwrap().value, 2.0);
        engine.with_trigger_pulses(|pulses, _, _| {
            assert_eq!(pulses.len(), 1);
            assert!(matches!(&pulses[0].pulse.source_stamp,
                TriggerSourceStamp::Clip { layer_id, beat, .. }
                if *layer_id == source_b_id && *beat == Beats(4.0)));
        }).unwrap();
    }
}

#[test]
fn backward_set_beat_clears_late_source_cutoff_before_earlier_start() {
    let (project, owner_id, _, source_b_id, _) = trigger_source_project();
    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(project);
    play_on_external_clock(&mut engine);
    clock_jumps_to(&mut engine, Beats(4.0));
    engine.sync_clips_to_time();

    let target = GraphTarget::Generator(owner_id.clone());
    let mut command = SetParamClipTriggerSourceCommand::for_assignment(
        target,
        "level",
        ClipTriggerSource::Lane {
            layer_id: source_b_id,
        },
    );
    run(&mut engine, &mut command, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();

    clock_jumps_to(&mut engine, Beats::ZERO);
    engine.sync_clips_to_time();
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), Some(1.0));
}

#[test]
fn backward_clock_clears_unevaluated_later_step_start() {
    let (mut project, owner_id, _, source_b_id, _) = trigger_source_project();
    project
        .graph_target_owner_mut(&GraphTarget::Generator(owner_id.clone()))
        .unwrap()
        .params
        .get_mut("level")
        .unwrap()
        .clip_trigger_source = ClipTriggerSource::Lane {
        layer_id: source_b_id.clone(),
    };
    project
        .timeline
        .find_layer_by_id_mut(&source_b_id)
        .unwrap()
        .1
        .clips = vec![TimelineClip::new_trigger(Beats(4.0), Beats(1.0))];

    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(project);
    play_on_external_clock(&mut engine);
    clock_jumps_to(&mut engine, Beats(4.0));
    engine.sync_clips_to_time();
    assert_eq!(step_value(&engine, &owner_id), None);

    clock_jumps_to(&mut engine, Beats::ZERO);
    engine.sync_clips_to_time();
    tick(&mut engine);
    assert_eq!(step_value(&engine, &owner_id), None);
}

#[test]
fn numeric_trigger_source_roundtrip_assignment_disconnect_and_undo() {
    const PARAM_ID: &str = "roundtrip_level";
    fn assert_value(engine: &PlaybackEngine, owner: &LayerId, expected: f32) {
        let value = engine.project().unwrap().graph_target_owner(&GraphTarget::Generator(owner.clone()))
            .unwrap().params.get(PARAM_ID).unwrap().value;
        assert_eq!(value, expected);
    }

    let (mut project, owner_id, source_a_id, _, _) = trigger_source_project();
    project
        .timeline
        .find_layer_by_id_mut(&owner_id)
        .unwrap()
        .1
        .clips = vec![
        TimelineClip::new_generator(Beats(1.0), Beats(1.0)),
        TimelineClip::new_generator(Beats(3.0), Beats(1.0)),
    ];
    project
        .timeline
        .find_layer_by_id_mut(&source_a_id)
        .unwrap()
        .1
        .clips = vec![
        TimelineClip::new_trigger(Beats::ZERO, Beats(1.0)),
        TimelineClip::new_trigger(Beats(4.0), Beats(1.0)),
        TimelineClip::new_trigger(Beats(8.0), Beats(1.0)),
    ];

    let owner = project
        .graph_target_owner_mut(&GraphTarget::Generator(owner_id.clone()))
        .unwrap();
    owner.params.push(Param::user_added(ParamSpecDef {
        id: PARAM_ID.into(),
        name: "Roundtrip Level".into(),
        min: 0.0,
        max: 8.0,
        default_value: 0.0,
        whole_numbers: true,
        ..ParamSpecDef::default()
    }));
    let mut response = step_envelope_for(PARAM_ID);
    response.enabled = false;
    owner.envelopes.as_mut().unwrap().push(response);

    let target = GraphTarget::Generator(owner_id.clone());
    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(project);

    // A validated assignment arms the existing numeric response and selects
    // the child lane. The parent's interleaved starts must remain irrelevant.
    let mut assign = SetParamClipTriggerSourceCommand::for_assignment(
        target.clone(),
        PARAM_ID,
        ClipTriggerSource::Lane {
            layer_id: source_a_id.clone(),
        },
    );
    run(&mut engine, &mut assign, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    assert!(assign.was_applied());
    assert_eq!(assign.rejection_reason(), None);
    engine.reconcile_clip_control_bindings();
    let assigned = engine
        .project()
        .unwrap()
        .graph_target_owner(&target)
        .unwrap();
    assert_eq!(
        assigned.params.get(PARAM_ID).unwrap().clip_trigger_source,
        ClipTriggerSource::Lane {
            layer_id: source_a_id.clone(),
        }
    );
    assert!(
        assigned
            .envelopes
            .as_ref()
            .unwrap()
            .iter()
            .find(|envelope| envelope.param_id.as_ref() == PARAM_ID)
            .unwrap()
            .enabled
    );
    play_on_external_clock(&mut engine);

    for (beat, expected) in [(0.0, Some(1.0)), (1.0, Some(1.0)), (3.0, Some(1.0))] {
        clock_jumps_to(&mut engine, Beats(beat));
        engine.sync_clips_to_time();
        tick(&mut engine);
        assert_eq!(
            step_value_for(&engine, &owner_id, PARAM_ID),
            expected,
            "beat {beat}"
        );
        assert_value(&engine, &owner_id, expected.unwrap());
    }

    // Disconnecting leaves the authored response untouched but makes its
    // selected source inert: nothing counts, so the base shows. Undo restores
    // the same child connection and its count at the new beat.
    let mut disconnect = SetParamClipTriggerSourceCommand::for_assignment(
        target.clone(),
        PARAM_ID,
        ClipTriggerSource::Disabled,
    );
    run(&mut engine, &mut disconnect, false);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    assert!(disconnect.was_applied());
    engine.reconcile_clip_control_bindings();
    clock_jumps_to(&mut engine, Beats(4.0));
    engine.sync_clips_to_time();
    tick(&mut engine);
    assert_eq!(step_value_for(&engine, &owner_id, PARAM_ID), None);
    assert_value(&engine, &owner_id, 0.0);

    run(&mut engine, &mut disconnect, true);
    engine.mark_clip_bindings_stale(); // the content reaction pass does this after an edit
    engine.reconcile_clip_control_bindings();
    clock_jumps_to(&mut engine, Beats(8.0));
    engine.sync_clips_to_time();
    tick(&mut engine);
    assert_eq!(step_value_for(&engine, &owner_id, PARAM_ID), Some(3.0));
    assert_value(&engine, &owner_id, 3.0);

    let mut save_path = std::env::temp_dir();
    save_path.push(format!(
        "manifold_trigger_source_roundtrip_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(engine.project().unwrap(), &save_path)
        .expect("save_project_v1 should preserve the user-added routed parameter");
    let reloaded = manifold_io::loader::load_project(&save_path).expect("reload should succeed");
    std::fs::remove_file(&save_path).ok();
    assert_eq!(
        reloaded
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get(PARAM_ID)
            .unwrap()
            .clip_trigger_source,
        ClipTriggerSource::Lane {
            layer_id: source_a_id.clone(),
        }
    );

    let mut loaded_engine = PlaybackEngine::new(Vec::new());
    loaded_engine.initialize(reloaded.clone());
    play_on_external_clock(&mut loaded_engine);
    for (beat, expected) in [
        (0.0, Some(1.0)),
        (1.0, Some(1.0)),
        (3.0, Some(1.0)),
        (4.0, Some(2.0)),
    ] {
        clock_jumps_to(&mut loaded_engine, Beats(beat));
        loaded_engine.sync_clips_to_time();
        tick(&mut loaded_engine);
        assert_eq!(
            step_value_for(&loaded_engine, &owner_id, PARAM_ID),
            expected,
            "reloaded beat {beat}"
        );
        assert_value(&loaded_engine, &owner_id, expected.unwrap());
    }

    // A persisted source that no longer exists remains explicit and inert.
    let mut missing = reloaded;
    let missing_id = LayerId::new("missing-trigger-source");
    let mut restore_missing = SetParamClipTriggerSourceCommand::new(
        target.clone(),
        PARAM_ID,
        ClipTriggerSource::Lane {
            layer_id: missing_id.clone(),
        },
    );
    restore_missing.execute(&mut missing);
    assert!(restore_missing.was_applied());
    let mut missing_path = std::env::temp_dir();
    missing_path.push(format!(
        "manifold_trigger_source_missing_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&missing, &missing_path)
        .expect("save missing source project should succeed");
    let missing = manifold_io::loader::load_project(&missing_path)
        .expect("reload with an unresolved source should succeed");
    std::fs::remove_file(&missing_path).ok();
    assert_eq!(
        missing
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get(PARAM_ID)
            .unwrap()
            .clip_trigger_source,
        ClipTriggerSource::Lane {
            layer_id: missing_id,
        }
    );
    let mut missing_engine = PlaybackEngine::new(Vec::new());
    missing_engine.initialize(missing);
    play_on_external_clock(&mut missing_engine);
    clock_jumps_to(&mut missing_engine, Beats::ZERO);
    missing_engine.sync_clips_to_time();
    tick(&mut missing_engine);
    assert_eq!(step_value_for(&missing_engine, &owner_id, PARAM_ID), None);
    assert_value(&missing_engine, &owner_id, 0.0);
    clock_jumps_to(&mut missing_engine, Beats(1.0));
    missing_engine.sync_clips_to_time();
    tick(&mut missing_engine);
    assert_eq!(step_value_for(&missing_engine, &owner_id, PARAM_ID), None);
    assert_value(&missing_engine, &owner_id, 0.0);
}

/// Apply or revert a command through the gateway, as the content thread would.
/// A command may learn its parts while it runs, so the gateway stamps what
/// `execute` or `undo` reports, never a set read beforehand.
fn run(engine: &mut PlaybackEngine, command: &mut dyn Command, undo: bool) {
    let edit = engine.project_edit().expect("project");
    manifold_editing::service::EditingService::new().mutate_with_history(edit, |p, _| {
        ((), if undo { command.undo(p) } else { command.execute(p) })
    });
}
