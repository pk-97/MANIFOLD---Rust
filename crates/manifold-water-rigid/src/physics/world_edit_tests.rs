//! Edits made while a show plays: a moved Fixed body and a body added to a
//! running world reach the engine without a rebuild.

use super::*;

const DT: f64 = 1.0 / 60.0;
const G: [f32; 3] = [0.0, -9.8, 0.0];

fn live() -> SimStep {
    SimStep::live(FIXED_TICK)
}

fn slab(x: f32) -> RigidBody {
    RigidBody {
        transform: Transform { pos: [x, 0.0, 0.0], scale: [4.0, 1.0, 4.0], ..Transform::default() },
        kind: 0,
        mass: 0.0,
        ..RigidBody::default()
    }
}

fn crate_at(pos: [f32; 3]) -> RigidBody {
    RigidBody { transform: Transform { pos, scale: [0.5; 3], ..Transform::default() }, ..RigidBody::default() }
}

fn scene(bodies: &[(usize, RigidBody)]) -> Vec<Option<RigidBody>> {
    let mut out = vec![None; 64];
    for (slot, body) in bodies {
        out[*slot] = Some(body.clone());
    }
    out
}

/// One display frame as the runtime drives a replay: the requested samples
/// first, then the frame itself.
fn replayed(simulation: &mut RigidSimulation, from: f64, until: f64, bodies: &[Option<RigidBody>]) {
    let mut requested = Vec::new();
    simulation.request_samples(PlayedTime::from_played(from), PlayedTime::from_played(until), &mut requested);
    for time in requested.iter().map(|time| time.seconds()).filter(|&time| time < until).chain([until]) {
        simulation.set_step(live().authored_sample());
        simulation.advance(bodies, G, PlayedTime::from_played(time), 1.0, 0.0).unwrap();
    }
    simulation.set_step(live());
    simulation.advance(bodies, G, PlayedTime::from_played(until), 1.0, 0.0).unwrap();
}

fn plain(simulation: &mut RigidSimulation, _from: f64, until: f64, bodies: &[Option<RigidBody>]) {
    simulation.set_step(live());
    simulation.advance(bodies, G, PlayedTime::from_played(until), 1.0, 0.0).unwrap();
}

type Drive = fn(&mut RigidSimulation, f64, f64, &[Option<RigidBody>]);

fn engine_pose(simulation: &RigidSimulation, slot: usize) -> [f32; 3] {
    let handle = simulation.handles[slot].expect("body installed");
    simulation.world.as_ref().unwrap().pose(handle).unwrap().position
}

fn frames(simulation: &mut RigidSimulation, drive: Drive, from: usize, to: usize, bodies: &[Option<RigidBody>]) {
    for frame in from..to {
        drive(simulation, frame as f64 * DT, (frame + 1) as f64 * DT, bodies);
    }
}

fn start(drive: Drive, bodies: &[Option<RigidBody>]) -> RigidSimulation {
    let mut simulation = RigidSimulation::default().with_step(live());
    simulation.advance(bodies, G, PlayedTime::ZERO, 1.0, 0.0).unwrap();
    frames(&mut simulation, drive, 0, 6, bodies);
    simulation
}

fn fixed_pose_edit_reaches_the_engine_within_one_tick(drive: Drive) {
    let before = scene(&[(0, slab(0.0)), (1, crate_at([0.0, 6.0, 0.0]))]);
    let mut simulation = start(drive, &before);
    let epoch = simulation.impulse_epoch();
    let falling = engine_pose(&simulation, 1);
    let after = scene(&[(0, slab(7.0)), (1, crate_at([0.0, 6.0, 0.0]))]);
    frames(&mut simulation, drive, 6, 8, &after);
    assert!((engine_pose(&simulation, 0)[0] - 7.0).abs() < 1e-4, "the engine slab moved: {:?}", engine_pose(&simulation, 0));
    assert_eq!(simulation.impulse_epoch(), epoch, "no rebuild");
    let resumed = engine_pose(&simulation, 1);
    assert!(resumed[1] < falling[1] && resumed[1] > falling[1] - 0.2, "the falling crate keeps falling from where it was: {falling:?} {resumed:?}");
}

#[test]
fn a_fixed_pose_edit_reaches_the_engine_body_in_a_plain_frame() {
    fixed_pose_edit_reaches_the_engine_within_one_tick(plain);
}

#[test]
fn a_fixed_pose_edit_reaches_the_engine_body_under_a_replay() {
    fixed_pose_edit_reaches_the_engine_within_one_tick(replayed);
}

fn moved_slab_catches_a_falling_crate(drive: Drive) {
    let before = scene(&[(0, slab(0.0)), (1, crate_at([9.0, 4.0, 0.0]))]);
    let mut simulation = start(drive, &before);
    let after = scene(&[(0, slab(9.0)), (1, crate_at([9.0, 4.0, 0.0]))]);
    frames(&mut simulation, drive, 6, 240, &after);
    let crate_y = engine_pose(&simulation, 1)[1];
    assert!(crate_y > 0.4, "the crate rests on the moved slab instead of falling through it: y {crate_y}");
}

#[test]
fn a_moved_fixed_slab_catches_a_falling_crate() {
    moved_slab_catches_a_falling_crate(plain);
    moved_slab_catches_a_falling_crate(replayed);
}

fn a_body_added_mid_play_leaves_the_others_alone(drive: Drive) {
    let alone = scene(&[(0, slab(0.0)), (1, crate_at([0.0, 6.0, 0.0]))]);
    let with_new = scene(&[(0, slab(0.0)), (1, crate_at([0.0, 6.0, 0.0])), (2, crate_at([2.0, 3.0, 0.0]))]);
    let mut control = start(drive, &alone);
    let mut simulation = start(drive, &alone);
    let epoch = simulation.impulse_epoch();
    frames(&mut control, drive, 6, 30, &alone);
    frames(&mut simulation, drive, 6, 30, &with_new);
    assert_eq!(simulation.impulse_epoch(), epoch, "no rebuild");
    let (kept, expected) = (engine_pose(&simulation, 1), engine_pose(&control, 1));
    assert!((kept[1] - expected[1]).abs() < 1e-4, "the falling crate is not reset: {kept:?} vs {expected:?}");
    assert!(engine_pose(&simulation, 2)[1] < 3.0, "the new crate is in the world and falling");
    assert!(engine_pose(&simulation, 2)[0] > 1.9, "the new crate started at its authored place");
}

#[test]
fn a_body_added_to_an_empty_slot_leaves_the_others_alone() {
    a_body_added_mid_play_leaves_the_others_alone(plain);
    a_body_added_mid_play_leaves_the_others_alone(replayed);
}

#[test]
fn a_disabled_body_enabled_mid_play_leaves_the_others_alone() {
    for drive in [plain as Drive, replayed as Drive] {
        let mut off = crate_at([2.0, 3.0, 0.0]);
        off.enabled = false;
        let alone = scene(&[(0, slab(0.0)), (1, crate_at([0.0, 6.0, 0.0])), (2, off)]);
        let with_new = scene(&[(0, slab(0.0)), (1, crate_at([0.0, 6.0, 0.0])), (2, crate_at([2.0, 3.0, 0.0]))]);
        let mut control = start(drive, &alone);
        let mut simulation = start(drive, &alone);
        let epoch = simulation.impulse_epoch();
        frames(&mut control, drive, 6, 30, &alone);
        frames(&mut simulation, drive, 6, 30, &with_new);
        assert_eq!(simulation.impulse_epoch(), epoch, "no rebuild");
        assert!((engine_pose(&simulation, 1)[1] - engine_pose(&control, 1)[1]).abs() < 1e-4);
        assert!(engine_pose(&simulation, 2)[1] < 3.0);
    }
}

#[test]
fn a_fixed_pose_edit_reaches_the_engine_in_a_worker_interval() {
    use manifold_physics::stepping::StepInterval;
    let mut inputs = RigidSceneInputs::default();
    inputs.bodies[0] = Some(slab(0.0));
    inputs.bodies[1] = Some(crate_at([0.0, 6.0, 0.0]));
    inputs.gravity = G;
    let mut simulation = RigidSimulation::with_worker_epoch(1).unwrap();
    let mut coupling = Uncoupled;
    simulation.advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling).unwrap();
    let tick = FIXED_TICK.0;
    for ordinal in 0..6 {
        let interval = StepInterval::new(Seconds(ordinal as f64 * tick), Seconds((ordinal + 1) as f64 * tick));
        simulation.advance_worker_interval(&inputs, interval, &[], &mut coupling).unwrap();
    }
    inputs.bodies[0] = Some(slab(7.0));
    let interval = StepInterval::new(Seconds(6.0 * tick), Seconds(7.0 * tick));
    simulation.advance_worker_interval(&inputs, interval, &[], &mut coupling).unwrap();
    assert!((engine_pose(&simulation, 0)[0] - 7.0).abs() < 1e-4, "{:?}", engine_pose(&simulation, 0));
}
#[test]
fn a_card_mass_is_the_engine_mass_whatever_the_size() {
    for scale in [[0.5; 3], [2.0; 3]] {
        let body = RigidBody { mass: 250.0, transform: Transform { scale, ..Transform::default() }, ..RigidBody::default() };
        let mut world = PhysicsWorld::new(G).unwrap();
        let (handle, volume) = add_body_geometry(&mut world, &body).unwrap();
        let reported = 1.0 / world.dynamics(handle).unwrap().inverse_mass;
        assert!((reported - 250.0).abs() < 0.5, "scale {scale:?} volume {volume}: {reported} kg");
    }
}
