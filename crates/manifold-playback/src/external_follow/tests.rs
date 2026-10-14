use super::*;

const FPS: f64 = 60.0;
const STEP: f64 = 1.0 / FPS;
const BPM: f64 = 120.0;
/// Ableton's audio clock against the Mac's (D6's simulations used 80 ppm).
const RATE_GAP: f64 = 80e-6;

/// Seeded xorshift so every run is reproducible.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
    }
}

fn sample(beat: f64, bpm: f64) -> ExternalSample {
    ExternalSample { beat, bpm, fresh: true, exact: false }
}

/// The music's beat at `t`: `BPM` with the rate gap, plus `offset` seconds of phase.
fn music(t: f64, offset: f64) -> f64 {
    (t + offset) * BPM / 60.0 * (1.0 + RATE_GAP)
}

struct StepRun {
    /// s from the step until the error stays under 1 ms.
    settle_1ms: f64,
    /// s from the step until the error stays under 4 ms.
    settle_4ms: f64,
    /// ms the error crosses past zero, the other way.
    overshoot_ms: f64,
    /// Largest |trim| after the step.
    max_trim: f64,
    seeks: u32,
    backward_frames: u32,
}

/// Seed, warm up 20 s so the integral learns the rate gap, then step the
/// source's phase by `step_ms` and run `after` seconds.
fn step_run(profile: FollowProfile, step_ms: f64, after: f64) -> StepRun {
    let mut f = ExternalFollower::starting_at(profile, Seconds(0.0), Beats(0.0), Bpm(BPM as f32));
    f.reseed();
    let step_at = 20.0;
    let frames = ((step_at + after) * FPS) as i64;
    let mut last_over_1 = step_at;
    let mut last_over_4 = step_at;
    let mut overshoot: f64 = 0.0;
    let mut max_trim: f64 = 0.0;
    let mut seeks = 0;
    let mut backward = 0;
    let mut prev_beat = f64::NEG_INFINITY;
    for n in 0..=frames {
        let t = n as f64 * STEP;
        let offset = if t >= step_at { step_ms / 1000.0 } else { 0.0 };
        let src = music(t, offset);
        if let FollowEvent::Seek { .. } = f.observe(Seconds(t), sample(src, BPM)) {
            seeks += 1;
        }
        let beat = f.beat_at(Seconds(t)).0;
        if beat < prev_beat {
            backward += 1;
        }
        prev_beat = beat;
        if t >= step_at {
            let err_ms = (src - beat) * 60.0 / BPM * 1000.0;
            if err_ms.abs() >= 1.0 {
                last_over_1 = t;
            }
            if err_ms.abs() >= 4.0 {
                last_over_4 = t;
            }
            overshoot = overshoot.max(-err_ms * step_ms.signum());
            max_trim = max_trim.max(f.trim().abs());
        }
    }
    StepRun {
        settle_1ms: last_over_1 - step_at,
        settle_4ms: last_over_4 - step_at,
        overshoot_ms: overshoot,
        max_trim,
        seeks,
        backward_frames: backward,
    }
}

/// D6's settle table, +20%; one overshoot under 0.6 ms; no seek, no backward frame.
#[test]
fn step_response_matches_the_profile() {
    for (step, settle) in [(2.0, 0.85), (5.0, 0.85), (10.0, 1.8), (20.0, 3.8), (-20.0, 3.8), (40.0, 7.7)] {
        let r = step_run(FollowProfile::FAST, step, 20.0);
        assert!(r.settle_1ms <= settle * 1.2, "fast {step} ms settled under 1 ms after {:.2} s", r.settle_1ms);
        assert!(r.overshoot_ms < 0.6, "fast {step} ms overshoot {:.3} ms", r.overshoot_ms);
        assert_eq!((r.seeks, r.backward_frames), (0, 0), "fast {step} ms");
    }
    for (step, under_4, under_1) in [(10.0, 2.2, 12.0), (20.0, 3.7, 13.0), (40.0, 7.5, 17.0)] {
        let r = step_run(FollowProfile::SLOW, step, 30.0);
        assert!(r.settle_4ms <= under_4 * 1.2, "slow {step} ms under 4 ms after {:.2} s", r.settle_4ms);
        assert!(r.settle_1ms <= under_1 * 1.2, "slow {step} ms under 1 ms after {:.2} s", r.settle_1ms);
        assert!(r.overshoot_ms < 1.0, "slow {step} ms overshoot {:.3} ms", r.overshoot_ms);
        assert_eq!((r.seeks, r.backward_frames), (0, 0), "slow {step} ms");
    }
}

/// The trim sits at the cap through a 40 ms step (several seconds) and the
/// frozen integral does not wind up: one overshoot, still under 0.6 ms.
#[test]
fn integral_freezes_at_the_cap() {
    let r = step_run(FollowProfile::FAST, 40.0, 20.0);
    assert!((r.max_trim - FollowProfile::FAST.trim_cap).abs() < 1e-12, "the trim reached its cap");
    assert!(r.overshoot_ms < 0.6, "overshoot {:.3} ms after 2 s at the cap", r.overshoot_ms);
}

/// Jittered samples and steps both ways: outside a catch-up window the trim
/// never passes the cap.
#[test]
fn trim_never_exceeds_the_cap_outside_catch_up() {
    for profile in [FollowProfile::FAST, FollowProfile::SLOW] {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut f = ExternalFollower::starting_at(profile, Seconds(0.0), Beats(0.0), Bpm(BPM as f32));
        for n in 0..(120.0 * FPS) as i64 {
            let t = n as f64 * STEP;
            let offset = rng.range(-0.002, 0.002) + if (t as i64 / 10) % 2 == 1 { 0.045 } else { 0.0 };
            f.observe(Seconds(t), sample(music(t, offset), BPM));
            assert!(f.catch_up_open() || f.trim().abs() <= profile.trim_cap + 1e-15, "trim {} at {t}", f.trim());
            assert!(!f.catch_up_open(), "steady tempo never opens the window");
        }
    }
}

/// +300 ms and −500 ms seek once, the row's hold after the move; +40 ms is
/// trimmed.
#[test]
fn lasting_error_seeks_after_the_hold() {
    for profile in [FollowProfile::FAST, FollowProfile::SLOW] {
        for step in [300.0, -500.0, 40.0] {
            let mut f = ExternalFollower::starting_at(profile, Seconds(0.0), Beats(0.0), Bpm(BPM as f32));
            let mut seeks = Vec::new();
            for n in 0..(10.0 * FPS) as i64 {
                let t = n as f64 * STEP;
                let offset = if t >= 5.0 { step / 1000.0 } else { 0.0 };
                let src = music(t, offset);
                if let FollowEvent::Seek { beat } = f.observe(Seconds(t), sample(src, BPM)) {
                    seeks.push(t);
                    assert_eq!(beat.0, src, "the seek goes to the source's beat");
                    f.seek_to(Seconds(t), beat, Bpm(BPM as f32));
                }
            }
            if step.abs() > SEEK_ERROR * 1000.0 {
                assert_eq!(seeks.len(), 1, "{step} ms seeks once: {seeks:?}");
                let after = seeks[0] - 5.0;
                assert!(after >= profile.seek_hold - 1e-9 && after < profile.seek_hold + STEP + 1e-9, "{step} ms sought after {after} s");
            } else {
                assert!(seeks.is_empty(), "{step} ms is trimmed: {seeks:?}");
            }
        }
    }
}

/// One wild sample moves the beat by at most the cap's worth and never seeks.
#[test]
fn single_outlier_never_seeks() {
    let mut f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(0.0), Beats(0.0), Bpm(BPM as f32));
    let mut worst: f64 = 0.0;
    for n in 0..(20.0 * FPS) as i64 {
        let t = n as f64 * STEP;
        let offset = if n == 600 { 0.3 } else { 0.0 };
        let event = f.observe(Seconds(t), sample(music(t, offset), BPM));
        assert_eq!(event, FollowEvent::None, "frame {n}");
        if t > 10.0 {
            worst = worst.max(((music(t, 0.0) - f.beat_at(Seconds(t)).0) * 60.0 / BPM).abs());
        }
    }
    assert!(worst < 0.002, "one outlier cost {:.3} ms of phase", worst * 1000.0);
}

/// Feed a tempo jump with the base tempo lagging the music by `lag` seconds.
/// Returns (window ever opened, largest trim, seconds the window stayed open).
fn tempo_jump(from: f64, to: f64, lag: f64) -> (bool, f64, f64) {
    let mut f = ExternalFollower::starting_at(FollowProfile::SLOW, Seconds(0.0), Beats(0.0), Bpm(from as f32));
    let jump = 10.0;
    let jump_beat = jump * from / 60.0;
    let (mut opened, mut max_trim, mut open_for) = (false, 0.0f64, 0.0);
    for n in 0..(20.0 * FPS) as i64 {
        let t = n as f64 * STEP;
        let beat = if t < jump { t * from / 60.0 } else { jump_beat + (t - jump) * to / 60.0 };
        let bpm = if t < jump + lag { from } else { to };
        f.observe(Seconds(t), sample(beat, bpm));
        if f.catch_up_open() {
            opened = true;
            open_for += STEP;
        }
        max_trim = max_trim.max(f.trim().abs());
    }
    (opened, max_trim, open_for)
}

/// The window opens on a confirmed tempo move and nothing else.
#[test]
fn catch_up_opens_on_a_tempo_move_only() {
    let mut rng = Rng(7);
    let mut f = ExternalFollower::starting_at(FollowProfile::SLOW, Seconds(0.0), Beats(0.0), Bpm(128.0));
    for n in 0..(300.0 * FPS) as i64 {
        let t = n as f64 * STEP;
        let bpm = 128.0 * (1.0 + rng.range(-0.005, 0.005));
        f.observe(Seconds(t), sample(t * 128.0 / 60.0 + rng.range(-0.02, 0.02), bpm));
        assert!(!f.catch_up_open(), "tempo noise of 0.5% opened the window at {t}");
    }
    let (opened, _, open_for) = tempo_jump(128.0, 174.0, 0.1);
    assert!(opened, "128 -> 174 opens the window");
    assert!(open_for <= CATCH_UP_MAX + STEP, "open {open_for} s");
    let (opened, _, _) = tempo_jump(128.0, 129.0, 0.1);
    assert!(!opened, "a 0.8% change is not a catch-up");
}

/// The catch-up cap is half the relative change, never above 25%.
#[test]
fn catch_up_cap_is_half_the_change_at_most_a_quarter() {
    for (from, to, cap) in [(174.0, 128.0, (174.0 - 128.0) / 174.0 / 2.0), (140.0, 70.0, 0.25), (128.0, 174.0, (174.0 - 128.0) / 128.0 / 2.0)] {
        let (_, max_trim, _) = tempo_jump(from, to, 0.2);
        assert!(max_trim <= cap + 1e-12, "{from} -> {to}: trim {max_trim} past cap {cap}");
        assert!(max_trim > FollowProfile::SLOW.trim_cap, "{from} -> {to}: the window lifted the cap ({max_trim})");
    }
}

/// A seed waits for a fresh sample, then takes its beat.
#[test]
fn seed_takes_the_source_beat() {
    let mut f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(1.0), Beats(10.0), Bpm(120.0));
    f.reseed();
    let stale = ExternalSample { beat: 37.3, bpm: 128.0, fresh: false, exact: false };
    assert_eq!(f.observe(Seconds(1.0 + STEP), stale), FollowEvent::None);
    assert_eq!(f.beat_at(Seconds(1.0 + STEP)).0, 10.0 + STEP * 2.0, "free-wheels until a fresh sample");
    assert_eq!(f.observe(Seconds(1.0 + 2.0 * STEP), sample(37.3, 128.0)), FollowEvent::Seeded);
    assert_eq!(f.beat_at(Seconds(1.0 + 2.0 * STEP)).0, 37.3);
    assert_eq!((f.trim(), f.base_bpm()), (0.0, Bpm(128.0)));
}

/// A handover keeps the beat and the beat speed within the cap.
#[test]
fn handover_is_bumpless() {
    let mut f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(0.0), Beats(0.0), Bpm(BPM as f32));
    let mut t = 0.0;
    for n in 0..(10.0 * FPS) as i64 {
        t = n as f64 * STEP;
        f.observe(Seconds(t), sample(music(t, 0.004), BPM));
    }
    let before = f.beat_at(Seconds(t)).0;
    let inc_before = f.beat_at(Seconds(t + STEP)).0 - before;
    f.hand_over(FollowProfile::SLOW);
    assert_eq!(f.beat_at(Seconds(t)).0, before, "the handover frame keeps its beat");
    let t1 = t + STEP;
    f.observe(Seconds(t1), sample(music(t1, 0.004), BPM));
    let inc_after = f.beat_at(Seconds(t1 + STEP)).0 - f.beat_at(Seconds(t1)).0;
    assert!((inc_after / inc_before - 1.0).abs() <= 2.0 * FollowProfile::FAST.trim_cap, "speed {inc_before} -> {inc_after}");
}

/// On a Link tempo change the segment takes Link's beat: no error left.
#[test]
fn link_tempo_change_takes_the_link_beat() {
    let mut f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(0.0), Beats(0.0), Bpm(120.0));
    let link = |t: f64| if t < 5.0 { t * 2.0 } else { 10.0 + (t - 5.0) * 140.0 / 60.0 };
    let mut worst: f64 = 0.0;
    let mut prev = f64::NEG_INFINITY;
    for n in 0..(15.0 * FPS) as i64 {
        let t = n as f64 * STEP;
        let bpm = if t < 5.0 { 120.0 } else { 140.0 };
        f.observe(Seconds(t), ExternalSample { beat: link(t), bpm, fresh: true, exact: true });
        let beat = f.beat_at(Seconds(t)).0;
        assert!(beat > prev, "monotonic at {t}");
        prev = beat;
        worst = worst.max((link(t) - beat).abs() * 60.0 / bpm);
    }
    assert!(worst < 1e-9, "Link's beat taken on its tempo change: {worst} s left");
}

/// Bad samples never break the follower (D18).
#[test]
fn non_finite_samples_are_ignored() {
    let mut f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(0.0), Beats(0.0), Bpm(120.0));
    let bad = [
        (f64::NAN, 120.0),
        (f64::INFINITY, 120.0),
        (f64::NEG_INFINITY, 120.0),
        (1.0, 0.0),
        (1.0, -1.0),
        (1.0, f64::NAN),
        (1.0, 1e300),
    ];
    let mut prev = f64::NEG_INFINITY;
    for n in 0..(30.0 * FPS) as i64 {
        let t = n as f64 * STEP;
        let (beat, bpm) = if n % 7 == 3 { bad[(n as usize / 7) % bad.len()] } else { (t * 2.0, 120.0) };
        let beat = if beat == 1.0 { t * 2.0 } else { beat };
        f.observe(Seconds(t), sample(beat, bpm));
        f.observe(Seconds(f64::NAN), sample(t * 2.0, 120.0));
        let now = f.beat_at(Seconds(t)).0;
        assert!(now.is_finite() && now > prev, "beat {now} at {t}");
        prev = now;
    }
}

/// Durations come from beat differences, carrying no anchor error.
#[test]
fn duration_is_a_beat_difference_over_the_rate() {
    let f = ExternalFollower::starting_at(FollowProfile::FAST, Seconds(100.0), Beats(200.0), Bpm(120.0));
    assert_eq!(f.duration(Beats(4.0), Beats(8.0)), Seconds(2.0));
    assert_eq!(f.time_at(Beats(202.0)), Seconds(101.0));
}

/// The segment is anchored on a time, so moving to another grid keeps the
/// beat (D8): an fps change and the export grid leave it where it was.
#[test]
fn segment_survives_grid_changes() {
    use crate::transport_clock::TransportClock;
    let mut clock = TransportClock::new(60.0);
    clock.locate(Seconds(0.0));
    let f = ExternalFollower::starting_at(FollowProfile::FAST, clock.time_at(600), Beats(20.0), Bpm(120.0));
    clock.advance(1_200);
    let before = f.beat_at(clock.time()).0;
    assert!((before - 40.0).abs() < 1e-9, "20 s at 120 BPM from beat 20 at 10 s, got {before}");
    clock.set_fps(30.0);
    assert!((f.beat_at(clock.time()).0 - before).abs() < 1e-9, "fps change moved the beat");
    clock.begin_export(24.0);
    assert!((f.beat_at(clock.time()).0 - before).abs() < 1e-9, "export grid moved the beat");
    clock.end_export(60.0);
    assert!((f.beat_at(clock.time()).0 - before).abs() < 1e-9, "end of export moved the beat");
}
