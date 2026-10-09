use super::*;

use manifold_node_engine::scene::impulse::ImpulseTarget;
use crate::physics_events::ResolvedNodeImpulse;

const VORTEX_SEQUENCE: u64 = 1;
const OUTER_TICKS: u64 = 12;

#[derive(Clone, Copy)]
struct Endpoint {
    stamp: TickStamp,
    pos: [f32; 3],
    rot_euler: [f32; 3],
    particles: u32,
    surface_vertices: usize,
}

fn vortex_event(
    runtime: &mut FluidRuntime,
    fixture: &(FluidSettings, FluidControls, RigidSceneInputs),
) {
    let position = fixture.2.bodies[0]
        .as_ref()
        .expect("vortex fixture body")
        .transform
        .pos;
    let center = [position[0] + 0.15, position[1], position[2]];
    let field = FieldValue::vortex(center, [0.0, 0.0, 1.0], 0.9, 1.0)
        .expect("valid vortex field")
        .scaled(0.5)
        .expect("valid vortex scale");
    let stamp = runtime
        .impulse_stamp(Seconds(TICK), VORTEX_SEQUENCE)
        .expect("accepted zero-time vortex stamp");
    runtime
        .enqueue_scene_impulse(
            stamp,
            ResolvedNodeImpulse {
                field,
                target: ImpulseTarget::Fluid,
            },
        )
        .expect("liquid-only vortex event admitted");
}

fn finite_frame(frame: &super::super::CoupledRigidFrame) -> bool {
    frame.poses.iter().all(|pose| {
        pose.pos
            .iter()
            .chain(pose.rot_euler.iter())
            .chain(pose.scale.iter())
            .all(|v| v.is_finite())
    })
}

fn run_case(
    viscosity: f64,
    colliders: bool,
    batched: bool,
) -> (Endpoint, super::super::CoupledRigidFrame, usize) {
    let mut fixture = super::fixture();
    fixture.0.liquid.viscosity = viscosity;
    fixture.2.acceleration_field = None;
    let mut runtime = FluidRuntime::default();
    super::observe(&mut runtime, &fixture, 0.0, 0.0, colliders);
    runtime.advance(true).expect("prepare vortex fixture");
    // Initial liquid volumes seed at the end of the first native substep.
    // Start the vortex trace with liquid already surrounding the collider.
    super::observe(&mut runtime, &fixture, TICK, 0.0, colliders);
    runtime.advance(true).expect("seed vortex fixture");
    let initial = runtime
        .coupled_rigid_frame()
        .expect("initial coupled frame")
        .clone();
    assert_eq!(initial.stamp.tick, 1);
    assert!(finite_frame(&initial));
    vortex_event(&mut runtime, &fixture);

    if batched {
        for tick in 1..=OUTER_TICKS {
            super::observe(&mut runtime, &fixture, (tick + 1) as f64 * TICK, 0.0, colliders);
        }
        runtime.advance(true).expect("batched vortex trace");
    } else {
        for tick in 1..=OUTER_TICKS {
            super::observe(&mut runtime, &fixture, (tick + 1) as f64 * TICK, 0.0, colliders);
            runtime.advance(true).expect("vortex trace");
            assert!(finite_frame(
                runtime
                    .coupled_rigid_frame()
                    .expect("accepted coupled frame")
            ));
        }
    }
    let final_frame = runtime
        .coupled_rigid_frame()
        .expect("final coupled frame")
        .clone();
    assert_eq!(final_frame.stamp.tick, OUTER_TICKS + 1);
    assert_eq!(final_frame.stamp.epoch, runtime.epoch);
    assert_eq!(runtime.completed_tick, OUTER_TICKS + 1);
    assert!(finite_frame(&final_frame));
    assert!(runtime.stats.particles > 0);
    assert!(!runtime.vertices.is_empty());
    let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].source.sequence, VORTEX_SEQUENCE);
    assert_eq!(receipts[0].value.target, ImpulseTarget::Fluid);

    let paused = final_frame.clone();
    super::observe(
        &mut runtime,
        &fixture,
        (OUTER_TICKS + 1) as f64 * TICK,
        0.0,
        colliders,
    );
    runtime.advance(true).expect("paused vortex observation");
    let paused_frame = runtime.coupled_rigid_frame().expect("paused coupled frame");
    assert_eq!(paused_frame.stamp, paused.stamp);
    assert_eq!(paused_frame.poses, paused.poses);
    assert_eq!(runtime.drain_scene_impulses().count(), 0);

    let pose = &final_frame.poses[0];
    let endpoint = Endpoint {
        stamp: final_frame.stamp,
        pos: pose.pos,
        rot_euler: pose.rot_euler,
        particles: runtime.stats.particles,
        surface_vertices: runtime.vertices.len(),
    };
    (endpoint, final_frame, receipts.len())
}

#[test]
fn fluid_coupled_liquid_vortex_transfers_positive_roll_only_through_selected_colliders() {
    for viscosity in [0.0, 1.0] {
        let (coupled, _, _) = run_case(viscosity, true, false);
        let (uncoupled, _, _) = run_case(viscosity, false, false);
        eprintln!(
            "vortex viscosity={viscosity}: coupled pos={:?} rot={:?}; uncoupled pos={:?} rot={:?}",
            coupled.pos, coupled.rot_euler, uncoupled.pos, uncoupled.rot_euler
        );
        assert_eq!(coupled.stamp.tick, OUTER_TICKS + 1);
        assert!(coupled.stamp.epoch > 0);
        assert!(coupled.particles > 0 && coupled.surface_vertices > 0);
        assert!(uncoupled.particles > 0 && uncoupled.surface_vertices > 0);
        assert!(coupled.rot_euler[2] > 1.0e-4);
        assert!(coupled.rot_euler[2] < 0.5);
        assert!(uncoupled.rot_euler[2].abs() < 1.0e-6);
    }
}

#[test]
fn fluid_coupled_liquid_vortex_historical_batch_matches_regular_trace() {
    let (regular, regular_frame, regular_receipts) = run_case(1.0, true, false);
    let (batched, batched_frame, batched_receipts) = run_case(1.0, true, true);
    eprintln!(
        "vortex viscosity=1 historical: regular pos={:?} rot={:?}; batched pos={:?} rot={:?}",
        regular.pos, regular.rot_euler, batched.pos, batched.rot_euler
    );
    assert_eq!(regular_receipts, 1);
    assert_eq!(batched_receipts, 1);
    assert_eq!(regular.stamp, batched.stamp);
    assert_eq!(regular_frame.stamp, batched_frame.stamp);
    for (actual, expected) in batched.pos.iter().zip(regular.pos) {
        assert!(
            (actual - expected).abs() < 1.0e-5,
            "batched={actual}, regular={expected}"
        );
    }
    for (actual, expected) in batched.rot_euler.iter().zip(regular.rot_euler) {
        assert!(
            (actual - expected).abs() < 1.0e-5,
            "batched={actual}, regular={expected}"
        );
    }
}
