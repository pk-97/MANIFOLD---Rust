use super::*;
use manifold_core::tempo::{TempoMap, TempoMapConverter};
use manifold_core::types::TempoPointSource;

/// Seeded xorshift so every sweep is reproducible.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
    }
}

const NTSC_2997: f32 = 29.97;
const RATES: [f32; 9] = [60.0, 50.0, 30.0, 25.0, 24.0, NTSC_2997, 23.976, 59.94, 120.0];
const K: u32 = PERSIST_OBSERVATIONS;

fn playing_clock(fps: f32) -> TransportClock {
    let mut clock = TransportClock::new(fps);
    clock.locate(Seconds(0.0));
    clock
}

/// One external sample `frames` from the current grid time.
fn observe_at(clock: &mut TransportClock, frames: f64, smoothing: Smoothing) {
    let target = Seconds(clock.time().0 + frames * clock.step().0);
    clock.observe_external(target, smoothing);
}

/// A full persistence streak of exact samples, then one tick.
fn persist_then_advance(clock: &mut TransportClock, frames: f64) -> i64 {
    for _ in 0..K {
        observe_at(clock, frames, Smoothing::Exact);
    }
    clock.advance(1)
}

// ── Absolute time ────────────────────────────────────────────────────────────

#[test]
fn time_is_absolute_at_two_hours() {
    for fps in [60.0_f32, 24.0, NTSC_2997] {
        let mut clock = playing_clock(fps);
        let step = 1.0 / fps as f64;
        let frames = (2.0 * 3600.0 * fps as f64) as i64;
        for index in 1..=frames {
            assert_eq!(clock.advance(1), 1);
            assert_eq!(clock.index(), index);
            let expected = index as f64 * step;
            assert_eq!(clock.time().0.to_bits(), expected.to_bits(), "fps {fps} index {index}");
        }
    }
}

// ── D14: on-frame edges ──────────────────────────────────────────────────────

fn tempo_map(bpm: u32) -> TempoMap {
    let mut map = TempoMap::default();
    map.add_or_replace_point(Beats(0.0), Bpm(bpm as f32), TempoPointSource::Manual, 0.0001);
    map.ensure_sorted();
    map
}

/// `fps` as an exact binary fraction `num / 2^shift` (every f32 is one).
fn exact_fps(fps: f32) -> (u128, u32) {
    let mut num = fps as f64;
    let mut shift = 0;
    while num.fract() != 0.0 {
        num *= 2.0;
        shift += 1;
    }
    (num as u128, shift)
}

/// Exact frame position of edge `n16` (in 1/16 beats) as `num / den` frames.
fn edge_frame_rational(n16: u64, bpm: u32, fps: f32) -> (u128, u128) {
    let (fps_num, shift) = exact_fps(fps);
    let num = n16 as u128 * 60 * fps_num;
    let den = 16 * bpm as u128 * (1u128 << shift);
    (num, den)
}

/// The frame the lattice rule must show an edge on: its exact ceil, or the frame
/// just before when the edge sits within the sample offset after it (D14).
/// Also whether a seek to the edge must land there: an edge between the sample
/// offset and the snap tolerance after a frame snaps to the frame before it.
fn expected_edge_frame(num: u128, den: u128) -> Option<(i64, bool)> {
    let floor = (num / den) as i64;
    let rem = num % den;
    if rem == 0 {
        return Some((floor, true));
    }
    let after = rem as f64 / den as f64;
    if (after - BEAT_SAMPLE_OFFSET_FRAMES).abs() < 1e-9 || (after - SNAP_TOLERANCE_FRAMES).abs() < 1e-9 {
        return None; // on a tolerance boundary itself: either frame is honest
    }
    let frame = if after <= BEAT_SAMPLE_OFFSET_FRAMES { floor } else { floor + 1 };
    Some((frame, after > SNAP_TOLERANCE_FRAMES || after <= BEAT_SAMPLE_OFFSET_FRAMES))
}

/// The beat the engine will see on frame `index`: the clock's own sample time
/// through the tempo map.
fn sampled_beat(map: &TempoMap, bpm: u32, clock: &TransportClock, index: i64) -> f64 {
    let mut at = clock.clone();
    at.set_export_frame(index, 0);
    TempoMapConverter::seconds_to_beat_immut(map, at.beat_sample_time(), Bpm(bpm as f32)).0
}

/// Edges to check: every exactly-on-frame edge (strided to at most ~2,000) and a
/// stride-101 sample of all edges, over the first two hours.
fn candidate_edges(bpm: u32, fps: f32) -> Vec<u64> {
    let total = 16 * bpm as u64 * 120;
    let mut edges: Vec<u64> = (0..total).step_by(101).collect();
    if fps.fract() == 0.0 {
        let fps_int = fps as u64;
        let g = (16 * bpm as u64) / gcd(60 * fps_int, 16 * bpm as u64);
        let count = total / g;
        let stride = (count / 2000).max(1);
        edges.extend((0..count).step_by(stride as usize).map(|i| i * g));
    }
    edges
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[test]
fn on_frame_edges_land_on_their_frame() {
    let mut on_frame = 0usize;
    for fps in [60.0_f32, 50.0, 30.0, 25.0, 24.0, NTSC_2997] {
        let clock = TransportClock::new(fps);
        for bpm in 120..=180u32 {
            let map = tempo_map(bpm);
            for n16 in candidate_edges(bpm, fps) {
                let (num, den) = edge_frame_rational(n16, bpm, fps);
                let Some((frame, seek_lands)) = expected_edge_frame(num, den) else { continue };
                if num % den == 0 {
                    on_frame += 1;
                }
                let edge = n16 as f64 / 16.0;
                assert!(
                    sampled_beat(&map, bpm, &clock, frame) >= edge,
                    "{fps} fps {bpm} BPM edge {edge}: frame {frame} must show it"
                );
                if frame > 0 {
                    assert!(
                        sampled_beat(&map, bpm, &clock, frame - 1) < edge,
                        "{fps} fps {bpm} BPM edge {edge}: frame {} must not show it",
                        frame - 1
                    );
                }
                // The seek rule agrees with the beat rule.
                if seek_lands {
                    let edge_time = TempoMapConverter::beat_to_seconds_immut(&map, Beats(edge), Bpm(bpm as f32));
                    assert_eq!(clock.index_at_or_after(edge_time), frame, "{fps} fps {bpm} BPM edge {edge}: seek");
                }
            }
        }
    }
    assert!(on_frame > 50_000, "sweep must cover on-frame edges, saw {on_frame}");
}

/// The D14 test discriminates: sampling at exact frame time, as today, shows
/// on-frame edges one frame late at 150 BPM / 60 fps.
#[test]
fn todays_sampling_misses_on_frame_edges() {
    let (bpm, fps) = (150u32, 60.0_f32);
    let map = tempo_map(bpm);
    let clock = TransportClock::new(fps);
    let mut late = 0;
    let mut total = 0;
    for n16 in candidate_edges(bpm, fps) {
        let (num, den) = edge_frame_rational(n16, bpm, fps);
        if num % den != 0 {
            continue;
        }
        total += 1;
        let frame = (num / den) as i64;
        let beat = TempoMapConverter::seconds_to_beat_immut(&map, clock.time_at(frame), Bpm(bpm as f32)).0;
        if beat < n16 as f64 / 16.0 {
            late += 1;
        }
    }
    assert!(late * 4 > total, "expected about 40% late at 150 BPM, saw {late} of {total}");
}

// ── D4: snapping and holds ───────────────────────────────────────────────────

#[test]
fn seek_snaps_forward_and_round_trips() {
    for fps in RATES {
        let mut clock = TransportClock::new(fps);
        let step = clock.step().0;
        for index in (0..5_000).chain(431_000..433_000) {
            let t = clock.time_at(index);
            assert_eq!(clock.index_at_or_after(t), index, "{fps} round trip {index}");
            assert_eq!(clock.index_nearest(t), index);
            assert_eq!(clock.index_at_or_after(Seconds(t.0 + 0.5 * step)), index + 1);
            assert_eq!(clock.index_at_or_after(Seconds(t.0 + 1e-4 * step)), index + 1);
            assert_eq!(clock.index_nearest(Seconds(t.0 + 0.4 * step)), index);
            clock.locate(Seconds(t.0 + 0.3 * step));
            assert_eq!(clock.index(), index + 1, "{fps} locate snaps forward");
        }
    }
}

/// A beat sampled on frame k (D14), turned back into a time through the map,
/// snaps to k: the snap tolerance is wider than the sample offset. Fails with
/// the two equal, where about one restore in ten lands a frame late.
#[test]
fn sampled_beats_round_trip_to_their_frame() {
    for fps in RATES {
        for bpm in [120u32, 128, 141, 150, 174] {
            let map = tempo_map(bpm);
            let clock = TransportClock::new(fps);
            for index in (0..5_000).chain(431_000..432_000) {
                let beat = sampled_beat(&map, bpm, &clock, index);
                let t = TempoMapConverter::beat_to_seconds_immut(&map, Beats(beat), Bpm(bpm as f32));
                assert_eq!(clock.index_at_or_after(t), index, "{fps} fps {bpm} BPM frame {index}");
            }
        }
    }
}

/// STOPGAP(frame-exact P3): a per-tick external target inside ¾ frame of the
/// grid leaves it alone; past ¾ it goes to the nearest frame.
#[test]
fn follow_external_keeps_the_grid_inside_three_quarters_of_a_frame() {
    let mut clock = playing_clock(60.0);
    clock.advance(100);
    let at = |clock: &TransportClock, frames: f64| Seconds(clock.time_at(100).0 + frames * clock.step().0);
    for frames in [-0.74, -0.5, -0.2, 0.0, 0.49, 0.51, 0.74] {
        assert!(!clock.follow_external(at(&clock, frames)), "{frames}");
        assert_eq!(clock.index(), 100, "{frames}");
    }
    assert!(clock.follow_external(at(&clock, 0.8)));
    assert_eq!(clock.index(), 101);
    assert!(clock.follow_external(at(&clock, -0.3)), "1.3 frames behind the grid now");
    assert_eq!(clock.index(), 100);
    assert!(clock.follow_external(at(&clock, -1.6)));
    assert_eq!(clock.index(), 98);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!clock.follow_external(Seconds(bad)));
    }
    assert_eq!(clock.index(), 98);
}

#[test]
fn only_playing_seek_holds() {
    let mut clock = TransportClock::new(60.0);
    let t = Seconds(10.25);
    let anchor = clock.index_at_or_after(t);

    // Play / resume / paused seek: advance normally from the anchor.
    clock.locate(t);
    assert_eq!(clock.index(), anchor);
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.index(), anchor + 1);

    // Seek while playing: the next tick renders the target.
    clock.seek_playing(t);
    assert_eq!(clock.advance(1), 0);
    assert_eq!(clock.index(), anchor);
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.index(), anchor + 1);

    // A stall right after a playing seek still shows the target first.
    clock.seek_playing(t);
    assert_eq!(clock.advance(3), 0);
    assert_eq!(clock.index(), anchor);

    // External locate: nearest frame, no hold.
    let near = Seconds(clock.time_at(anchor).0 + 0.3 * clock.step().0);
    clock.locate_nearest(near);
    assert_eq!(clock.index(), anchor);
    assert_eq!(clock.advance(1), 1);
}

#[test]
fn stall_skips_and_stays_in_time() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    clock.advance(300);
    assert_eq!(clock.index(), 301);
    assert_eq!(clock.last_advance(), 300);
}

#[test]
fn reset_returns_to_zero_and_clears_hold() {
    let mut clock = TransportClock::new(60.0);
    clock.seek_playing(Seconds(4.0));
    clock.reset();
    assert_eq!(clock.index(), 0);
    assert_eq!(clock.advance(1), 1);
}

// ── D10, D9, D17: fps change and export ──────────────────────────────────────

#[test]
fn set_fps_keeps_time_and_snaps_forward() {
    let mut clock = playing_clock(60.0);
    clock.advance(126); // 2.1 s, off the 24 fps grid
    let before = clock.time();
    clock.set_fps(24.0);
    assert_eq!(clock.fps(), 24.0);
    assert_eq!(clock.index(), clock.index_at_or_after(before));
    assert!(clock.time().0 >= before.0 && clock.time().0 - before.0 < clock.step().0);
    assert_eq!(clock.advance(1), 1, "no hold after an fps change");
}

#[test]
fn fps_outside_one_to_a_thousand_falls_back_to_sixty() {
    for fps in [0.0_f32, -24.0, 0.5, 1000.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, 1e-30] {
        let clock = TransportClock::new(fps);
        assert_eq!(clock.fps(), 60.0, "{fps}");
        assert_eq!(clock.step().0.to_bits(), (1.0_f64 / 60.0).to_bits(), "{fps}");
        let mut clock = playing_clock(24.0);
        clock.set_fps(fps);
        assert_eq!(clock.fps(), 60.0, "set_fps {fps}");
        clock.begin_export(fps);
        assert_eq!(clock.fps(), 60.0, "begin_export {fps}");
    }
    for fps in [1.0_f32, 23.976, 1000.0] {
        assert_eq!(TransportClock::new(fps).fps(), fps, "{fps} is accepted");
    }
}

/// Export frame k is the first song-grid frame at or after the start, plus k
/// (D17), so live and export sample identical times for the same song moment.
#[test]
fn export_frames_sit_on_the_song_grid() {
    let start = Seconds(37.123_456_789);
    for fps in [24.0_f32, NTSC_2997, 60.0] {
        let mut clock = playing_clock(60.0);
        clock.begin_export(fps);
        assert_eq!(clock.fps(), fps);
        let step = 1.0 / fps as f64;
        let first = clock.index_at_or_after(start);
        for k in 0..20_000u32 {
            clock.set_export_frame(first, k);
            let expected = (first + i64::from(k)) as f64 * step;
            assert_eq!(clock.time().0.to_bits(), expected.to_bits(), "{fps} frame {k}");
        }
        clock.set_export_frame(first, 0);
        assert!(clock.time().0 >= start.0 && clock.time().0 - start.0 < step, "{fps}: frame 0 is the first at or after the start");
        clock.end_export(60.0);
        assert_eq!(clock.fps(), 60.0);
    }

    let mut live = playing_clock(60.0);
    live.locate(start);
    let mut export = TransportClock::new(24.0);
    export.begin_export(60.0);
    let first = export.index_at_or_after(start);
    for k in 0..1_000u32 {
        export.set_export_frame(first, k);
        assert_eq!(export.time().0.to_bits(), live.time().0.to_bits(), "export frame {k} equals live");
        live.advance(1);
    }
}

#[test]
fn export_frame_clears_a_pending_hold() {
    let mut clock = playing_clock(60.0);
    clock.seek_playing(Seconds(2.0));
    clock.set_export_frame(clock.index(), 0);
    assert_eq!(clock.advance(1), 1, "an export frame leaves no seek hold behind");
}

// ── D6: phase lock ───────────────────────────────────────────────────────────

#[test]
fn correction_needs_a_persistent_streak() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    // One sample back inside ¾ breaks the streak.
    for _ in 0..K - 1 {
        observe_at(&mut clock, 0.9, Smoothing::Exact);
    }
    observe_at(&mut clock, 0.5, Smoothing::Exact);
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.last_correction(), Correction::None);

    // One short of the streak: nothing yet. The last sample completes it.
    for _ in 0..K - 1 {
        observe_at(&mut clock, 0.9, Smoothing::Exact);
    }
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.last_correction(), Correction::None);
    observe_at(&mut clock, 0.9, Smoothing::Exact);
    assert_eq!(clock.advance(1), 2);
    assert_eq!(clock.last_correction(), Correction::Skip);
    let drift = clock.last_correction_drift().0 / clock.step().0;
    assert!((drift - 0.9).abs() < 1e-9, "the drift that triggered the skip, got {drift}");
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.last_correction_drift().0, 0.0);

    // A change of side starts a new streak.
    for _ in 0..K - 1 {
        observe_at(&mut clock, 0.9, Smoothing::Exact);
    }
    for _ in 0..K - 1 {
        observe_at(&mut clock, -0.9, Smoothing::Exact);
    }
    assert_eq!(clock.advance(1), 1);
    assert_eq!(clock.last_correction(), Correction::None);
    observe_at(&mut clock, -0.9, Smoothing::Exact);
    assert_eq!(clock.advance(1), 0);
    assert_eq!(clock.last_correction(), Correction::Hold);

    // Each slew frame needs its own streak: after a skip at 2.0 the residual
    // 1.0 is still past ¾, but one sample is not enough.
    persist_then_advance(&mut clock, 2.0);
    assert_eq!(clock.last_correction(), Correction::Skip);
    observe_at(&mut clock, 1.0, Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);
}

#[test]
fn correction_subtracts_from_estimate() {
    for smoothing in [Smoothing::Exact, Smoothing::Coarse] {
        let mut clock = playing_clock(60.0);
        // At 1.4 a coarse filter that kept the old drift after the skip stays
        // past ¾ for a full streak while it decays toward 0.4, and skips again.
        let corrections = simulate(&mut clock, Source::Constant(smoothing), 1.4, 120.0, 10_000, 1);
        assert_eq!(corrections, vec![Correction::Skip], "{smoothing:?}: one skip, then quiet");
    }
}

/// Up to 2½ frames the lock slews a frame at a time and the index never moves
/// backward. 64 fps makes every drift here exact in binary.
#[test]
fn slew_never_rewinds() {
    for (drift, holds) in [(-1.0, -1), (-1.5, -1), (-1.9, -2), (-2.01, -2), (-2.3, -2), (-2.5, -2)] {
        let mut clock = playing_clock(64.0);
        let step = clock.step().0;
        let mut last = clock.index();
        for n in 1..=2_000i64 {
            clock.advance(1);
            assert!(clock.index() >= last, "slew moved backward at drift {drift}");
            assert!(!matches!(clock.last_correction(), Correction::Jump(_)), "drift {drift} jumped");
            last = clock.index();
            let truth = (n as f64 + drift) * step;
            clock.observe_external(Seconds(truth), Smoothing::Exact);
        }
        assert_eq!(clock.index() - 2_000, holds, "holds absorb {drift}");
    }
}

#[test]
fn jumps_beyond_two_and_a_half_frames() {
    let mut clock = playing_clock(64.0);
    clock.advance(1);
    persist_then_advance(&mut clock, 5.3);
    assert_eq!(clock.last_correction(), Correction::Jump(5));
    assert_eq!(clock.index(), 7);

    persist_then_advance(&mut clock, -4.6);
    assert_eq!(clock.last_correction(), Correction::Jump(-5));
    assert_eq!(clock.index(), 3, "a jump may move backward");

    persist_then_advance(&mut clock, -2.75);
    assert_eq!(clock.last_correction(), Correction::Jump(-3));
    assert_eq!(clock.index(), 1);

    // Exactly 2½ is still a slew.
    persist_then_advance(&mut clock, 2.5);
    assert_eq!(clock.last_correction(), Correction::Skip);
    persist_then_advance(&mut clock, -2.5);
    assert_eq!(clock.last_correction(), Correction::Hold);
}

/// The seek-hold tick has no frame to remove, so a due hold waits for the
/// next tick instead of stepping the index backward (D6).
#[test]
fn due_hold_waits_when_there_is_no_frame_to_remove() {
    let mut clock = playing_clock(60.0);
    clock.advance(10);
    clock.seek_playing(Seconds(1.0));
    let target = clock.index();
    for _ in 0..K {
        observe_at(&mut clock, -1.0, Smoothing::Exact);
    }
    assert_eq!(clock.advance(1), 0, "the seek hold renders the target");
    assert_eq!(clock.index(), target);
    assert_eq!(clock.last_correction(), Correction::None);
    assert_eq!(clock.advance(1), 0, "the hold lands on the next tick");
    assert_eq!(clock.last_correction(), Correction::Hold);
    assert_eq!(clock.advance(1), 1);

    for _ in 0..K {
        observe_at(&mut clock, -1.0, Smoothing::Exact);
    }
    assert_eq!(clock.advance(0), 0, "a zero-frame tick has nothing to remove either");
    assert_eq!(clock.last_correction(), Correction::None);
    assert_eq!(clock.advance(1), 0);
    assert_eq!(clock.last_correction(), Correction::Hold);
}

#[test]
fn authority_change_clears_and_reseeds() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    // Coarse seeds from its first sample; averaged in from 0 it would sit near
    // 0.42 after a streak's worth of samples.
    clock.clear_external();
    for _ in 0..K {
        observe_at(&mut clock, 0.9, Smoothing::Coarse);
    }
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Skip, "first sample seeds the estimate");

    // After a clear, the opposite side needs only its own streak.
    clock.clear_external();
    for _ in 0..K {
        observe_at(&mut clock, -0.9, Smoothing::Coarse);
    }
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Hold);

    // Re-anchors clear too: a full exact streak would otherwise skip.
    for _ in 0..K {
        observe_at(&mut clock, 0.9, Smoothing::Exact);
    }
    clock.locate(clock.time());
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);

    for _ in 0..K {
        observe_at(&mut clock, 0.9, Smoothing::Exact);
    }
    clock.clear_external();
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);
}

/// After a skip the residual sits inside ¾; when the source then moves back
/// past ¾ on the other side, one hold follows within a streak. The old re-arm
/// band left −0.8..−1.2 uncorrected for an hour.
#[test]
fn reverse_after_skip_is_never_stuck() {
    for back in [-0.6, -0.8, -0.9, -1.0, -1.1, -1.2, -1.6] {
        let mut clock = playing_clock(60.0);
        let step = clock.step().0;
        let mut corrections = Vec::new();
        let mut reversed_at = None;
        let moves_at = 600i64;
        for n in 1..=216_000i64 {
            clock.advance(1);
            if clock.last_correction() != Correction::None {
                corrections.push(clock.last_correction());
                if n > moves_at && reversed_at.is_none() {
                    reversed_at = Some(n - moves_at);
                }
            }
            // +1.3 frames ahead, so one skip leaves +0.3; then the source moves
            // so the residual becomes `back`.
            let offset = if n < moves_at { 1.3 } else { 1.0 + back };
            clock.observe_external(Seconds((n as f64 + offset) * step), Smoothing::Exact);
        }
        if back < -ACT_ABOVE_FRAMES {
            assert_eq!(corrections, vec![Correction::Skip, Correction::Hold], "back {back}");
            assert!(reversed_at.unwrap() <= i64::from(K) + 1, "back {back}: hold after {reversed_at:?} ticks");
        } else {
            assert_eq!(corrections, vec![Correction::Skip], "back {back}");
        }
    }
}

/// A coarse locate below the seek threshold is one jump: two agreeing samples
/// re-seed the estimate instead of averaging toward it.
#[test]
fn coarse_locate_is_one_jump() {
    for (frames, jump) in [(3.0, 3), (24.0, 24), (100.0, 100), (-24.0, -24)] {
        let mut clock = playing_clock(60.0);
        let step = clock.step().0;
        let mut rng = Rng(5);
        let mut got = Vec::new();
        for n in 1..=3_000i64 {
            clock.advance(1);
            if clock.last_correction() != Correction::None {
                got.push(clock.last_correction());
            }
            let offset = if n < 300 { 0.0 } else { frames };
            if n % 2 == 0 {
                let target = (n as f64 + offset + rng.range(-0.05, 0.05)) * step;
                clock.observe_external(Seconds(target), Smoothing::Coarse);
            }
        }
        assert_eq!(got, vec![Correction::Jump(jump)], "{frames}-frame locate");
    }
}

#[test]
fn coarse_outlier_is_ignored() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    // One short of a streak at 0.9; an outlier must neither complete it nor move the estimate.
    for _ in 0..K - 1 {
        observe_at(&mut clock, 0.9, Smoothing::Coarse);
    }
    observe_at(&mut clock, 50.0, Smoothing::Coarse);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);
    observe_at(&mut clock, 0.9, Smoothing::Coarse);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Skip, "the outlier left the streak and estimate alone");

    // Two outliers that disagree are not a locate.
    observe_at(&mut clock, 50.0, Smoothing::Coarse);
    observe_at(&mut clock, 20.0, Smoothing::Coarse);
    for _ in 0..3_000 {
        observe_at(&mut clock, 0.0, Smoothing::Coarse);
        clock.advance(1);
        assert_eq!(clock.last_correction(), Correction::None);
    }
}

#[test]
fn non_finite_and_absurd_input_is_safe() {
    let mut clock = playing_clock(60.0);
    clock.advance(600);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        clock.locate(Seconds(bad));
        clock.seek_playing(Seconds(bad));
        clock.locate_nearest(Seconds(bad));
        assert_eq!(clock.index(), 600, "{bad} is ignored");
        for _ in 0..K {
            clock.observe_external(Seconds(bad), Smoothing::Exact);
            clock.observe_external(Seconds(bad), Smoothing::Coarse);
        }
    }
    assert_eq!(clock.advance(1), 1, "non-finite input set no hold and no correction");
    assert_eq!(clock.last_correction(), Correction::None);

    for huge in [1e300, -1e300] {
        clock.locate(Seconds(huge));
        assert!(clock.time().0.is_finite());
        clock.advance(u32::MAX);
        clock.advance(u32::MAX);
        clock.seek_playing(Seconds(huge));
        clock.advance(1);
        assert!(clock.time().0.is_finite());
    }

    let mut clock = playing_clock(60.0);
    clock.advance(1);
    for target in [1e300, -1e300] {
        for _ in 0..K {
            clock.observe_external(Seconds(target), Smoothing::Exact);
        }
        clock.advance(1);
        let Correction::Jump(n) = clock.last_correction() else {
            panic!("{target}: expected a jump, got {:?}", clock.last_correction())
        };
        assert!(n.unsigned_abs() as f64 <= MAX_DRIFT_FRAMES, "{target}: jump {n} is clamped");
    }
    clock.set_export_frame(i64::MAX, u32::MAX);
    assert!(clock.time().0.is_finite());
    clock.set_export_frame(i64::MIN, 0);
    clock.advance(u32::MAX);
    assert!(clock.time().0.is_finite());
}

/// External source models. Each produces the target the P3 source code will
/// hand the clock: its position projected to this frame's deadline. The
/// projection uses the true tempo and exact send times; P3 owns the error of
/// its own BPM estimate and arrival stamps.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A target every frame with no error.
    Constant(Smoothing),
    /// 24 PPQN ticks with ±1 ms arrival jitter, projected from arrival (D7).
    MidiClock,
    /// The P3 M4L sender: unquantised song seconds every `period_ms` on its
    /// own free-running metro, starting at `phase` of a period, ±1 ms
    /// arrival jitter, projected from arrival (D7).
    SongSeconds { period_ms: f64, phase: f64 },
}

/// Run `ticks` frames against a source whose true position is `drift` frames
/// ahead of the grid at start. Returns the corrections applied.
fn simulate(
    clock: &mut TransportClock,
    source: Source,
    drift: f64,
    bpm: f64,
    ticks: usize,
    seed: u64,
) -> Vec<Correction> {
    let step = clock.step().0;
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut out = Vec::new();
    // Wall time of frame n's deadline is n * step; the source's transport
    // position at wall w is w + drift * step.
    let mut last_event: i64 = -1;
    for n in 1..=ticks as i64 {
        clock.advance(1);
        if clock.last_correction() != Correction::None {
            out.push(clock.last_correction());
        }
        let wall = n as f64 * step;
        let truth = wall + drift * step;
        let sample = match source {
            Source::Constant(smoothing) => Some((truth, smoothing)),
            Source::MidiClock => {
                let period = 60.0 / (bpm * 24.0);
                // Latest tick whose arrival (true time + jitter) is at or before the deadline.
                let k = ((truth - 0.001) / period).floor() as i64;
                if k > last_event {
                    last_event = k;
                    let jitter = rng.range(-0.001, 0.001);
                    let position = k as f64 * period;
                    let arrival = position - drift * step + jitter;
                    Some((position + (wall - arrival), Smoothing::Coarse))
                } else {
                    None
                }
            }
            Source::SongSeconds { period_ms, phase } => {
                let period = period_ms / 1000.0;
                let k = ((wall - phase * period - 0.001) / period).floor() as i64;
                if k > last_event {
                    last_event = k;
                    let jitter = rng.range(-0.001, 0.001);
                    let send_wall = (k as f64 + phase) * period;
                    let position = send_wall + drift * step;
                    let arrival = send_wall + jitter;
                    Some((position + (wall - arrival), Smoothing::Coarse))
                } else {
                    None
                }
            }
        };
        if let Some((target, smoothing)) = sample {
            clock.observe_external(Seconds(target), smoothing);
        }
    }
    out
}

fn assert_no_flap(corrections: &[Correction], context: &str) {
    let mut sign = 0i64;
    for c in corrections {
        let s = c.frames().signum();
        assert!(sign == 0 || s == sign, "{context}: flapped {corrections:?}");
        sign = s;
    }
}

#[test]
fn phase_lock_hysteresis_no_flap() {
    let phases: Vec<f64> = (-32..=32).map(|i| i as f64 / 64.0).collect();
    for base in [0.0, 0.6, -0.6, 0.8, -0.8] {
        for (pi, phase) in phases.iter().enumerate() {
            let drift = base + phase;
            let mut sources = vec![(Source::Constant(Smoothing::Exact), 120.0)];
            for bpm in (90..=180).step_by(15) {
                sources.push((Source::MidiClock, bpm as f64));
            }
            for (source, bpm) in sources {
                let mut clock = playing_clock(60.0);
                let seed = (pi as u64 + 1) * 1_000 + bpm as u64;
                let got = simulate(&mut clock, source, drift, bpm, 3_600, seed);
                let context = format!("{source:?} {bpm} BPM drift {drift:+.3}");
                assert_no_flap(&got, &context);
                let net: i64 = got.iter().map(|c| c.frames()).sum();
                if drift.abs() < 0.65 {
                    assert!(got.is_empty(), "{context}: corrected {got:?}");
                } else if drift.abs() > 0.85 {
                    assert_eq!(net, drift.signum() as i64, "{context}: {got:?}");
                    assert_eq!(got.len(), 1, "{context}: {got:?}");
                } else {
                    assert!(got.len() <= 1, "{context}: {got:?}");
                }
            }
        }
    }
}

/// Noise past the ¼-frame contract, swept over true offset and start phase,
/// for an hour each: a constant offset gets at most two corrections more than
/// a clean source needs. One observation per correction gives 18,000 an hour
/// at ±0.3 frame.
#[test]
fn phase_lock_bounded_under_noise() {
    let hour = 216_000u32;
    let cases: [(Smoothing, u32, &[f64]); 3] = [
        (Smoothing::Exact, 1, &[0.0, 0.1, 0.2, 0.25, 0.3]),
        (Smoothing::Coarse, 1, &[0.0, 0.25, 0.5]),
        (Smoothing::Coarse, 2, &[0.0, 0.25, 0.5]),
    ];
    for (smoothing, every, noises) in cases {
        for &noise in noises {
            for offset16 in -24..=24 {
                let offset = offset16 as f64 / 16.0;
                for phase in 0..every.max(2) {
                    let mut clock = playing_clock(60.0);
                    let mut rng = Rng(0x5EED ^ (u64::from(phase) << 32) ^ (offset16 + 100) as u64);
                    let mut corrections = 0u32;
                    for n in 0..hour {
                        clock.advance(1);
                        if clock.last_correction() != Correction::None {
                            corrections += 1;
                        }
                        if (n + phase) % every == 0 {
                            let residual = offset - corrections_net(&clock, n);
                            observe_at(&mut clock, residual + rng.range(-noise, noise), smoothing);
                        }
                    }
                    let needed = if offset.abs() > ACT_ABOVE_FRAMES { (offset.abs() - ACT_ABOVE_FRAMES).ceil() as u32 } else { 0 };
                    assert!(
                        corrections <= needed + 2,
                        "{smoothing:?} every {every} noise ±{noise} offset {offset:+} phase {phase}: {corrections} corrections in an hour"
                    );
                }
            }
        }
    }
}

/// Net frames the corrections have moved the grid: the index minus the ticks.
fn corrections_net(clock: &TransportClock, ticks_before: u32) -> f64 {
    (clock.index() - i64::from(ticks_before) - 1) as f64
}

/// Projected sources hold phase at matched tempo: MIDI clock over the start
/// phase and tempo sweep, and the P3 song-seconds sender over free-running
/// send periods. A sender that quantises to timecode frames on an unlocked
/// metro does not; P3 changes the sender (D7).
#[test]
fn projected_sources_hold_phase() {
    let two_hours = 2 * 3600 * 60;
    for bpm in [120.0, 174.0] {
        for drift in [-0.5, 0.0, 0.5] {
            let mut clock = playing_clock(60.0);
            let got = simulate(&mut clock, Source::MidiClock, drift, bpm, two_hours, 7);
            assert!(got.is_empty(), "MIDI {bpm} BPM drift {drift}: {got:?}");
        }
    }
    for period_ms in [40.0, 40.1, 33.3, 33.4, 20.0, 50.0] {
        for phase in [0.1, 0.5, 0.9] {
            for drift in [-0.5, 0.0, 0.5] {
                let mut clock = playing_clock(60.0);
                let source = Source::SongSeconds { period_ms, phase };
                let got = simulate(&mut clock, source, drift, 120.0, two_hours, 11);
                assert!(got.is_empty(), "song seconds every {period_ms} ms phase {phase} drift {drift}: {got:?}");
            }
        }
    }
}

// ── D8: live tempo ───────────────────────────────────────────────────────────

#[test]
fn live_tempo_is_absolute_and_continuous() {
    let clock = TransportClock::new(60.0);
    let mut tempo = LiveTempo::new(clock.time_at(120), Beats(4.0), Bpm(120.0));
    let at = |tempo: &LiveTempo, index: i64| tempo.beat_at(clock.time_at(index)).0;
    assert_eq!(at(&tempo, 120), 4.0);
    assert!((at(&tempo, 150) - 5.0).abs() < 1e-12, "30 frames at 120 BPM is one beat");
    assert!((tempo.time_at(Beats(5.0)).0 - clock.time_at(150).0).abs() < 1e-12);

    // A ramp: re-anchor every 15 frames, BPM up 0.5 each time. The beat never jumps.
    let mut index = 150;
    for i in 0..64 {
        let before = at(&tempo, index);
        tempo.reanchor(clock.time_at(index), Bpm(120.0 + 0.5 * i as f32));
        assert_eq!(at(&tempo, index).to_bits(), before.to_bits(), "continuous at {index}");
        index += 15;
    }
    assert!(at(&tempo, index) > at(&tempo, index - 1));
}

/// The segment is anchored on a time, so moving to another grid keeps the beat.
#[test]
fn live_tempo_survives_grid_changes() {
    let mut clock = playing_clock(60.0);
    let tempo = LiveTempo::new(clock.time_at(600), Beats(20.0), Bpm(120.0));
    clock.advance(1_200);
    let before = tempo.beat_at(clock.time()).0;
    assert!((before - 40.0).abs() < 1e-9, "20 s at 120 BPM from beat 20 at 10 s, got {before}");
    clock.set_fps(30.0);
    assert!((tempo.beat_at(clock.time()).0 - before).abs() < 1e-9, "fps change moved the beat");
    clock.begin_export(24.0);
    assert!((tempo.beat_at(clock.time()).0 - before).abs() < 1e-9, "export grid moved the beat");
    clock.end_export(60.0);
    assert!((tempo.beat_at(clock.time()).0 - before).abs() < 1e-9, "end of export moved the beat");
}
