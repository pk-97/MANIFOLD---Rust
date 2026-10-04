use super::*;
use manifold_core::Beats;

struct Fixture {
    directory: Arc<PathBuf>,
    settings: FluidSettings,
}

impl Fixture {
    fn new(timed: bool) -> Self {
        Self::new_at(timed, 40.0)
    }

    fn new_at(timed: bool, origin: f64) -> Self {
        Self::with_project_tempo(timed, origin, None)
    }

    fn with_project_tempo(
        timed: bool,
        origin: f64,
        project_tempo: Option<crate::preset_context::ProjectTempo>,
    ) -> Self {
        Self::with_sources(timed, origin, project_tempo, None)
    }

    fn with_sources(
        timed: bool,
        origin: f64,
        project_tempo: Option<crate::preset_context::ProjectTempo>,
        source_identity: Option<[u8; 32]>,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = Arc::new(std::env::temp_dir().join(format!(
            "manifold-fluid-project-playback-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        )));
        let settings = FluidSettings {
            resolution: 8,
            ..Default::default()
        };
        let initial = FluidControls::default();
        let request = Request {
            outputs: Default::default(),
            source_identity,
            project_tempo,
            epoch: 1,
            settings,
            initial,
            start_tick: 0,
            start_time: Seconds::ZERO,
            count: 6,
            schedule: None,
            history: [0.0, 6.0 * TICK]
                .map(|time| Sample {
                    time,
                    controls: initial,
                    acceleration_field: None,
                })
                .to_vec(),
            impulses: Vec::new(),
            role_setup: Arc::new(roles::Setup::default()),
            role_history: Vec::new(),
            recycle: Vec::new(),
            recycle_whitewater: WhitewaterFrame::default(),
            cache_mode: CacheMode::Record,
            cache_path: Arc::clone(&directory),
            coupled: None,
            timing: take::TimingHandoff {
                points: if timed {
                    [
                        (origin, 0.0),
                        (origin + 2.0 * TICK, 2.0 * TICK),
                        (origin + 3.0 * TICK, 4.0 * TICK),
                        (origin + 2.0, 4.0 * TICK),
                        (origin + 2.0 + TICK, 6.0 * TICK),
                    ]
                    .map(|(transport, simulation)| TakeTime {
                        beat: Beats(transport * 2.0),
                        transport: Seconds(transport),
                        simulation: Seconds(simulation),
                    })
                    .to_vec()
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
            playback: None,
        };
        let mut cache = CacheWriter::create_for_take(Arc::clone(&directory), settings).unwrap();
        let mut take = take::Writer::create(Arc::clone(&directory), &request).unwrap();
        // Distinct, synthetic cache frames isolate timeline addressing from
        // numerical simulation. Native coupled playback is covered separately.
        for tick in 1..=6 {
            cache
                .append(
                    tick,
                    &[bytemuck::Zeroable::zeroed(); 3],
                    &WhitewaterFrame::default(),
                    Transform {
                        pos: [tick as f32, 0.0, 0.0],
                        ..Default::default()
                    },
                    FrameStats {
                        particles: tick as u32,
                        triangles: 1,
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        take.append(&request, 6, 6, None).unwrap();
        cache.publish_take_prefix(take.identity()).unwrap();
        Self {
            directory,
            settings,
        }
    }

    fn runtime(&self) -> FluidRuntime {
        let mut runtime = FluidRuntime::default();
        runtime
            .set_cache(CacheMode::Playback, self.directory.to_str().unwrap())
            .unwrap();
        runtime
    }

    fn observe(&self, runtime: &mut FluidRuntime, transport: f64, speed: f32) {
        runtime
            .observe_coupled_frame(
                self.settings,
                FluidControls::default(),
                &[],
                None,
                None,
                super::super::FrameTime {
                    seconds: Seconds(transport),
                    beats: Beats(transport * 2.0),
                    delta: Seconds::ZERO,
                    frame_count: 0,
                },
                speed,
                0.0,
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.directory.as_ref()).unwrap();
    }
}

#[test]
fn fluid_playback_resolves_project_origin_speed_changes_holds_and_backward_seeks() {
    let fixture = Fixture::new(true);
    let mut runtime = fixture.runtime();
    let mut epoch = None;
    let mut previous = None;
    for (transport, speed, tick) in [
        (40.0, 1.0, 0),
        (40.0 + TICK, 2.0, 1),
        (40.0 + 2.5 * TICK, 0.0, 3),
        (41.0, 4.0, 4),
        (42.0, 1.0, 4),
        (42.0 + TICK, 2.0, 6),
        (40.0 + TICK, 1.0, 1),
    ] {
        fixture.observe(&mut runtime, transport, speed);
        runtime.advance(true).unwrap();
        if let Some((previous_tick, version)) = previous
            && tick == previous_tick
        {
            assert_eq!(
                runtime.version, version,
                "a recorded hold must reuse the published mesh"
            );
        }
        assert_eq!(runtime.completed_tick, tick);
        assert_eq!(runtime.stats.particles, tick as u32);
        if tick > 0 {
            assert_eq!(runtime.obstacle.pos[0], tick as f32);
        }
        assert_eq!(runtime.stats.simulation_ms, 0.0);
        assert_eq!(runtime.stats.meshing_ms, 0.0);
        assert_eq!(runtime.lag_seconds(), 0.0);
        assert_eq!(*epoch.get_or_insert(runtime.epoch), runtime.epoch);
        let version = runtime.version;
        runtime.advance(true).unwrap();
        assert_eq!(
            runtime.version, version,
            "same project address must finish once"
        );
        previous = Some((tick, version));
    }
}

#[test]
fn fluid_playback_loads_recorded_motion_at_negative_project_time() {
    let fixture = Fixture::new_at(true, -4.0);
    let mut runtime = fixture.runtime();
    fixture.observe(&mut runtime, -3.0, 1.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 4);
    assert_eq!(runtime.stats.particles, 4);
    assert_eq!(runtime.lag_seconds(), 0.0);
}

#[test]
fn fluid_playback_rejects_requests_outside_the_recorded_project_range() {
    let fixture = Fixture::new(true);
    for transport in [39.0, 42.0 + 2.0 * TICK] {
        let mut runtime = fixture.runtime();
        fixture.observe(&mut runtime, transport, 1.0);
        assert!(
            runtime
                .advance(true)
                .unwrap_err()
                .contains("outside the completed project range")
        );
        assert!(runtime.vertices.is_empty());
        assert!(!runtime.initialized);
    }
}

#[test]
fn fluid_playback_untimed_bound_take_preserves_absolute_speed_scaled_address() {
    let fixture = Fixture::new(false);
    let mut runtime = fixture.runtime();
    for (transport, speed, tick) in [(2.0 * TICK, 2.0, 4), (TICK, 3.0, 3)] {
        fixture.observe(&mut runtime, transport, speed);
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, tick);
        assert_eq!(runtime.stats.particles, tick as u32);
        assert_eq!(runtime.lag_seconds(), 0.0);
    }
}

#[test]
fn fluid_playback_old_reply_does_not_acknowledge_a_newer_seek() {
    let fixture = Fixture::new(true);
    let mut runtime = fixture.runtime();
    let (requests, receive) = mpsc::sync_channel(1);
    let (send, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    let mut native = NativeSimulation::default();
    fixture.observe(&mut runtime, 41.0, 1.0);
    runtime.advance(false).unwrap();
    let older = receive.try_recv().unwrap();
    fixture.observe(&mut runtime, 40.0 + TICK, 1.0);
    send.send(native.process(older, &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(runtime.completed_tick, 4);
    let latest = receive.try_recv().unwrap();
    assert_eq!(
        latest.playback.unwrap().address.transport,
        Seconds(40.0 + TICK)
    );
    send.send(native.process(latest, &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 1);
    assert_eq!(runtime.lag_seconds(), 0.0);
    assert!(receive.try_recv().is_err());
}

#[test]
fn fluid_playback_revalidates_current_project_tempo_and_rejects_unproven_timing() {
    use crate::preset_context::ProjectTempo;
    use manifold_core::{Bpm, tempo::TempoMap};
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(120.0));
    let fixture = Fixture::with_project_tempo(true, 40.0, Some(tempo.clone()));
    let mut runtime = fixture.runtime();
    runtime.set_project_tempo(Some(&tempo));
    fixture.observe(&mut runtime, 41.0, 1.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 4);
    let epoch = runtime.epoch;
    runtime.set_project_tempo(Some(&tempo));
    assert_eq!(
        runtime.epoch, epoch,
        "unchanged tempo must reuse the verified reader"
    );
    let changed = ProjectTempo::new(&TempoMap::default(), Bpm(60.0));
    runtime.set_project_tempo(Some(&changed));
    assert!(runtime.epoch > epoch);
    fixture.observe(&mut runtime, 41.0, 1.0);
    assert!(
        runtime
            .advance(true)
            .unwrap_err()
            .to_lowercase()
            .contains("tempo")
    );
    assert!(runtime.vertices.is_empty());
    assert!(!runtime.initialized);

    let unproven = Fixture::new(true);
    let mut runtime = unproven.runtime();
    runtime.set_project_tempo(Some(&tempo));
    unproven.observe(&mut runtime, 41.0, 1.0);
    assert!(
        runtime.advance(true).is_err(),
        "a timed take alone is not authoritative project tempo"
    );
}

#[test]
fn fluid_playback_tempo_edit_cancels_an_already_completed_old_reply() {
    use crate::preset_context::ProjectTempo;
    use manifold_core::{Bpm, tempo::TempoMap};
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(120.0));
    let fixture = Fixture::with_project_tempo(true, 40.0, Some(tempo.clone()));
    let mut runtime = fixture.runtime();
    runtime.set_project_tempo(Some(&tempo));
    let (requests, receive) = mpsc::sync_channel(1);
    let (send, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    let mut native = NativeSimulation::default();
    fixture.observe(&mut runtime, 41.0, 1.0);
    runtime.advance(false).unwrap();
    let old_reply = native.process(receive.try_recv().unwrap(), &runtime.cancel_epoch);
    assert!(old_reply.error.is_none());
    let changed = ProjectTempo::new(&TempoMap::default(), Bpm(60.0));
    runtime.set_project_tempo(Some(&changed));
    fixture.observe(&mut runtime, 41.0, 1.0);
    send.send(old_reply).unwrap();
    runtime.advance(false).unwrap();
    assert!(
        !runtime.initialized,
        "old validated geometry must not cross the tempo edit"
    );
    assert!(runtime.vertices.is_empty());
    let new_request = receive.try_recv().unwrap();
    let reply = native.process(new_request, &runtime.cancel_epoch);
    assert!(reply.error.is_some());
    send.send(reply).unwrap();
    assert!(runtime.advance(true).is_err());
}

#[test]
fn fluid_project_tempo_edits_preserve_live_owner_and_ignore_display_bpm_for_a_map() {
    use crate::preset_context::ProjectTempo;
    use manifold_core::{Bpm, tempo::TempoMap, types::TempoPointSource};
    let mut map = TempoMap::default();
    map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
    map.ensure_sorted();
    let tempo = ProjectTempo::new(&map, Bpm(120.0));
    let display_change = ProjectTempo::new(&map, Bpm(137.0));
    let mut runtime = FluidRuntime::default();
    runtime.set_project_tempo(Some(&tempo));
    runtime
        .observe(
            FluidSettings::default(),
            FluidControls::default(),
            Seconds::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    let epoch = runtime.epoch;
    runtime.set_project_tempo(Some(&ProjectTempo::new(&TempoMap::default(), Bpm(60.0))));
    assert_eq!(
        runtime.epoch, epoch,
        "tempo edits do not reset live physics"
    );
    let fixture = Fixture::with_project_tempo(true, 40.0, Some(tempo.clone()));
    let mut playback = fixture.runtime();
    playback.set_project_tempo(Some(&tempo));
    fixture.observe(&mut playback, 41.0, 1.0);
    playback.advance(true).unwrap();
    let epoch = playback.epoch;
    playback.set_project_tempo(Some(&display_change));
    assert_eq!(
        playback.epoch, epoch,
        "display BPM cannot invalidate a populated tempo map"
    );
    assert!(playback.initialized);
}

#[test]
fn fluid_recording_rejects_losing_project_clock_provenance_mid_take() {
    use crate::preset_context::ProjectTempo;
    use manifold_core::{Bpm, tempo::TempoMap};
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(120.0));
    let mut runtime = FluidRuntime::default();
    runtime.set_project_tempo(Some(&tempo));
    // Observation alone performs no filesystem or native work.
    runtime
        .set_cache(CacheMode::Record, "unused-clock-provenance-test")
        .unwrap();
    let observe = |runtime: &mut FluidRuntime, seconds: f64| {
        runtime.observe_coupled_frame(
            FluidSettings::default(),
            FluidControls::default(),
            &[],
            None,
            None,
            super::super::FrameTime {
                seconds: Seconds(seconds),
                beats: Beats(seconds * 2.0),
                delta: Seconds::ZERO,
                frame_count: 0,
            },
            1.0,
            0.0,
        )
    };
    observe(&mut runtime, 0.0).unwrap();
    runtime.set_project_tempo(None);
    assert!(
        observe(&mut runtime, TICK)
            .unwrap_err()
            .contains("provenance changed")
    );
    assert!(runtime.failure.is_some());
    runtime.clear();
    observe(&mut runtime, 0.0).unwrap();
    assert!(runtime.failure.is_none());
    assert_eq!(runtime.recording_project_timing, Some(false));
}

#[test]
fn fluid_playback_revalidates_authored_sources_and_cancels_completed_old_replies() {
    let fixture = Fixture::with_sources(true, 40.0, None, Some([1; 32]));
    let mut runtime = fixture.runtime();
    runtime.set_source_identity(Ok([1; 32]));
    let (requests, receive) = mpsc::sync_channel(1);
    let (send, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    let mut native = NativeSimulation::default();
    fixture.observe(&mut runtime, 41.0, 1.0);
    runtime.advance(false).unwrap();
    let old_reply = native.process(receive.try_recv().unwrap(), &runtime.cancel_epoch);
    assert!(old_reply.error.is_none());
    let epoch = runtime.epoch;
    runtime.set_source_identity(Ok([1; 32]));
    assert_eq!(runtime.epoch, epoch);
    runtime.set_source_identity(Ok([2; 32]));
    assert!(runtime.epoch > epoch);
    fixture.observe(&mut runtime, 41.0, 1.0);
    send.send(old_reply).unwrap();
    runtime.advance(false).unwrap();
    assert!(!runtime.initialized);
    assert!(runtime.vertices.is_empty());
    let reply = native.process(receive.try_recv().unwrap(), &runtime.cancel_epoch);
    assert!(
        reply
            .error
            .as_deref()
            .unwrap()
            .contains("source identity changed")
    );
    send.send(reply).unwrap();
    assert!(runtime.advance(true).is_err());
}

#[test]
fn fluid_record_commits_source_only_changes_without_native_ticks_or_new_clock_points() {
    let directory =
        std::env::temp_dir().join(format!("manifold-fluid-source-only-{}", std::process::id()));
    let mut runtime = FluidRuntime::default();
    runtime.set_source_identity(Ok([1; 32]));
    runtime
        .set_cache(CacheMode::Record, directory.to_str().unwrap())
        .unwrap();
    runtime
        .observe(
            FluidSettings {
                resolution: 8,
                fill_height: 0.0,
                ..Default::default()
            },
            FluidControls {
                emission: false,
                obstacle_enabled: false,
                ..Default::default()
            },
            Seconds::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    runtime.advance(true).unwrap();
    let epoch = runtime.epoch;
    let version = runtime.version;
    let original = FluidTakeReplay::open(&directory).unwrap();
    original.validate_source_identity([1; 32]).unwrap();
    runtime.set_source_identity(Err("temporarily unresolved authored dependency".into()));
    assert!(runtime.advance(true).is_err());
    assert_eq!(runtime.epoch, epoch);
    assert!(runtime.initialized);
    runtime.set_source_identity(Ok([2; 32]));
    assert_eq!(runtime.epoch, epoch);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 0);
    assert_eq!(runtime.version, version);
    assert_eq!(runtime.committed_source_identity, Some([2; 32]));
    let updated = FluidTakeReplay::open(&directory).unwrap();
    updated.validate_source_identity([2; 32]).unwrap();
    assert!(updated.validate_source_identity([1; 32]).is_err());
    original.validate_source_identity([1; 32]).unwrap();
    drop(runtime);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn fluid_source_preparation_error_survives_reset_and_recovers_explicitly() {
    let mut runtime = FluidRuntime::default();
    let live_epoch = runtime.epoch;
    runtime.set_source_identity(Err("invalid authored dependency".into()));
    runtime.advance(true).unwrap(); // Live does not require cache provenance.
    runtime.set_source_identity(Ok([1; 32]));
    assert_eq!(runtime.epoch, live_epoch);
    runtime.set_source_identity(Err("invalid authored dependency".into()));
    runtime
        .set_cache(CacheMode::Record, "unused-source-error-test")
        .unwrap();
    runtime.clear();
    assert_eq!(runtime.domain_snapshot().state, FluidDomainState::Failed);
    assert!(
        runtime
            .advance(true)
            .unwrap_err()
            .contains("authored dependency")
    );
    runtime.set_source_identity(Ok([3; 32]));
    runtime.advance(true).unwrap();
    assert!(runtime.source_error.is_none());
}

#[test]
fn fluid_source_acknowledgement_preserves_newer_authored_edits() {
    let mut runtime = FluidRuntime::default();
    runtime.set_source_identity(Ok([1; 32]));
    runtime
        .set_cache(CacheMode::Record, "unused-source-ack-test")
        .unwrap();
    runtime
        .observe(
            FluidSettings::default(),
            FluidControls::default(),
            Seconds::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    let (requests, receive) = mpsc::sync_channel(1);
    let (send, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    runtime.advance(false).unwrap();
    send.send(cancelled_reply(receive.try_recv().unwrap()))
        .unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(runtime.committed_source_identity, Some([1; 32]));
    runtime.set_source_identity(Ok([2; 32]));
    runtime.advance(false).unwrap();
    let older = receive.try_recv().unwrap();
    assert!(older.timing.metadata_only);
    runtime.set_source_identity(Ok([3; 32]));
    send.send(cancelled_reply(older)).unwrap();
    runtime.advance(false).unwrap();
    let newer = receive.try_recv().unwrap();
    assert_eq!(newer.source_identity, Some([3; 32]));
    send.send(cancelled_reply(newer)).unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(runtime.committed_source_identity, Some([3; 32]));
    assert!(receive.try_recv().is_err());
}
