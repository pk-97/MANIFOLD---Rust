use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::Seconds;
use manifold_physics::{FieldValue, TickStamp, input::EventStamp};

use super::*;
use crate::node_graph::fluid::{CoupledRigidInputs, FluidRuntime, Transform, Worker};
use crate::node_graph::fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry};
use crate::node_graph::physics::{
    ColliderGeometry, RigidBody, RigidImpulseTargets, RigidSceneInputs,
};
use crate::node_graph::physics_events::ImpulseTarget;

struct Directory(Arc<PathBuf>);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "manifold-physics-take-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(Arc::new(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(self.0.as_ref()).unwrap();
    }
}

/// Capture the real runtime handoff, including sealed impulse assignments and
/// aligned role/rigid histories. Only the native worker is replaced by a channel.
pub(in crate::node_graph::fluid) fn request() -> Request {
    let settings = FluidSettings {
        resolution: 12,
        fill_height: 0.0,
        initial_volume: Some(Transform {
            pos: [0.0, 0.9, 0.0],
            scale: [1.6; 3],
            ..Default::default()
        }),
        ..Default::default()
    };
    let controls = FluidControls {
        emission: false,
        obstacle_enabled: false,
        gravity: [0.0; 3],
        ..Default::default()
    };
    let points = vec![
        [-0.2, -0.2, -0.2],
        [0.2, -0.2, -0.2],
        [-0.2, 0.2, -0.2],
        [-0.2, -0.2, 0.2],
    ];
    let mut rigid = RigidSceneInputs::default();
    rigid.bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: [0.0, 0.8, 0.0],
            ..Default::default()
        },
        mass: 30.0,
        collider: Some(Arc::new(ColliderGeometry {
            hulls: vec![points.clone()],
        })),
        ..Default::default()
    });
    let mut role = FluidRole {
        geometry: Arc::new(PreparedFluidGeometry {
            meshes: vec![manifold_physics::cook_hull_mesh(&points).unwrap()],
        }),
        kind: FluidRoleKind::Collider,
        transform: Transform {
            pos: [0.65, 0.8, 0.0],
            ..Default::default()
        },
        enabled: true,
        velocity: [0.0; 3],
        inherit_motion: 0.0,
        friction: 0.2,
    };
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let (_reply, replies) = std::sync::mpsc::sync_channel(1);
    let mut runtime = FluidRuntime::default();
    runtime.worker = Some(Worker {
        requests: send,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    for tick in 0..=6 {
        role.transform.pos[0] += 0.001;
        rigid.acceleration_field =
            Some(FieldValue::uniform([tick as f32 * 0.02, 0.0, 0.0]).unwrap());
        runtime
            .observe_coupled_scene_with_field(
                settings,
                controls,
                &[Some(role.clone())],
                Some(FieldValue::uniform([0.0, 0.0, tick as f32 * 0.1]).unwrap()),
                Some(CoupledRigidInputs {
                    scene: &rigid,
                    colliders: RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                    density: 1000.0,
                }),
                Seconds(tick as f64 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        if tick == 0 {
            runtime.advance(false).unwrap();
            let initial = receive.recv().unwrap();
            let reply = super::super::native::NativeSimulation::default()
                .process(initial, &runtime.cancel_epoch);
            runtime.accept(reply).unwrap();
        }
        if tick == 1 || tick == 4 {
            runtime
                .enqueue_scene_impulse(
                    EventStamp {
                        epoch: runtime.epoch,
                        time: Seconds(tick as f64 * TICK),
                        sequence: tick,
                    },
                    ResolvedNodeImpulse {
                        field: FieldValue::uniform([0.1, 0.0, 0.0]).unwrap(),
                        target: ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
                            bodies: 1,
                            copies: false,
                        }),
                    },
                )
                .unwrap();
        }
    }
    runtime.advance(false).unwrap();
    let request = receive.recv().unwrap();
    assert_eq!(request.count, 6);
    request
}

#[test]
fn fluid_take_replays_native_coupling_roles_fields_and_events() {
    let directory = Directory::new();
    let mut input = request();
    input.settings.seed = 0x3141_5926_5358_9793;
    input.cache_mode = CacheMode::Record;
    input.cache_path = Arc::clone(&directory.0);
    let expected =
        super::super::native::NativeSimulation::default().process(input, &AtomicU64::new(1));
    assert_eq!(expected.error, None);
    assert_eq!(expected.tick, 6);
    assert!(expected.stats.particles > 0);
    assert!(!expected.vertices.is_empty());
    let reader = Reader::open(Arc::clone(&directory.0)).unwrap();
    assert_eq!(reader.header.settings.seed, 0x3141_5926_5358_9793);
    let mut replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert_eq!(replay.recorded_tick(), 6);
    assert_eq!(replay.recording_failure(), None);
    let mut receipts = Vec::new();
    let mut ticks = Vec::new();
    while replay.advance().unwrap() {
        let frame = replay.frame().unwrap();
        ticks.push(frame.tick);
        receipts.extend(frame.impulses.iter().map(|event| {
            (
                event.source,
                event.applied,
                event.lateness,
                event.value.clone(),
            )
        }));
    }
    assert_eq!(ticks, [4, 6]);
    let actual = replay.frame().unwrap();
    assert_eq!(actual.stats.particles, expected.stats.particles);
    assert_eq!(actual.surface.len(), expected.vertices.len());
    for (actual, expected) in actual.surface.iter().zip(&expected.vertices) {
        for (a, b) in actual.position.into_iter().zip(expected.position) {
            assert!((a - b).abs() < 1e-5);
        }
    }
    let rigid = actual.rigid.unwrap();
    assert_eq!(rigid.stamp, TickStamp { epoch: 1, tick: 6 });
    let expected_rigid = &expected.coupled.as_ref().unwrap().output;
    for (a, b) in rigid.poses[0]
        .pos
        .into_iter()
        .chain(rigid.poses[0].rot_euler)
        .zip(
            expected_rigid.poses[0]
                .pos
                .into_iter()
                .chain(expected_rigid.poses[0].rot_euler),
        )
    {
        assert!((a - b).abs() < 1e-5, "{a} != {b}");
    }
    let expected_receipts: Vec<_> = expected
        .impulses
        .iter()
        .map(|event| {
            (
                event.source,
                event.applied,
                event.lateness,
                event.value.clone(),
            )
        })
        .collect();
    assert_eq!(receipts, expected_receipts);
}

#[test]
fn fluid_take_restores_shared_geometry_and_exact_handoff_values() {
    let directory = Directory::new();
    let original = request();
    let mut writer = Writer::create(Arc::clone(&directory.0), &original).unwrap();
    writer.append(&original, 6, 6, None).unwrap();
    let (disk, _): (Batch, _) = read_record(&batch_path(&directory.0, 0)).unwrap();
    assert!(
        disk.coupled_history
            .as_ref()
            .unwrap()
            .iter()
            .all(|sample| sample.inputs.bodies[0].as_ref().unwrap().collider.is_none())
    );
    let mut reader = Reader::open(Arc::clone(&directory.0)).unwrap();
    let restored = reader.next_request(19).unwrap().unwrap();
    assert_eq!(restored.settings, original.settings);
    assert_eq!(restored.initial, original.initial);
    assert_eq!(restored.role_history, original.role_history);
    for (a, b) in restored.history.iter().zip(&original.history) {
        assert_eq!(a.time.to_bits(), b.time.to_bits());
        assert_eq!(a.controls, b.controls);
        assert_eq!(a.acceleration_field, b.acceleration_field);
    }
    let rigid = restored.coupled.as_ref().unwrap();
    let header_geometry = rigid.setup.initial.bodies[0]
        .as_ref()
        .unwrap()
        .collider
        .as_ref()
        .unwrap();
    for (a, b) in rigid
        .history
        .iter()
        .zip(&original.coupled.as_ref().unwrap().history)
    {
        assert_eq!(a.inputs, b.inputs);
        assert!(Arc::ptr_eq(
            header_geometry,
            a.inputs.bodies[0]
                .as_ref()
                .unwrap()
                .collider
                .as_ref()
                .unwrap()
        ));
    }
    for (a, b) in restored.impulses.iter().zip(&original.impulses) {
        assert_eq!(a.source.epoch, 19);
        assert_eq!(a.applied.epoch, 19);
        assert_eq!(a.applied.tick, b.applied.tick);
        assert_eq!(a.source.time, b.source.time);
        assert_eq!(a.source.sequence, b.source.sequence);
        assert_eq!(a.lateness, b.lateness);
        assert_eq!(a.value, b.value);
    }
    assert!(reader.next_request(19).unwrap().is_none());
}

#[test]
fn fluid_take_failure_preserves_only_completed_prefix() {
    for completed in [0, 2] {
        let directory = Directory::new();
        let input = request();
        let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
        writer
            .append(
                &input,
                completed,
                completed as u64 + 1,
                Some("fixture failure"),
            )
            .unwrap();
        assert!(writer.append(&input, 0, 0, None).is_err());
        let mut replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
        assert_eq!(replay.recorded_tick(), completed as u64);
        assert_eq!(replay.recording_failure(), Some("fixture failure"));
        if completed > 0 {
            assert!(replay.advance().unwrap());
            let frame = replay.frame().unwrap();
            assert_eq!(frame.tick, completed as u64);
            assert!(
                frame
                    .impulses
                    .iter()
                    .all(|event| event.applied.tick < completed as u64)
            );
        }
        assert!(!replay.advance().unwrap());
    }
}

#[test]
fn fluid_take_missing_or_corrupt_batch_latches_replay_failure() {
    for corrupt in [false, true] {
        let directory = Directory::new();
        let input = request();
        let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
        writer.append(&input, 6, 6, None).unwrap();
        let mut replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
        let path = batch_path(&directory.0, 0);
        if corrupt {
            fs::write(&path, b"damaged").unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let first = replay.advance().unwrap_err();
        assert_eq!(replay.advance().unwrap_err(), first);
        assert!(replay.frame().is_none());
    }
}

#[test]
fn fluid_take_rejects_invalid_roles_event_order_and_rigid_layout() {
    for case in 0..5 {
        let directory = Directory::new();
        let mut input = request();
        let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
        match case {
            0 => {
                input.role_history.pop();
            }
            1 => input.impulses.swap(0, 1),
            2 => {
                input.coupled.as_mut().unwrap().history[0].inputs.bodies[0]
                    .as_mut()
                    .unwrap()
                    .fragment_parent = Some(64)
            }
            3 => input.impulses[0].source.epoch += 1,
            4 => {
                let mut value = serde_json::to_value(input.role_history[0]).unwrap();
                value["transform"]["scale"][0] = serde_json::json!(0.0);
                input.role_history[0] = serde_json::from_value(value).unwrap();
            }
            _ => unreachable!(),
        }
        writer.append(&input, 6, 6, None).unwrap();
        let mut reader = Reader::open(Arc::clone(&directory.0)).unwrap();
        assert!(reader.next_request(1).is_err(), "case {case}");
    }
}

#[test]
fn fluid_take_setup_failure_and_incompatible_solver_are_explicit() {
    let directory = Directory::new();
    let mut input = request();
    input.count = 0;
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer
        .append(&input, 0, 0, Some("native setup failed"))
        .unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert_eq!(replay.recording_failure(), Some("native setup failed"));
    assert_eq!(replay.recorded_tick(), 0);
    let path = directory.0.join(HEADER);
    let (mut header, _): (Header, _) = read_record(&path).unwrap();
    header.numerics_revision += 1;
    fs::remove_file(&path).unwrap();
    write_new(&path, &header).unwrap();
    assert!(
        FluidTakeReplay::open(directory.0.as_ref())
            .err()
            .unwrap()
            .contains("incompatible")
    );
}

#[test]
fn fluid_take_rejects_changed_sources_with_an_intact_header_chain() {
    let directory = Directory::new();
    let mut input = request();
    input.count = 0;
    let _writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    let path = directory.0.join(HEADER);
    let (mut header, _): (Header, _) = read_record(&path).unwrap();
    header.solver_identity[0] ^= 1;
    fs::remove_file(&path).unwrap();
    let hash = write_new(&path, &header).unwrap();
    let (mut progress, _): (Progress, _) = read_record(&directory.0.join(PROGRESS)).unwrap();
    progress.header_hash = hash;
    progress.last_batch_hash = hash;
    publish_progress(&directory.0, &progress).unwrap();
    assert!(
        FluidTakeReplay::open(directory.0.as_ref())
            .err()
            .unwrap()
            .contains("incompatible solver")
    );
}

#[test]
fn fluid_take_validates_entire_hash_chain_before_replay_and_geometry_identity() {
    let directory = Directory::new();
    let mut input = request();
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    input.count = 3;
    input.impulses.clear();
    writer.append(&input, 3, 3, None).unwrap();
    input.start_tick = 3;
    writer.append(&input, 3, 6, None).unwrap();
    // A valid compressed record with changed input must fail its chain, even
    // though the altered first record has a valid zstd checksum and header ID.
    let path = batch_path(&directory.0, 0);
    let (mut first, _): (Batch, _) = read_record(&path).unwrap();
    first.history[0].controls.gravity[0] = 1.0;
    fs::remove_file(&path).unwrap();
    write_new(&path, &first).unwrap();
    assert!(
        FluidTakeReplay::open(directory.0.as_ref())
            .err()
            .unwrap()
            .contains("chain")
    );
    let header_path = directory.0.join(HEADER);
    let (mut header, old_hash): (Header, _) = read_record(&header_path).unwrap();
    let mut rigid = header.coupled_setup.take().unwrap();
    let collider = Arc::get_mut(&mut rigid).unwrap().initial.bodies[0]
        .as_mut()
        .unwrap()
        .collider
        .as_mut()
        .unwrap();
    Arc::get_mut(collider).unwrap().hulls[0][0][0] += 0.01;
    header.coupled_setup = Some(rigid);
    fs::remove_file(&header_path).unwrap();
    let new_hash = write_new(&header_path, &header).unwrap();
    assert_ne!(old_hash, new_hash);
    assert!(
        FluidTakeReplay::open(directory.0.as_ref())
            .err()
            .unwrap()
            .contains("incompatible")
    );
}

#[test]
fn fluid_take_fixed_slot_arrays_reject_wrong_lengths() {
    let original = serde_json::to_value(RigidSceneInputs::default()).unwrap();
    for key in ["bodies", "targetedFields"] {
        for add in [false, true] {
            let mut invalid = original.clone();
            let slots = invalid[key].as_array_mut().unwrap();
            if add {
                slots.push(serde_json::Value::Null);
            } else {
                slots.pop();
            }
            assert!(serde_json::from_value::<RigidSceneInputs>(invalid).is_err());
        }
    }
}

fn clock_point(beat: f64, transport: f64, simulation: f64) -> TakeTime {
    TakeTime {
        beat: manifold_core::Beats(beat),
        transport: Seconds(transport),
        simulation: Seconds(simulation),
    }
}

#[test]
fn fluid_take_project_clock_maps_origin_speed_holds_and_tempo() {
    let directory = Directory::new();
    let mut input = request();
    let points = vec![
        clock_point(-4.0, -2.0, 0.0),
        clock_point(-3.0, -1.98, TICK),
        clock_point(-1.0, -1.96, 3.0 * TICK),
        clock_point(3.0, -0.5, 3.0 * TICK),
        clock_point(3.0, 0.0, 3.0 * TICK),
        clock_point(4.0, 0.1, 6.0 * TICK),
    ];
    input.timing.points = points.clone();
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert_eq!(
        replay.project_range(),
        Some(TakeRange {
            start: points[0],
            end: points[5]
        })
    );
    for point in &points {
        let at_seconds = replay.simulation_time_at(point.transport).unwrap();
        let at_beats = replay.simulation_time_at_beat(point.beat).unwrap();
        assert!((at_seconds.0 - point.simulation.0).abs() < 1e-12);
        assert!((at_beats.0 - point.simulation.0).abs() < 1e-12);
    }
    assert_eq!(
        replay.simulation_time_at(Seconds(-0.25)).unwrap(),
        Seconds(3.0 * TICK)
    );
    assert!(
        (replay
            .simulation_time_at_beat(manifold_core::Beats(-2.0))
            .unwrap()
            .0
            - 2.0 * TICK)
            .abs()
            < 1e-12
    );
    for value in [-2.01, 0.11, f64::NAN] {
        assert!(replay.simulation_time_at(Seconds(value)).is_err());
    }
}

#[test]
fn fluid_take_project_clock_clips_failed_native_prefix() {
    let directory = Directory::new();
    let mut input = request();
    input.timing.points = vec![
        clock_point(8.0, 2.0, 0.0),
        clock_point(14.0, 3.0, 6.0 * TICK),
    ];
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer
        .append(&input, 2, 3, Some("native fixture failure"))
        .unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    let range = replay.project_range().unwrap();
    assert!((range.end.beat.0 - 10.0).abs() < 1e-12);
    assert!((range.end.transport.0 - (2.0 + 1.0 / 3.0)).abs() < 1e-12);
    assert!((range.end.simulation.0 - 2.0 * TICK).abs() < 1e-12);
    assert!((replay.simulation_time_at(range.end.transport).unwrap().0 - 2.0 * TICK).abs() < 1e-12);
    assert!(replay.simulation_time_at(Seconds(2.5)).is_err());
    assert!(
        replay
            .simulation_time_at_beat(manifold_core::Beats(11.0))
            .is_err()
    );
}

#[test]
fn fluid_take_project_clock_rejects_gaps_invalid_points_and_untimed_lookup() {
    let directory = Directory::new();
    let mut input = request();
    let origin = clock_point(2.0, 1.0, 0.0);
    input.timing.points = vec![origin];
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    input.timing.points = vec![clock_point(3.0, 2.0, TICK)];
    assert!(
        writer
            .append(&input, 6, 6, None)
            .unwrap_err()
            .contains("anchor")
    );
    input.timing.points = vec![origin, clock_point(1.0, 2.0, TICK)];
    assert!(
        writer
            .append(&input, 6, 6, None)
            .unwrap_err()
            .contains("beats")
    );
    input.timing.points = vec![origin, clock_point(3.0, 2.0, f64::INFINITY)];
    assert!(writer.append(&input, 6, 6, None).is_err());
    let untimed = Directory::new();
    input.timing.points.clear();
    let mut writer = Writer::create(Arc::clone(&untimed.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();
    let replay = FluidTakeReplay::open(untimed.0.as_ref()).unwrap();
    assert_eq!(replay.project_range(), None);
    assert!(
        replay
            .simulation_time_at(Seconds(0.0))
            .unwrap_err()
            .contains("no project timing")
    );
}

#[test]
fn fluid_take_project_clock_runtime_flushes_holds_without_publishing_frames() {
    let directory = Directory::new();
    let settings = FluidSettings {
        resolution: 12,
        initial_volume: Some(Transform {
            pos: [0.0, 0.9, 0.0],
            scale: [1.6; 3],
            ..Default::default()
        }),
        ..Default::default()
    };
    let controls = FluidControls {
        emission: false,
        ..Default::default()
    };
    let mut runtime = FluidRuntime::default();
    runtime
        .set_cache(CacheMode::Record, directory.0.to_str().unwrap())
        .unwrap();
    let observe = |runtime: &mut FluidRuntime, beats, seconds, speed| {
        runtime
            .observe_coupled_frame(
                settings,
                controls,
                &[],
                None,
                None,
                crate::node_graph::FrameTime {
                    beats: manifold_core::Beats(beats),
                    seconds: Seconds(seconds),
                    delta: Seconds(0.0),
                    frame_count: 0,
                },
                speed,
                0.0,
            )
            .unwrap();
        runtime.advance(true).unwrap();
    };
    observe(&mut runtime, -2.0, -1.0, 1.0);
    observe(&mut runtime, -1.0, -1.0 + 1.25 * TICK, 1.0);
    assert_eq!(runtime.completed_tick, 1);
    assert!(!runtime.vertices.is_empty());
    let version = runtime.version;
    let stats = runtime.stats;
    let vertices = bytemuck::cast_slice::<_, u8>(&runtime.vertices).to_vec();
    for frame in 0..6 {
        observe(&mut runtime, frame as f64, frame as f64, 0.0);
        assert_eq!(runtime.completed_tick, 1);
        assert_eq!(runtime.version, version);
        assert_eq!(runtime.stats, stats);
        assert_eq!(bytemuck::cast_slice::<_, u8>(&runtime.vertices), vertices);
        assert!(!runtime.timing.pending());
    }
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    assert_eq!(replay.project_range().unwrap().end.transport, Seconds(5.0));
    assert!((replay.simulation_time_at(Seconds(3.0)).unwrap().0 - 1.25 * TICK).abs() < 1e-12);
    observe(&mut runtime, 6.0, 5.0 + TICK, 2.0);
    assert_eq!(runtime.completed_tick, 3);
    let mut replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    let mut ticks = Vec::new();
    while replay.advance().unwrap() {
        ticks.push(replay.frame().unwrap().tick);
    }
    assert_eq!(ticks, [1, 3]);
    drop(runtime);
}

#[test]
fn fluid_take_project_clock_rejects_timing_changed_after_open() {
    let directory = Directory::new();
    let mut input = request();
    input.timing.points = vec![
        clock_point(0.0, 0.0, 0.0),
        clock_point(6.0, 1.0, 6.0 * TICK),
    ];
    let mut writer = Writer::create(Arc::clone(&directory.0), &input).unwrap();
    writer.append(&input, 6, 6, None).unwrap();
    let replay = FluidTakeReplay::open(directory.0.as_ref()).unwrap();
    let path = batch_path(&directory.0, 0);
    let (mut batch, _): (Batch, _) = read_record(&path).unwrap();
    batch.clock[1].beat.0 = 7.0;
    fs::remove_file(&path).unwrap();
    write_new(&path, &batch).unwrap();
    // Even a lookup at the unchanged origin must verify the complete chain.
    assert!(
        replay
            .simulation_time_at(Seconds(0.0))
            .unwrap_err()
            .contains("timing changed")
    );
    assert!(FluidTakeReplay::open(directory.0.as_ref()).is_err());
}
