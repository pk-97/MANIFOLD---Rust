use super::*;

const GRAVITY: f32 = -9.81;

fn neutral_tank(cells: u32) -> (FluidWorld, PhysicsWorld, BodyHandle) {
    let mut mesh = proxy();
    for vertex in &mut mesh.vertices {
        vertex[0] *= 0.3 / 0.25;
        vertex[2] *= 0.3 / 0.225;
    }
    let mut rigid = PhysicsWorld::new([0.0, GRAVITY, 0.0]).unwrap();
    let body = rigid
        .add_hull(
            &mesh.vertices,
            BodyConfig {
                position: [1.2, 0.9, 1.2],
                mass: 1000.0 * 0.6 * 0.4 * 0.6,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = FluidWorld::new(Config {
        cells: [cells; 3],
        cell_size: 2.4 / f64::from(cells),
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid
        .set_time_step_options(TimeStepOptions {
            min_substeps: 2,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        })
        .unwrap();
    fluid
        .set_liquid_options(LiquidOptions {
            viscosity: 0.0,
            surface_tension: 0.0,
        })
        .unwrap();
    let collider = fluid
        .add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap())
        .unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.0; 3],
                max: [2.4, 1.5, 2.4],
            },
            [0.0; 3],
        )
        .unwrap();
    // Generate the initial particles without gravitational startup motion.
    fluid.step(DT).unwrap();
    fluid.set_gravity([0.0, GRAVITY, 0.0]).unwrap();
    fluid.prepare_rigid_coupling(&[collider], 1000.0).unwrap();
    (fluid, rigid, body)
}

#[derive(Debug)]
struct Motion {
    drift: f32,
    max_drift: f32,
    velocity: f32,
    response_error: f64,
}

fn run_neutral(fps: usize, predict: bool) -> Motion {
    // Eight cells across the shortest proxy dimension. The four-cell case is
    // measured separately below: its native mesh buoyancy misses by 12.6%.
    let (mut fluid, mut rigid, body) = neutral_tank(48);
    let initial = rigid.pose(body).unwrap().position[1];
    let frame_dt = Seconds(1.0 / fps as f64);
    let mut max_drift = 0.0_f32;
    let mut response_error = 0.0_f64;
    for frame_index in 0..fps {
        let mut frame = fluid.begin_frame(frame_dt).unwrap();
        let mut elapsed = 0.0;
        while elapsed < frame_dt.0 - 1e-12 {
            let before = rigid.dynamics(body).unwrap();
            let mut input = before;
            if !predict {
                // Control: old velocity alone omits the pending gravity term.
                input.external_linear_acceleration = [0.0; 3];
                input.external_angular_acceleration = [0.0; 3];
            }
            frame
                .set_rigid_bodies(&[RigidBodyState {
                    pose: rigid.pose(body).unwrap(),
                    dynamics: input,
                }])
                .unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap_or_else(|error| {
                panic!(
                    "neutral fps={fps} predict={predict} frame={frame_index} elapsed={elapsed} dt={dt:?} pose={:?} dynamics={before:?}: {error}",
                    rigid.pose(body).unwrap()
                )
            });
            let reaction = frame.rigid_reactions().unwrap()[0];
            rigid
                .apply_impulses(&[reaction.body_impulse(body).unwrap()])
                .unwrap();
            rigid.step(dt, 1).unwrap();
            let after = rigid.dynamics(body).unwrap();
            for axis in 0..3 {
                let expected = f64::from(before.linear_velocity[axis])
                    + dt.0 * f64::from(before.external_linear_acceleration[axis])
                    + reaction.delta_linear[axis];
                response_error =
                    response_error.max((f64::from(after.linear_velocity[axis]) - expected).abs());
            }
            let y = rigid.pose(body).unwrap().position[1];
            assert!(y.is_finite() && after.linear_velocity.iter().all(|v| v.is_finite()));
            max_drift = max_drift.max((y - initial).abs());
            elapsed += dt.0;
        }
        frame.finish().unwrap();
    }
    Motion {
        drift: rigid.pose(body).unwrap().position[1] - initial,
        max_drift,
        velocity: rigid.dynamics(body).unwrap().linear_velocity[1],
        response_error,
    }
}

#[test]
fn production_coupling_hydrostatic_mesh_resolution_converges() {
    let expected_force = 1000.0 * 0.6 * 0.4 * 0.6 * f64::from(-GRAVITY);
    let mut errors = Vec::new();
    for cells in [24, 48] {
        let (mut fluid, rigid, body) = neutral_tank(cells);
        let mut dynamics = rigid.dynamics(body).unwrap();
        dynamics.kind = BodyKind::Animated;
        let state = RigidBodyState {
            pose: rigid.pose(body).unwrap(),
            dynamics,
        };
        let mut frame = fluid.begin_frame(DT).unwrap();
        let mut vertical_impulse = 0.0;
        for _ in 0..2 {
            frame.set_rigid_bodies(&[state]).unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap();
            vertical_impulse += frame.rigid_reactions().unwrap()[0].linear[1];
        }
        frame.finish().unwrap();
        let force = vertical_impulse / DT.0;
        let relative_error = (force / expected_force - 1.0).abs();
        println!(
            "hydrostatic cells={cells} force={force} expected={expected_force} error={relative_error}"
        );
        errors.push(relative_error);
    }
    // The finer mesh must meet the predeclared 2.5% hydrostatic error limit.
    // Do not compensate coarse geometry error by changing the authored mass.
    assert!(errors[1] < 0.025, "fine hydrostatic error: {}", errors[1]);
    assert!(errors[1] <= errors[0] + 1e-4);
}

#[test]
fn production_coupling_neutral_body_uses_queued_gravity_prediction() {
    let coarse = run_neutral(60, true);
    let fine = run_neutral(120, true);
    let previous_velocity = run_neutral(60, false);
    println!("neutral predicted 60Hz: {coarse:?}");
    println!("neutral predicted 120Hz: {fine:?}");
    println!("neutral old-velocity control: {previous_velocity:?}");
    for result in [&coarse, &fine] {
        assert!(result.max_drift < 0.05, "neutral drift: {result:?}");
        assert!(result.velocity.abs() < 0.1, "neutral speed: {result:?}");
        assert!(
            result.response_error < 1e-5,
            "force counted once: {result:?}"
        );
    }
    assert!((coarse.drift - fine.drift).abs() < 0.02);
    assert!(previous_velocity.max_drift > 2.0 * coarse.max_drift);
}
