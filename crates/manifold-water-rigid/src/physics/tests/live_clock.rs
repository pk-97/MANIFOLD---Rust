//! The live clock under replay: tick caps, export parity, history drain,
//! and the knots a replay-driven world records at a new grid origin.

use super::*;

/// A replay-driven frame as the runtime drives it: the requested samples
/// and the closing sample at the frame carry the held gravity, the frame
/// its own.
fn replayed_frame(
    simulation: &mut RigidSimulation,
    from: f64,
    until: f64,
    held: [f32; 3],
    own: [f32; 3],
) {
    replayed_frame_at(simulation, live_step(), from, until, held, own);
}

/// [`replayed_frame`] on `step`'s Sim Rate.
fn replayed_frame_at(
    simulation: &mut RigidSimulation,
    step: SimStep,
    from: f64,
    until: f64,
    held: [f32; 3],
    own: [f32; 3],
) {
    let mut played = Vec::new();
    simulation.request_samples(
        PlayedTime::from_played(from),
        PlayedTime::from_played(until),
        &mut played,
    );
    let requests: Vec<f64> = played.into_iter().map(PlayedTime::seconds).collect();
    for &time in requests
        .iter()
        .filter(|&&time| time < until)
        .chain([&until])
    {
        simulation.set_step(step.authored_sample());
        simulation
            .advance(
                &one_body([0.0, 4.0, 0.0]),
                held,
                PlayedTime::from_played(time),
                1.0,
                0.0,
            )
            .unwrap();
    }
    simulation.set_step(step);
    simulation
        .advance(
            &one_body([0.0, 4.0, 0.0]),
            own,
            PlayedTime::from_played(until),
            1.0,
            0.0,
        )
        .unwrap();
}

#[test]
fn an_overload_reanchor_records_the_frame_knot_at_the_new_origin() {
    let calm = [0.0, -9.8, 0.0];
    let pushed = [3.0, -9.8, 0.0];
    let mut simulation = RigidSimulation::default().with_step(live_step());
    simulation
        .advance(&one_body([0.0, 4.0, 0.0]), calm, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    replayed_frame(&mut simulation, 0.0, FRAME, calm, calm);
    // Six intervals late, past the two-interval live cap.
    let late = FRAME + 0.1;
    replayed_frame(&mut simulation, FRAME, late, calm, pushed);
    let origin = simulation.clock.tick_start(simulation.clock.ticks_done());
    assert_eq!(
        origin,
        Some(PlayedTime::from_played(late)),
        "the late frame reanchors the grid at itself"
    );
    let last = simulation.authored_samples.back().expect("knots recorded");
    assert_eq!(
        last.time, simulation.authored_time,
        "a knot sits at the new origin"
    );
    assert_eq!(
        last.gravity, pushed,
        "the tick at the new origin starts from the frame's inputs"
    );
}

/// Play from a cue lands its first frame one frame past the cue. The
/// world restarts at the cue and simulates nothing past it, so that
/// frame's knot, and a Fire's sample before the next replay, sit at the
/// cue; the replay that opens there then records forward.
#[test]
fn a_frame_landing_past_its_placement_knots_at_the_target() {
    const CUE: f64 = 2.0;
    let calm = [0.0, -9.8, 0.0];
    let placed = live_step().with_played_target(Some(PlayedTime::from_played(CUE)));
    let mut simulation = RigidSimulation::default().with_step(placed);
    simulation
        .advance(
            &one_body([0.0, 4.0, 0.0]),
            calm,
            PlayedTime::from_played(CUE + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    simulation.set_step(placed.authored_sample());
    simulation
        .advance(
            &one_body([0.0, 4.0, 0.0]),
            calm,
            PlayedTime::from_played(CUE + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    let last = simulation.authored_samples.back().expect("knots recorded");
    assert!(
        last.time.abs() < 1e-12,
        "the knots sit at the cue, not at {}",
        last.time
    );
    replayed_frame(&mut simulation, CUE - 1e-6, CUE + 2.0 * FRAME, calm, calm);
}

/// A world evaluated twice a frame (the second a held frame) at 60 fps
/// export: replay grid point 4k/240 and frame end k/60 are one instant
/// whose two spellings can differ by an ulp (k = 23), and the held frame's
/// knot at the frame end must not land behind the replay's.
#[test]
fn a_frame_evaluated_twice_records_forward_where_grid_and_frame_spell_one_time_apart() {
    let calm = [0.0, -9.8, 0.0];
    let step = SimStep::export(Seconds(FRAME));
    let mut simulation = RigidSimulation::default().with_step(step);
    simulation
        .advance(&one_body([0.0, 4.0, 0.0]), calm, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    for k in 0..60 {
        let (from, until) = (f64::from(k) * FRAME, f64::from(k + 1) * FRAME);
        replayed_frame_at(&mut simulation, step, from, until, calm, calm);
        simulation
            .advance(
                &one_body([0.0, 4.0, 0.0]),
                calm,
                PlayedTime::from_played(until),
                1.0,
                0.0,
            )
            .unwrap_or_else(|error| panic!("frame {}: {error}", k + 1));
    }
}

/// A placement while paused, then a paused frame, then Play: the paused
/// frame knots at the cue too, so the replay that opens there on Play
/// records forward.
#[test]
fn a_paused_frame_after_a_placement_knots_at_the_target_until_play() {
    const CUE: f64 = 2.0;
    let calm = [0.0, -9.8, 0.0];
    let placed = live_step().with_played_target(Some(PlayedTime::from_played(CUE)));
    let mut simulation = RigidSimulation::default().with_step(placed);
    simulation
        .advance(
            &one_body([0.0, 4.0, 0.0]),
            calm,
            PlayedTime::from_played(CUE + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    simulation.set_step(live_step());
    simulation
        .advance(
            &one_body([0.0, 4.0, 0.0]),
            calm,
            PlayedTime::from_played(CUE + FRAME),
            1.0,
            0.0,
        )
        .unwrap();
    replayed_frame(&mut simulation, CUE - 1e-6, CUE + 2.0 * FRAME, calm, calm);
}

/// A forward seek at a 30 Hz Sim Rate from a frame half a tick past the
/// last boundary: that frame's replay took knots past the boundary, and the
/// new grid starts at the boundary's simulated time. Those knots belong to
/// transport the grid abandoned, so they go, and the jump records forward.
#[test]
fn a_forward_seek_drops_the_knots_the_abandoned_grid_took() {
    let calm = [0.0, -9.8, 0.0];
    let step = SimStep::live(Seconds(1.0 / 30.0));
    let mut simulation = RigidSimulation::default().with_step(step);
    simulation
        .advance(&one_body([0.0, 4.0, 0.0]), calm, PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    replayed_frame_at(&mut simulation, step, 0.0, FRAME, calm, calm);
    simulation.set_step(step.with_played_target(Some(PlayedTime::from_played(0.5))));
    let jump = simulation.advance(
        &one_body([0.0, 4.0, 0.0]),
        calm,
        PlayedTime::from_played(0.5 + FRAME),
        1.0,
        0.0,
    );
    assert!(jump.is_ok(), "the jump frame records forward: {jump:?}");
    replayed_frame_at(
        &mut simulation,
        step,
        0.5 - 1e-6,
        0.5 + 2.0 * FRAME,
        calm,
        calm,
    );
}
