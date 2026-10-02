use super::*;
use manifold_physics::{TickStamp, input::EventStamp};

fn bodies() -> [Option<RigidBody>; MAX_BODIES] {
    let mut bodies = std::array::from_fn(|_| None);
    bodies[0] = Some(RigidBody::default());
    bodies
}

fn advance<C: StepCoupling>(
    simulation: &mut RigidSimulation,
    bodies: [Option<RigidBody>; MAX_BODIES],
    time: f64,
    coupling: &mut C,
) -> Result<(), String> {
    let mut targeted: [Option<FieldValue>; TARGET_SLOTS] = std::array::from_fn(|_| None);
    targeted[0] = Some(FieldValue::uniform([0.0, 0.0, 4.0]).unwrap());
    simulation.advance_with_coupling(
        bodies,
        None,
        0.0,
        1.25,
        16.0,
        0.0,
        [0.0, -2.0, 0.0],
        Seconds(time),
        1.0,
        0.0,
        Some(FieldValue::uniform([3.0, 0.0, 0.0]).unwrap()),
        &targeted,
        coupling,
    )
}

struct Probe {
    body: BodyHandle,
    stamps: Vec<TickStamp>,
    steps: usize,
    duration: f64,
    finished: usize,
    fail_finish: bool,
    invalid_duration: Option<f64>,
}

struct ProbeFrame<'a>(&'a mut Probe);

impl StepCoupling for Probe {
    type Error = &'static str;
    type Frame<'a> = ProbeFrame<'a>;

    fn begin_tick(&mut self, stamp: TickStamp, _: Seconds) -> Result<Self::Frame<'_>, Self::Error> {
        self.stamps.push(stamp);
        Ok(ProbeFrame(self))
    }
}

impl SubstepExchange for ProbeFrame<'_> {
    type Error = &'static str;

    fn next_substep(
        &mut self,
        rigid: &PhysicsWorld,
        maximum: Seconds,
    ) -> Result<Seconds, Self::Error> {
        // The real native queue must contain the same global and targeted
        // forces before EVERY offer, because Box3D clears them after stepping.
        // Box3D stores a force, so the acceleration round-trips through the
        // body's mass and is exact only to float rounding.
        let queued = rigid
            .dynamics(self.0.body)
            .unwrap()
            .external_linear_acceleration;
        for (got, want) in queued.into_iter().zip([3.0, -2.0, 4.0]) {
            assert!((got - want).abs() < 1e-5, "queued {queued:?}");
        }
        Ok(Seconds(
            self.0
                .invalid_duration
                .unwrap_or_else(|| maximum.0.min(FIXED_TICK.0 / 4.0)),
        ))
    }

    fn exchange(&mut self, _: &mut PhysicsWorld, duration: Seconds) -> Result<(), Self::Error> {
        self.0.steps += 1;
        self.0.duration += duration.0;
        Ok(())
    }

    fn finish(self, _: &PhysicsWorld) -> Result<(), Self::Error> {
        if self.0.fail_finish {
            return Err("fixture finish failure");
        }
        self.0.finished += 1;
        Ok(())
    }
}

#[derive(Debug)]
struct CapturePublication {
    stamp: TickStamp,
    body_position: [f32; 3],
    copy_positions: Vec<[f32; 3]>,
}

struct CaptureCompanion {
    body: BodyHandle,
    copies: Vec<BodyHandle>,
    publications: Vec<CapturePublication>,
    fail_finish: bool,
}

struct CaptureFrame<'a> {
    companion: &'a mut CaptureCompanion,
    stamp: TickStamp,
}

impl StepCoupling for CaptureCompanion {
    type Error = &'static str;
    type Frame<'a> = CaptureFrame<'a>;

    fn begin_tick(&mut self, stamp: TickStamp, _: Seconds) -> Result<Self::Frame<'_>, Self::Error> {
        Ok(CaptureFrame {
            companion: self,
            stamp,
        })
    }
}

impl SubstepExchange for CaptureFrame<'_> {
    type Error = &'static str;

    fn next_substep(&mut self, _: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, Self::Error> {
        Ok(Seconds(maximum.0.min(FIXED_TICK.0 / 4.0)))
    }

    fn exchange(&mut self, _: &mut PhysicsWorld, _: Seconds) -> Result<(), Self::Error> {
        Ok(())
    }

    fn finish(self, rigid: &PhysicsWorld) -> Result<(), Self::Error> {
        if self.companion.fail_finish {
            return Err("capture finish failure");
        }
        let body_position = rigid
            .pose(self.companion.body)
            .map_err(|_| "capture body pose read failure")?
            .position;
        let copy_positions = self
            .companion
            .copies
            .iter()
            .map(|&handle| {
                rigid
                    .pose(handle)
                    .map(|pose| pose.position)
                    .map_err(|_| "capture copy pose read failure")
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.companion.publications.push(CapturePublication {
            stamp: self.stamp,
            body_position,
            copy_positions,
        });
        Ok(())
    }
}

fn advance_capture<C: StepCoupling>(
    simulation: &mut RigidSimulation,
    bodies: [Option<RigidBody>; MAX_BODIES],
    prototype: Option<RigidBody>,
    time: f64,
    speed: f32,
    coupling: &mut C,
) -> Result<(), String> {
    simulation.advance_with_coupling(
        bodies,
        prototype,
        2.0,
        1.25,
        2.0,
        0.0,
        [0.0; 3],
        Seconds(time),
        speed,
        0.0,
        None,
        &[],
        coupling,
    )
}

fn fixture() -> (RigidSimulation, Probe) {
    let mut simulation = RigidSimulation::default();
    advance(&mut simulation, bodies(), 0.0, &mut Uncoupled).unwrap();
    let probe = Probe {
        body: simulation.handles[0].unwrap(),
        stamps: Vec::new(),
        steps: 0,
        duration: 0.0,
        finished: 0,
        fail_finish: false,
        invalid_duration: None,
    };
    let epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            EventStamp {
                epoch,
                time: Seconds::ZERO,
                sequence: 1,
            },
            ResolvedRigidImpulse {
                field: FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                targets: RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            },
        )
        .unwrap();
    (simulation, probe)
}

#[test]
fn scene_physics_coupled_substeps_reuse_forces_and_consume_edge_once() {
    let (mut simulation, mut probe) = fixture();
    advance(&mut simulation, bodies(), FIXED_TICK.0 * 2.0, &mut probe).unwrap();
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .dynamics(probe.body)
        .unwrap()
        .linear_velocity;
    let expected = [
        2.0 + 3.0 * FIXED_TICK.0 * 2.0,
        -2.0 * FIXED_TICK.0 * 2.0,
        4.0 * FIXED_TICK.0 * 2.0,
    ];
    for (actual, expected) in velocity.into_iter().zip(expected) {
        assert!((f64::from(actual) - expected).abs() < 2e-6);
    }
    assert!(probe.steps >= 8);
    assert!((probe.duration - FIXED_TICK.0 * 2.0).abs() < 1e-15);
    assert_eq!(probe.finished, 2);
    assert_eq!(
        probe
            .stamps
            .iter()
            .map(|stamp| stamp.tick)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(
        probe
            .stamps
            .iter()
            .all(|stamp| Some(stamp.epoch) == simulation.impulse_epoch())
    );
    assert_eq!(simulation.impulse_receipts.len(), 1);
    assert_eq!(simulation.physics_time, FIXED_TICK.0 * 2.0);
}

#[test]
fn scene_physics_coupling_failure_retains_published_pose_and_latches() {
    let (mut simulation, mut probe) = fixture();
    let published = simulation.poses;
    probe.fail_finish = true;
    let error = advance(&mut simulation, bodies(), FIXED_TICK.0, &mut probe).unwrap_err();
    assert!(error.contains("fixture finish failure"));
    assert_eq!(simulation.poses, published);
    assert_eq!(simulation.physics_time, 0.0);
    assert_eq!(simulation.impulse_receipts.len(), 1);
    let steps = probe.steps;
    assert_eq!(
        advance(&mut simulation, bodies(), FIXED_TICK.0 * 2.0, &mut probe).unwrap_err(),
        error
    );
    assert_eq!(probe.steps, steps);
    assert_eq!(simulation.impulse_receipts.len(), 1);
}

#[test]
fn scene_physics_coupling_capture_publishes_native_pose_before_deferred_edit() {
    let _scope = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
    let mut bodies = std::array::from_fn(|_| None);
    let animated = RigidBody {
        kind: 2,
        transform: Transform {
            pos: [-2.0, 0.0, 0.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    };
    bodies[0] = Some(animated.clone());
    let mut prototype = animated.clone();

    let mut simulation = RigidSimulation::default();
    let mut uncoupled = Uncoupled;
    advance_capture(
        &mut simulation,
        bodies.clone(),
        Some(prototype.clone()),
        0.0,
        1.0,
        &mut uncoupled,
    )
    .unwrap();
    let body = simulation.handles[0].unwrap();
    let copies: Vec<_> = simulation.copy_handles[..simulation.active_copy_count]
        .iter()
        .copied()
        .flatten()
        .collect();
    let copy_offsets: Vec<_> = copies
        .iter()
        .map(|&handle| {
            simulation
                .world
                .as_ref()
                .unwrap()
                .pose(handle)
                .unwrap()
                .position[0]
                + 2.0
        })
        .collect();
    assert_eq!(copies.len(), 2);
    let mut capture = CaptureCompanion {
        body,
        copies,
        publications: Vec::new(),
        fail_finish: false,
    };

    bodies[0].as_mut().unwrap().transform.pos[0] = -1.0;
    prototype.transform.pos[0] = -1.0;
    advance_capture(
        &mut simulation,
        bodies.clone(),
        Some(prototype.clone()),
        FIXED_TICK.0 * 2.0,
        1.0,
        &mut capture,
    )
    .unwrap();
    assert_eq!(capture.publications.len(), 1);
    assert_eq!(capture.publications[0].stamp.tick, 0);
    assert_eq!(simulation.pending_time, FIXED_TICK);

    bodies[0].as_mut().unwrap().transform.pos[0] = 5.0;
    prototype.transform.pos[0] = 5.0;
    advance_capture(
        &mut simulation,
        bodies.clone(),
        Some(prototype.clone()),
        FIXED_TICK.0 * 2.0,
        0.0,
        &mut capture,
    )
    .unwrap();
    assert_eq!(capture.publications.len(), 1);
    assert!((simulation.poses[0].pos[0] - 5.0).abs() < 1.0e-5);
    for (pose, offset) in simulation.copy_poses.iter().zip(&copy_offsets) {
        assert!((pose.pos[0] - (5.0 + offset)).abs() < 1.0e-5);
    }
    assert_eq!(simulation.physics_time, FIXED_TICK.0);
    assert_eq!(simulation.pending_time, FIXED_TICK);

    // The last owed tick samples the left side of the edit boundary. Its
    // native publication must remain historical even while the preview pose
    // above shows the authored teleport.
    advance_capture(
        &mut simulation,
        bodies.clone(),
        Some(prototype.clone()),
        FIXED_TICK.0 * 2.0,
        1.0,
        &mut capture,
    )
    .unwrap();
    assert_eq!(capture.publications.len(), 2);
    assert_eq!(capture.publications[1].stamp.tick, 1);
    assert!((capture.publications[1].body_position[0] + 1.0).abs() < 1.0e-5);
    assert_eq!(
        capture.publications[1].copy_positions.len(),
        copy_offsets.len()
    );
    for (pose, offset) in capture.publications[1]
        .copy_positions
        .iter()
        .zip(&copy_offsets)
    {
        assert!((pose[0] - (-1.0 + offset)).abs() < 1.0e-5);
    }
    assert_eq!(simulation.physics_time, FIXED_TICK.0 * 2.0);
    assert_eq!(simulation.pending_time, Seconds::ZERO);

    // The next accepted tick begins after the deferred edit has been applied,
    // so both the ordinary body and every animated copy publish the new pose.
    advance_capture(
        &mut simulation,
        bodies,
        Some(prototype),
        FIXED_TICK.0 * 3.0,
        1.0,
        &mut capture,
    )
    .unwrap();
    assert_eq!(capture.publications.len(), 3);
    assert_eq!(capture.publications[2].stamp.tick, 2);
    assert!((capture.publications[2].body_position[0] - 5.0).abs() < 1.0e-5);
    for (captured, expected) in capture.publications[2]
        .copy_positions
        .iter()
        .zip(&simulation.copy_poses[..simulation.active_copy_count])
    {
        for (&actual, expected) in captured.iter().zip(expected.pos) {
            assert!((actual - expected).abs() < 1.0e-5);
        }
    }
    assert_eq!(simulation.physics_time, FIXED_TICK.0 * 3.0);
}

#[test]
fn scene_physics_coupling_capture_failure_does_not_publish() {
    let mut bodies = std::array::from_fn(|_| None);
    bodies[0] = Some(RigidBody::default());
    let mut simulation = RigidSimulation::default();
    let mut uncoupled = Uncoupled;
    advance_capture(
        &mut simulation,
        bodies.clone(),
        None,
        0.0,
        1.0,
        &mut uncoupled,
    )
    .unwrap();
    let mut capture = CaptureCompanion {
        body: simulation.handles[0].unwrap(),
        copies: Vec::new(),
        publications: Vec::new(),
        fail_finish: false,
    };
    simulation
        .world
        .as_mut()
        .unwrap()
        .set_velocity(capture.body, [1.0, 0.0, 0.0], [0.0; 3])
        .unwrap();
    advance_capture(
        &mut simulation,
        bodies.clone(),
        None,
        FIXED_TICK.0,
        1.0,
        &mut capture,
    )
    .unwrap();
    let published = simulation.poses;
    let captured = capture.publications[0].body_position;
    capture.fail_finish = true;
    let error = advance_capture(
        &mut simulation,
        bodies,
        None,
        FIXED_TICK.0 * 2.0,
        1.0,
        &mut capture,
    )
    .unwrap_err();
    assert!(error.contains("capture finish failure"));
    assert_eq!(capture.publications.len(), 1);
    assert_eq!(capture.publications[0].body_position, captured);
    assert_eq!(simulation.poses, published);
    assert_ne!(
        simulation
            .world
            .as_ref()
            .unwrap()
            .pose(capture.body)
            .unwrap()
            .position,
        captured
    );
    assert_eq!(simulation.physics_time, FIXED_TICK.0);
}

#[test]
fn scene_physics_coupling_rejects_invalid_offers_before_native_step() {
    for duration in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        FIXED_TICK.0 * 2.0,
        f64::MIN_POSITIVE,
    ] {
        let (mut simulation, mut probe) = fixture();
        let before = simulation.world.as_ref().unwrap().pose(probe.body).unwrap();
        probe.invalid_duration = Some(duration);
        let error = advance(&mut simulation, bodies(), FIXED_TICK.0, &mut probe).unwrap_err();
        assert!(error.contains("invalid substep"), "{error}");
        assert_eq!(probe.steps, 0);
        assert_eq!(
            simulation.world.as_ref().unwrap().pose(probe.body).unwrap(),
            before
        );
        assert_eq!(simulation.physics_time, 0.0);
    }
}

struct FluidCompanion {
    fluid: manifold_fluids::FluidWorld,
    coupling: manifold_fluids::RigidFluidCoupling,
    epoch: u64,
    stamps: Vec<TickStamp>,
}

impl StepCoupling for FluidCompanion {
    type Error = manifold_fluids::FluidError;
    type Frame<'a> = manifold_fluids::CoupledFluidFrame<'a, 'a>;

    fn begin_tick(
        &mut self,
        stamp: TickStamp,
        duration: Seconds,
    ) -> Result<Self::Frame<'_>, Self::Error> {
        assert_eq!(stamp.epoch, self.epoch);
        assert_eq!(stamp.tick, self.stamps.len() as u64);
        self.stamps.push(stamp);
        self.coupling.begin_frame(&mut self.fluid, duration, &[])
    }
}

fn fluid_trace(times: &[f64], origin: [f32; 3]) -> (Transform, [f32; 3]) {
    use manifold_fluids::{
        Bounds, Config, FluidWorld, LiquidOptions, MeshRole, RigidFluidCoupling, TimeStepOptions,
    };
    let vertices: Vec<_> = [-0.25, 0.25]
        .into_iter()
        .flat_map(|x| {
            [-0.2, 0.2]
                .into_iter()
                .flat_map(move |y| [-0.225, 0.225].into_iter().map(move |z| [x, y, z]))
        })
        .collect();
    let mut bodies = bodies();
    bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: std::array::from_fn(|axis| origin[axis] + [1.2, 1.05, 1.2][axis]),
            ..Transform::default()
        },
        // 90 kg in the 0.5 × 0.4 × 0.45 m box.
        density: 1000.0,
        bounce: 0.0,
        collider: Some(Arc::new(ColliderGeometry {
            hulls: vec![vertices],
        })),
        ..RigidBody::default()
    });
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
        .unwrap();
    let handle = simulation.handles[0].unwrap();
    let world = simulation.world.as_ref().unwrap();
    let meshes = world.hull_meshes(handle).unwrap();
    assert_eq!(meshes.len(), 1);
    let mut fluid = FluidWorld::new(Config {
        cells: [16; 3],
        cell_size: 0.15,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid
        .set_liquid_options(LiquidOptions {
            viscosity: 0.1,
            surface_tension: 0.0,
        })
        .unwrap();
    fluid
        .set_time_step_options(TimeStepOptions {
            min_substeps: 2,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        })
        .unwrap();
    let mut pose = world.pose(handle).unwrap();
    pose.position = std::array::from_fn(|axis| pose.position[axis] - origin[axis]);
    let collider = fluid
        .add_mesh(&meshes[0], MeshRole::Collider, pose)
        .unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.45; 3],
                max: [1.95, 1.65, 1.95],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.step(FIXED_TICK).unwrap();
    let coupling =
        RigidFluidCoupling::prepare(&mut fluid, world, &[(collider, handle)], origin, 1000.0)
            .unwrap();
    let epoch = simulation.impulse_epoch().unwrap();
    let mut companion = FluidCompanion {
        fluid,
        coupling,
        epoch,
        stamps: Vec::new(),
    };
    simulation
        .enqueue_impulse(
            EventStamp {
                epoch,
                time: Seconds::ZERO,
                sequence: 1,
            },
            ResolvedRigidImpulse {
                field: FieldValue::uniform([1.0, 0.0, 0.0]).unwrap(),
                targets: RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            },
        )
        .unwrap();
    for &time in times {
        simulation
            .advance_with_coupling(
                bodies.clone(),
                None,
                0.0,
                1.25,
                16.0,
                0.0,
                [0.0; 3],
                Seconds(time),
                1.0,
                0.0,
                None,
                &[],
                &mut companion,
            )
            .unwrap();
    }
    assert_eq!(companion.stamps.len(), 3);
    assert_eq!(simulation.impulse_receipts.len(), 1);
    assert!((simulation.physics_time - FIXED_TICK.0 * 3.0).abs() < 1e-15);
    let stats = companion.coupling.last_stats().unwrap();
    assert!(stats.particles > 0 && stats.substeps >= 2);
    let completed: Vec<_> = companion.coupling.completed_bodies().unwrap().collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].0, handle);
    assert_eq!(
        completed[0].1.pose,
        simulation.world.as_ref().unwrap().pose(handle).unwrap()
    );
    assert_eq!(
        completed[0].1.dynamics,
        simulation.world.as_ref().unwrap().dynamics(handle).unwrap()
    );
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .dynamics(handle)
        .unwrap()
        .linear_velocity;
    assert!(
        velocity[0] > 0.0 && velocity[0] < 0.95,
        "liquid reaction: {velocity:?}"
    );
    (simulation.poses[0], velocity)
}

#[test]
fn scene_physics_real_fluid_exchange_reuses_rigid_clock_and_survives_display_stall() {
    let origin = [-4.0, 2.0, 7.0];
    let regular = fluid_trace(
        &[FIXED_TICK.0, FIXED_TICK.0 * 2.0, FIXED_TICK.0 * 3.0],
        origin,
    );
    let stalled = fluid_trace(&[FIXED_TICK.0 * 3.0], origin);
    for axis in 0..3 {
        assert!((regular.0.pos[axis] - stalled.0.pos[axis]).abs() < 1e-4);
        assert!((regular.1[axis] - stalled.1[axis]).abs() < 1e-3);
    }
}
