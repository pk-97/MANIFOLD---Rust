//! Tempo edits keep the beat and move its seconds, clip lengths stay beat
//! differences, and physics gets no target from an edit.

use super::*;

#[test]
fn manual_tempo_edit_preserves_beat_while_paused_and_playing() {
    for state in [PlaybackState::Paused, PlaybackState::Playing] {
        let mut engine = PlaybackEngine::new(Vec::new());
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            Bpm(120.0),
            TempoPointSource::Manual,
            0.001,
        );
        engine.initialize(project);
        engine.set_state(state);
        engine.locate(Seconds(10.0));
        let old_map = engine.project().unwrap().tempo_map.clone();
        engine
            .project_mut()
            .unwrap()
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(60.0), TempoPointSource::Manual, 0.001);
        engine.reconcile_tempo_edit(&old_map, Bpm(120.0));
        // The frame's beat sits a ten-millionth of a frame past its time (D14).
        assert!(
            (engine.current_beat().0 - 20.0).abs() < 1e-6,
            "{}",
            engine.current_beat()
        );
        assert_eq!(engine.current_time(), Seconds(20.0));
        assert_eq!(engine.transport_frame(), 1200);
        // Half a second later the new tempo holds: 30 frames at 60 BPM.
        if state == PlaybackState::Playing {
            let _ = engine.tick(TickContext {
                frames: 30,
                ..TickContext::default()
            });
        } else {
            engine.locate(Seconds(20.5));
        }
        assert!(
            (engine.current_beat().0 - 20.5).abs() < 1e-6,
            "{}",
            engine.current_beat()
        );
    }
}

/// The edit happens ahead of the playhead, so the frame's position in
/// the old and new maps is the same: the round trip keeps the frame,
/// even though the frame's beat is sampled a hair after its time.
#[test]
fn tempo_edit_after_the_playhead_keeps_the_frame() {
    for frame in [1i64, 37, 600, 4321] {
        let mut engine = PlaybackEngine::new(Vec::new());
        let mut project = Project::default();
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            Bpm(150.0),
            TempoPointSource::Manual,
            0.001,
        );
        engine.initialize(project);
        engine.locate(engine.transport_clock().time_at(frame));
        let old_map = engine.project().unwrap().tempo_map.clone();
        engine
            .project_mut()
            .unwrap()
            .tempo_map
            .add_or_replace_point(Beats(10_000.0), Bpm(90.0), TempoPointSource::Manual, 0.001);
        engine.reconcile_tempo_edit(&old_map, Bpm(120.0));
        assert_eq!(engine.transport_frame(), frame);
    }
}

/// D8: under the follower a clip's duration and loop length are its beat
/// difference at the segment's rate, even for a clip that started long
/// before the segment's anchor; nothing converts its distant start beat
/// through the segment.
#[test]
fn durations_use_beat_differences() {
    let mut engine = PlaybackEngine::new(Vec::new());
    engine.initialize(Project::default());
    engine.set_follow_authority(Some(FollowProfile::FAST));
    engine.play();
    engine.set_external_sample(ExternalSample {
        beat: 200.0,
        bpm: 128.0,
        fresh: true,
        exact: true,
    });
    let result = engine.tick(TickContext {
        frames: 1,
        realtime_now: Seconds::ZERO,
        frame_count: 0,
    });
    engine.reclaim_tick_result(result);
    assert!(
        (engine.frame_beat().0 - 200.0).abs() < 1e-9,
        "seeded at the source's beat"
    );

    let mut clip = TimelineClip::new_trigger(Beats(0.0), Beats(8.0));
    clip.loop_duration_beats = Beats(2.0);
    let duration = engine.get_clip_duration_seconds(&clip).0;
    assert!(
        (duration - 8.0 * 60.0 / 128.0).abs() < 1e-9,
        "duration {duration}"
    );
    let loop_len = f64::from(engine.get_clip_loop_duration_seconds(&clip));
    assert!(
        (loop_len - 2.0 * 60.0 / 128.0).abs() < 1e-6,
        "loop {loop_len}"
    );
}

/// A live hop clock moves with a tempo edit's transport move, so a hop keeps
/// its played time and audio-driven physics reads it in place, up or down.
#[test]
fn a_tempo_edit_moves_live_hop_clocks_with_the_transport() {
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, HopClock, ParameterAudioMod,
    };
    for bpm in [150.0_f32, 100.0] {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            Bpm(120.0),
            manifold_core::types::TempoPointSource::Manual,
            0.001,
        );
        let send = manifold_core::audio_setup::AudioSend::new("Kick");
        let mut layer = manifold_core::layer::Layer::new_generator(
            "Gen".into(),
            manifold_core::PresetTypeId::new("TestGen"),
            0,
        );
        let feature = AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low);
        layer
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(ParameterAudioMod::new(
                "speed".to_owned().into(),
                send.id.clone(),
                feature,
            ));
        project.audio_setup.sends.push(send);
        project.timeline.layers.push(layer);
        let mut engine = PlaybackEngine::new(Vec::new());
        engine.initialize(project);
        engine.play();
        for _ in 0..30 {
            let result = engine.tick(TickContext {
                frames: 1,
                realtime_now: Seconds::ZERO,
                frame_count: 0,
            });
            engine.reclaim_tick_result(result);
        }
        fn hop_clock(engine: &mut PlaybackEngine) -> &mut Option<HopClock> {
            &mut engine.project_mut().unwrap().timeline.layers[0]
                .gen_params_mut()
                .unwrap()
                .audio_mods_mut()[0]
                .hop_timeline
                .clock
        }
        let now = engine.current_time_double();
        *hop_clock(&mut engine) = Some(HopClock::anchored(1, 48_000, 48_000, now));
        let heard = engine.played_time().seconds();
        let old_map = engine.project().unwrap().tempo_map.clone();
        let edited = engine.project_mut().unwrap();
        edited.settings.bpm = Bpm(bpm);
        edited.tempo_map.add_or_replace_point(
            Beats::ZERO,
            Bpm(bpm),
            manifold_core::types::TempoPointSource::Manual,
            0.001,
        );
        engine.reconcile_tempo_edit(&old_map, Bpm(120.0));
        assert_ne!(
            engine.current_time_double(),
            now,
            "{bpm} BPM: the edit moves the transport"
        );
        let mapped = hop_clock(&mut engine).unwrap().time_of(48_000);
        let played =
            manifold_core::PlayedTime::from_transport(Seconds(mapped), engine.tempo_shift())
                .seconds();
        assert!(
            (played - heard).abs() < 1e-9,
            "{bpm} BPM: the hop moved from played {heard} to {played}"
        );
    }
}

/// A tempo edit while playing keeps the beat and moves its seconds; it is
/// no jump, so physics gets no target from it.
#[test]
fn a_tempo_edit_while_playing_gives_physics_no_target() {
    for fps in [30.0_f32, 60.0, 120.0, 144.0] {
        for bpm in [150.0_f32, 100.0] {
            let mut project = Project::default();
            project.settings.frame_rate = fps;
            project.settings.bpm = Bpm(120.0);
            project.tempo_map.add_or_replace_point(
                Beats::ZERO,
                Bpm(120.0),
                manifold_core::types::TempoPointSource::Manual,
                0.001,
            );
            let mut engine = PlaybackEngine::new(Vec::new());
            engine.initialize(project);
            engine.play();
            for _ in 0..30 {
                let result = engine.tick(TickContext {
                    frames: 1,
                    realtime_now: Seconds::ZERO,
                    frame_count: 0,
                });
                engine.reclaim_tick_result(result);
                engine.take_played_target();
            }
            let old_map = engine.project().unwrap().tempo_map.clone();
            let before = engine.current_time_double();
            let played = engine.played_time();
            let edited = engine.project_mut().unwrap();
            edited.settings.bpm = Bpm(bpm);
            edited.tempo_map.add_or_replace_point(
                Beats::ZERO,
                Bpm(bpm),
                manifold_core::types::TempoPointSource::Manual,
                0.001,
            );
            engine.reconcile_tempo_edit(&old_map, Bpm(120.0));
            assert_ne!(
                engine.current_time_double(),
                before,
                "{fps} fps, {bpm} BPM: the edit moves the beat's seconds"
            );
            assert!(
                (engine.played_time().seconds() - played.seconds()).abs() < 1e-9,
                "{fps} fps, {bpm} BPM: the edit moved played time"
            );
            let result = engine.tick(TickContext {
                frames: 1,
                realtime_now: Seconds::ZERO,
                frame_count: 0,
            });
            engine.reclaim_tick_result(result);
            assert_eq!(
                engine.take_played_target(),
                None,
                "{fps} fps, {bpm} BPM: a tempo edit reached physics as a jump"
            );
        }
    }
}
