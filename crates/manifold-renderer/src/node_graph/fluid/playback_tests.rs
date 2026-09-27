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
            epoch: 1,
            settings,
            initial,
            start_tick: 0,
            count: 6,
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
