use manifold_core::project::Project;

use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

// The run loop publishes after each command drain, before the paced
// frame, so the UI confirms edits even while the GPU is behind.

fn one_layer(layer_type: manifold_core::types::LayerType) -> Project {
    let mut project = Project::default();
    let mut layer = manifold_core::layer::Layer::new("Pad".into(), layer_type, 0);
    layer.midi_note = 60;
    layer.midi_channel = -1;
    project.timeline.layers.push(layer);
    project.timeline.reindex_layers();
    project
}

fn last_snapshot(
    rx: &crossbeam_channel::Receiver<crate::content_state::ContentState>,
) -> Option<std::sync::Arc<Project>> {
    rx.try_iter().filter_map(|s| s.project_snapshot).last()
}

/// BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo
/// point), FRAME_EXACT_TRANSPORT_DESIGN P3 test 10: Link plays at 128
/// over a one-point map at 120; when its peers vanish, authority falls to
/// Internal and the map must be untouched (D21).
#[test]
fn peers_vanishing_keeps_the_beat_and_the_map() {
    use manifold_core::types::{ClockAuthority, TempoPointSource};
    use manifold_core::{Beats, Bpm};
    use manifold_playback::link_sync::{LinkSyncController, SimulatedLink};
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.tempo_map.add_or_replace_point(
        Beats::ZERO,
        Bpm(120.0),
        TempoPointSource::Unknown,
        0.001,
    );
    let mut thread = headless_content_thread(project, 64, 64);
    thread.timer.set_frame_clocked(true);
    let (state_tx, _state_rx) = crate::content_state::state_channel();
    let mut link = LinkSyncController::new();
    let session = SimulatedLink {
        at: std::time::Instant::now(),
        beat: 0.0,
        tempo: 128.0,
        peers: 1,
        playing: false,
    };
    link.simulate(session);
    thread.transport_controller.link_sync = Some(link);
    thread.handle_command(ContentCommand::Play);
    for _ in 0..300 {
        thread.tick_frame(&state_tx);
    }
    assert_eq!(
        thread.engine.project().unwrap().settings.clock_authority,
        ClockAuthority::Link
    );

    thread
        .transport_controller
        .link_sync
        .as_mut()
        .unwrap()
        .simulate(SimulatedLink {
            peers: 0,
            ..session
        });
    for _ in 0..60 {
        thread.tick_frame(&state_tx);
    }
    let p = thread.engine.project().unwrap();
    assert_eq!(p.settings.clock_authority, ClockAuthority::Internal);
    assert_eq!(
        p.tempo_map.point_count(),
        1,
        "peers vanishing must not add tempo points"
    );
    assert_eq!(
        p.tempo_map.get_bpm_at_beat_immut(Beats::ZERO, Bpm(0.0)),
        Bpm(120.0),
        "the stale Link tempo must not be written into the beat-0 point"
    );
}

/// BUG-0u1al (Link-initiated Play never caches the Link beat offset),
/// FRAME_EXACT_TRANSPORT_DESIGN P3 test 9: after a Stop, MANIFOLD sits at
/// beat 10 and Link starts playing at its beat 37.3. The offset rounds to
/// Link's quantum, so MANIFOLD's bar lines up with Link's, and Link then
/// steers for a minute within Link's 1 ms bound (D15).
#[test]
fn link_started_play_aligns_to_the_bar_and_steers() {
    use manifold_core::types::{ClockAuthority, TempoPointSource};
    use manifold_core::{Beats, Bpm};
    use manifold_playback::link_sync::{LinkSyncController, SimulatedLink};
    use std::time::Duration;
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.tempo_map.add_or_replace_point(
        Beats::ZERO,
        Bpm(120.0),
        TempoPointSource::Unknown,
        0.001,
    );
    let mut thread = headless_content_thread(project, 64, 64);
    let (state_tx, _state_rx) = crate::content_state::state_channel();
    let step = Duration::from_secs_f64(1.0 / 60.0);
    let anchor = std::time::Instant::now() + step;
    let at = |k: u32| anchor + step * k + step.mul_f64(0.3);
    // Link's beat is 37.3 when it starts playing, 600 frames in.
    let session = SimulatedLink {
        at: anchor,
        beat: 37.3 - 10.0 * 2.0,
        tempo: 120.0,
        peers: 1,
        playing: false,
    };
    let mut link = LinkSyncController::new();
    link.simulate(session);
    thread.transport_controller.link_sync = Some(link);
    thread.handle_command(ContentCommand::Stop);
    thread.handle_command(ContentCommand::SeekToBeat(Beats(10.0)));
    let mut k = 0;
    while k < 600 {
        thread.tick_frame_at(at(k), &state_tx);
        k += 1;
    }
    assert!(!thread.engine.is_playing(), "MANIFOLD waits at beat 10");
    thread
        .transport_controller
        .link_sync
        .as_mut()
        .unwrap()
        .simulate(SimulatedLink {
            playing: true,
            ..session
        });
    let mut worst_ms: f64 = 0.0;
    for _ in 0..3_600 {
        thread.tick_frame_at(at(k), &state_tx);
        k += 1;
        assert_eq!(
            thread.engine.project().unwrap().settings.clock_authority,
            ClockAuthority::Link
        );
        if k < 600 + 300 {
            continue;
        }
        assert!(thread.engine.is_playing(), "Link's play started MANIFOLD");
        let link_beat = session.beat_at(thread.link_sample_at);
        let diff = link_beat - thread.engine.frame_beat().0;
        let folded = (diff + 2.0).rem_euclid(4.0) - 2.0;
        worst_ms = worst_ms.max(folded.abs() * 60.0 / 120.0 * 1000.0);
    }
    assert!(
        worst_ms <= 1.0,
        "MANIFOLD's bar is {worst_ms:.2} ms off Link's (mod 4 beats)"
    );
}

/// BUG-mmct0.5 (MIDI recording commits bypass EditingService): a recorded
/// take commits through the gateway, so the UI receives it and Cmd+Z
/// removes it.
#[test]
fn midi_take_reaches_the_ui_and_undoes() {
    use manifold_core::change::Part;
    use manifold_playback::midi_input::{MidiNoteEvent, MidiNoteEventType};
    let mut thread =
        headless_content_thread(one_layer(manifold_core::types::LayerType::Trigger), 64, 64);
    thread.timer.set_frame_clocked(true);
    let (state_tx, state_rx) = crate::content_state::state_channel();
    thread.handle_command(ContentCommand::SetRecording(true));
    thread.handle_command(ContentCommand::Play);
    let note = |event_type, sequence| MidiNoteEvent {
        event_type,
        note: 60,
        velocity: 1.0,
        channel: 0,
        source_index: 0,
        absolute_tick: -1,
        sequence,
    };
    thread
        .midi_input
        .inject_note(note(MidiNoteEventType::NoteOn, 1));
    for _ in 0..30 {
        thread.tick_frame(&state_tx);
    }
    let _ = last_snapshot(&state_rx);
    let before = thread.editing_service.stamps();
    thread
        .midi_input
        .inject_note(note(MidiNoteEventType::NoteOff, 2));
    thread.tick_frame(&state_tx);
    assert!(
        thread
            .editing_service
            .stamps()
            .moved_since(&before)
            .contains(Part::Clips),
        "the take moves the Clips stamp"
    );
    let snap = last_snapshot(&state_rx).expect("the take publishes");
    assert_eq!(
        snap.timeline.layers[0].clips.len(),
        1,
        "the UI receives the take"
    );

    thread.handle_command(ContentCommand::Undo);
    thread.tick_frame(&state_tx);
    let snap = last_snapshot(&state_rx).expect("the undo publishes");
    assert!(
        snap.timeline.layers[0].clips.is_empty(),
        "Cmd+Z removes the take"
    );
}

/// BUG-7f4d (percussion import outside EditingService): clearing a clip's
/// detected triggers writes through the gateway, so the UI receives it.
#[test]
fn clear_detected_triggers_reaches_the_ui() {
    use manifold_core::change::Part;
    let mut project = one_layer(manifold_core::types::LayerType::Generator);
    let source = manifold_core::ClipId::from("audio-source");
    let mut trigger = manifold_core::clip::TimelineClip::new_trigger(
        manifold_core::Beats(0.0),
        manifold_core::Beats(1.0),
    );
    trigger.detection_source = Some(source.clone());
    trigger.layer_id = project.timeline.layers[0].layer_id.clone();
    project.timeline.layers[0].clips.push(trigger);
    let mut thread = headless_content_thread(project, 64, 64);
    let (state_tx, state_rx) = crate::content_state::state_channel();
    thread.tick_frame(&state_tx);
    let _ = last_snapshot(&state_rx);
    let before = thread.editing_service.stamps();
    thread.handle_command(ContentCommand::ClearClipTriggers(source));
    thread.tick_frame(&state_tx);
    assert!(
        thread
            .editing_service
            .stamps()
            .moved_since(&before)
            .contains(Part::Clips)
    );
    let snap = last_snapshot(&state_rx).expect("the clear publishes");
    assert!(snap.timeline.layers[0].clips.is_empty());
    thread.handle_command(ContentCommand::Undo);
    thread.tick_frame(&state_tx);
    assert_eq!(
        last_snapshot(&state_rx)
            .expect("the undo publishes")
            .timeline
            .layers[0]
            .clips
            .len(),
        1
    );
}

#[test]
fn mutation_publishes_snapshot_without_a_tick() {
    let mut thread = headless_content_thread(Project::default(), 1280, 720);
    let (state_tx, state_rx) = crate::content_state::state_channel();
    assert!(!thread.handle_command(ContentCommand::SetProject));
    thread.publish_snapshot_if_dirty(&state_tx);
    let state = state_rx.try_recv().expect("snapshot published");
    assert!(
        state.data_version > 0,
        "expected data_version to bump, got {}",
        state.data_version
    );
}

#[test]
fn sdr_preview_publishes_runtime_snapshot_without_a_tick() {
    let mut thread = headless_content_thread(Project::default(), 64, 64);
    let (state_tx, state_rx) = crate::content_state::state_channel();
    assert!(!thread.handle_command(ContentCommand::SetSdrPreview(true)));
    thread.publish_snapshot_if_dirty(&state_tx);
    let state = state_rx
        .try_recv()
        .expect("runtime preview snapshot published");
    assert_eq!(state.sdr_preview, Some(true));
    assert_eq!(state.data_version, 0, "preview must not create an edit");
    assert!(
        !state.editing_is_dirty,
        "preview must not dirty the project"
    );
}

/// FRAME_EXACT_TRANSPORT_DESIGN D10: the project setting is the only fps.
/// The engine clock follows it on the next tick and the content timer
/// follows the clock: after a load, a frame-rate edit, its undo and redo.
#[test]
fn timer_and_clock_follow_project_fps() {
    use manifold_editing::commands::settings::ChangeFrameRateCommand;
    let mut thread = headless_content_thread(Project::default(), 64, 64);
    let (state_tx, _state_rx) = crate::content_state::state_channel();
    let tick_and_check = |thread: &mut super::ContentThread, fps: f32, after: &str| {
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !thread.content_pipeline.is_surface_ready(thread.frame_count) {
            assert!(
                std::time::Instant::now() < give_up,
                "GPU surface not ready after 5 s"
            );
            std::thread::yield_now();
        }
        thread.tick_frame_at(std::time::Instant::now(), &state_tx);
        assert_eq!(
            thread.engine.frame_rate(),
            fps,
            "engine clock after {after}"
        );
        assert_eq!(
            thread.timer.target_fps(),
            f64::from(fps),
            "content timer after {after}"
        );
    };
    let mut project = Project::default();
    project.settings.frame_rate = 24.0;
    thread.install_project(Box::new(project));
    tick_and_check(&mut thread, 24.0, "load");
    let edit = ChangeFrameRateCommand::new(24.0, 30.0);
    assert!(!thread.handle_command(ContentCommand::Execute(Box::new(edit))));
    assert!(!thread.handle_command(ContentCommand::SetFrameRate(30.0)));
    tick_and_check(&mut thread, 30.0, "the frame-rate edit");
    assert!(!thread.handle_command(ContentCommand::Undo));
    tick_and_check(&mut thread, 24.0, "undo");
    assert!(!thread.handle_command(ContentCommand::Redo));
    tick_and_check(&mut thread, 30.0, "redo");
}

/// Metal's presents arrive between a scrub's seeks: they feed the screen
/// delay without flushing the coalesced seek, so a burst is one seek.
#[test]
fn presents_inside_a_scrub_burst_leave_one_seek() {
    let mut thread = headless_content_thread(Project::default(), 64, 64);
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let epoch = thread.engine.transport_epoch();
    let now = std::time::Instant::now();
    for (id, seconds) in [(1u64, 1.0), (2, 2.0), (3, 3.0)] {
        cmd_tx
            .send(ContentCommand::SeekTo(manifold_core::Seconds(seconds)))
            .expect("send");
        thread.issued_presents.record(id, now);
        let at = now + std::time::Duration::from_millis(25);
        cmd_tx
            .send(ContentCommand::FramePresented {
                drawable_id: id,
                at,
            })
            .expect("send");
    }
    assert!(!thread.drain_commands(&cmd_rx), "no shutdown");
    assert_eq!(
        thread.engine.transport_epoch(),
        epoch.wrapping_add(1),
        "one seek for the burst"
    );
    assert!(
        (thread.engine.current_time().0 - 3.0).abs() < 1.0 / 60.0,
        "the burst's last seek wins"
    );
    assert!(
        thread.issued_presents.take(1).is_none(),
        "the presents reached the calibration"
    );
}

fn plasma_project(bpm: f32, frame_rate: f32) -> Project {
    let mut project = Project::default();
    project.settings.bpm = manifold_core::Bpm(bpm);
    project.settings.frame_rate = frame_rate;
    let mut layer = manifold_core::layer::Layer::new_generator(
        "Plasma".into(),
        manifold_core::PresetTypeId::PLASMA,
        0,
    );
    layer
        .clips
        .push(manifold_core::clip::TimelineClip::new_generator(
            manifold_core::Beats(0.0),
            manifold_core::Beats(2.0),
        ));
    project.timeline.layers.push(layer);
    project
}

#[cfg(target_os = "macos")]
fn two_beat_export(
    name: &str,
) -> (
    manifold_media::export_config::ExportConfig,
    std::path::PathBuf,
) {
    let path = std::env::temp_dir().join(format!("manifold-{name}-{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let config = manifold_media::export_config::ExportConfig {
        output_path: path.to_string_lossy().into_owned(),
        width: 64,
        height: 64,
        fps: 30.0,
        hdr: false,
        start_beat: 0.0,
        end_beat: 2.0,
        audio_path: None,
        audio_start_beat: 0.0,
        audio_encoder_delay: 0.0,
        split_at_markers: false,
        include_gizmos: false,
        metadata: Vec::new(),
    };
    (config, path)
}

#[cfg(target_os = "macos")]
#[test]
fn an_export_request_runs_at_the_next_frame_and_later_commands_wait() {
    let mut thread = headless_content_thread(plasma_project(137.0, 60.0), 64, 64);
    let (config, path) = two_beat_export("pending-export");
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
    let (state_tx, state_rx) = crate::content_state::state_channel();

    assert!(!thread.handle_command(ContentCommand::StartExport(Box::new(config))));
    assert!(!thread.handle_command(ContentCommand::SetSdrPreview(true)));
    assert!(
        !thread.content_pipeline.sdr_preview(),
        "held behind the export"
    );
    assert!(
        thread
            .held_commands
            .iter()
            .any(|c| matches!(c, ContentCommand::SetSdrPreview(true)))
    );

    assert!(!thread.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx));
    let finished = state_rx
        .try_iter()
        .flat_map(|state| state.export_finished.clone())
        .last();
    let written = path.exists();
    let _ = std::fs::remove_file(&path);
    assert!(
        finished.is_some_and(|event| event.success),
        "export finished"
    );
    assert!(written, "the export wrote its file");
    assert!(
        thread.content_pipeline.sdr_preview(),
        "ran after the export"
    );
    assert!(thread.held_commands.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_quit_behind_a_pending_export_cancels_it_and_still_quits() {
    let mut thread = headless_content_thread(plasma_project(137.0, 60.0), 64, 64);
    let (config, path) = two_beat_export("pending-quit");
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
    let (state_tx, state_rx) = crate::content_state::state_channel();

    assert!(!thread.handle_command(ContentCommand::StartExport(Box::new(config))));
    assert!(
        !thread.handle_command(ContentCommand::Shutdown),
        "the quit is held, not run"
    );
    assert!(thread.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx));
    let finished = state_rx
        .try_iter()
        .flat_map(|state| state.export_finished.clone())
        .last();
    let written = path.exists();
    let _ = std::fs::remove_file(&path);
    assert!(
        finished.is_some_and(|event| !event.success),
        "export cancelled"
    );
    assert!(!written, "no output left");
}

#[test]
fn an_open_project_request_installs_and_warms_at_the_next_frame() {
    let mut thread = headless_content_thread(Project::default(), 64, 64);
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
    let (state_tx, _state_rx) = crate::content_state::state_channel();
    cmd_tx
        .send(ContentCommand::LoadProject(Box::new(plasma_project(
            137.0, 24.0,
        ))))
        .expect("send");
    cmd_tx
        .send(ContentCommand::SetSdrPreview(true))
        .expect("send");

    assert!(!thread.drain_commands(&cmd_rx));
    let before = thread.engine.project().unwrap();
    assert!(
        before.settings.frame_rate != 24.0,
        "old project still loaded"
    );
    assert!(!thread.content_pipeline.sdr_preview());

    assert!(!thread.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx));
    let after = thread.engine.project().unwrap();
    assert_eq!(after.settings.frame_rate, 24.0);
    assert_eq!(after.timeline.layers.len(), 1);
    assert!(thread.content_pipeline.sdr_preview());
}
