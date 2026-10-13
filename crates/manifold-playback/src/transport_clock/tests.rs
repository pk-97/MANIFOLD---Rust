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

fn playing_clock(fps: f32) -> TransportClock {
    let mut clock = TransportClock::new(fps);
    clock.locate(Seconds(0.0));
    clock
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
            let expected = 0.0 + index as f64 * step;
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
/// just before when the edge sits within the tolerance after it (D14).
fn expected_edge_frame(num: u128, den: u128) -> Option<i64> {
    let floor = (num / den) as i64;
    let rem = num % den;
    if rem == 0 {
        return Some(floor);
    }
    let after = rem as f64 / den as f64;
    if (after - LATTICE_TOLERANCE_FRAMES).abs() < 1e-9 {
        return None; // on the tolerance boundary itself: either frame is honest
    }
    Some(if after <= LATTICE_TOLERANCE_FRAMES { floor } else { floor + 1 })
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
                let Some(frame) = expected_edge_frame(num, den) else { continue };
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
                let edge_time = TempoMapConverter::beat_to_seconds_immut(&map, Beats(edge), Bpm(bpm as f32));
                assert_eq!(clock.index_at_or_after(edge_time), frame, "{fps} fps {bpm} BPM edge {edge}: seek");
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

// ── D10, D9: fps change and export ───────────────────────────────────────────

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
fn export_frames_match_the_export_formula() {
    for fps in [24.0_f32, NTSC_2997, 60.0] {
        let origin = Seconds(37.123_456_789);
        let mut clock = TransportClock::new(60.0);
        clock.begin_export(fps, origin);
        let dt = 1.0 / fps as f64;
        for k in 0..20_000u32 {
            clock.set_export_frame(0, k);
            let today = origin.0 + k as f64 * dt;
            assert_eq!(clock.time().0.to_bits(), today.to_bits(), "{fps} frame {k}");
        }
        // Song-time-0 anchor: frame k sits on the live grid.
        let mut song = TransportClock::new(60.0);
        song.begin_export(fps, Seconds(0.0));
        let first = song.index_at_or_after(origin);
        song.set_export_frame(first, 0);
        assert!(song.time().0 >= origin.0 && song.time().0 - origin.0 < dt);
        song.set_export_frame(first, 7);
        assert_eq!(song.time().0.to_bits(), ((first + 7) as f64 * dt).to_bits());
        song.end_export(60.0);
        assert_eq!(song.fps(), 60.0);
        assert_eq!(song.time_at(0).0, 0.0, "live origin restored");
    }
}

// ── D6: phase lock ───────────────────────────────────────────────────────────

fn applied(c: Correction) -> i64 {
    match c {
        Correction::None => 0,
        Correction::Skip => 1,
        Correction::Hold => -1,
        Correction::Jump(n) => n,
    }
}

#[test]
fn correction_subtracts_from_estimate() {
    for smoothing in [Smoothing::Exact, Smoothing::Coarse] {
        let mut clock = playing_clock(60.0);
        // At 0.95 a filter that kept the old drift after the skip would skip again.
        let corrections = simulate(&mut clock, Source::Constant(smoothing), 0.95, 120.0, 10_000, 1);
        assert_eq!(corrections, vec![Correction::Skip], "{smoothing:?}: one skip, then quiet");
    }
}

#[test]
fn slew_holds_never_rewind() {
    // Holds stop once the residual is inside ¾ frame: -1.5 needs one, -1.9 two.
    for (drift, holds) in [(-1.0, -1), (-1.5, -1), (-1.9, -2)] {
        let mut clock = playing_clock(60.0);
        let step = clock.step().0;
        let mut last = clock.index();
        for n in 1..=2_000i64 {
            clock.advance(1);
            assert!(clock.index() >= last, "slew moved backward at drift {drift}");
            last = clock.index();
            let truth = (n as f64 + drift) * step;
            clock.observe_external(Seconds(truth), Smoothing::Exact);
        }
        let net = clock.index() - 2_000;
        assert_eq!(net, holds, "holds absorb {drift}");
    }
}

#[test]
fn jumps_beyond_two_frames() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    clock.observe_external(Seconds(clock.time().0 + 5.3 * clock.step().0), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Jump(5));
    assert_eq!(clock.index(), 7);

    clock.observe_external(Seconds(clock.time().0 - 4.6 * clock.step().0), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Jump(-5));
    assert_eq!(clock.index(), 3, "a jump may move backward");
}

#[test]
fn authority_change_clears_and_reseeds() {
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    // Coarse: an unseeded average from 0 would need many samples to cross ¾.
    clock.clear_external();
    clock.observe_external(Seconds(clock.time().0 + 0.9 * clock.step().0), Smoothing::Coarse);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Skip, "first sample seeds the estimate");

    // After a clear, the opposite direction is free at once.
    clock.clear_external();
    clock.observe_external(Seconds(clock.time().0 - 0.9 * clock.step().0), Smoothing::Coarse);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Hold);

    // Re-anchors clear too.
    clock.observe_external(Seconds(clock.time().0 + 0.9 * clock.step().0), Smoothing::Coarse);
    clock.locate(clock.time());
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);
}

#[test]
fn reverse_correction_needs_rearm() {
    let step = 1.0 / 60.0;
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    let at = |clock: &TransportClock, frames: f64| Seconds(clock.time().0 + frames * step);
    clock.observe_external(at(&clock, 1.3), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Skip);
    // Residual +0.3, not yet back inside ¼: the reverse is refused.
    clock.observe_external(at(&clock, -0.3), Smoothing::Exact);
    clock.observe_external(at(&clock, -0.8), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::None);
    // Back inside ¼, then out again: allowed.
    clock.observe_external(at(&clock, -0.1), Smoothing::Exact);
    clock.observe_external(at(&clock, -0.8), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Hold);
}

/// A real reverse move larger than any flapping noise is never refused, even
/// when an exact source steps across the re-arm band without landing in it.
#[test]
fn large_reverse_is_never_stuck() {
    let step = 1.0 / 60.0;
    let mut clock = playing_clock(60.0);
    clock.advance(1);
    let at = |clock: &TransportClock, frames: f64| Seconds(clock.time().0 + frames * step);
    clock.observe_external(at(&clock, 1.3), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Skip);
    clock.observe_external(at(&clock, -1.5), Smoothing::Exact);
    clock.advance(1);
    assert_eq!(clock.last_correction(), Correction::Hold);
}

/// External source models. Each produces the target the P3 source code will
/// hand the clock: its position projected to this frame's deadline.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A target every frame with no error.
    Constant(Smoothing),
    /// 24 PPQN ticks with ±1 ms arrival jitter, projected from arrival (D7).
    MidiClock,
    /// Timecode at `hz`, floor-quantised to its frames, sent at a fixed phase,
    /// ±1 ms jitter, +½ timecode frame bias, projected from arrival (D7).
    Timecode { hz: f64, send_phase: f64 },
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
            Source::Timecode { hz, send_phase } => {
                let period = 1.0 / hz;
                let k = ((wall - send_phase * period - 0.001) / period).floor() as i64;
                if k > last_event {
                    last_event = k;
                    let jitter = rng.range(-0.001, 0.001);
                    let send_wall = (k as f64 + send_phase) * period;
                    let position = send_wall + drift * step;
                    let reported = (position / period).floor() * period;
                    let arrival = send_wall + jitter;
                    Some((reported + 0.5 * period + (wall - arrival), Smoothing::Coarse))
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
        let s = applied(*c).signum();
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
                let net: i64 = got.iter().map(|c| applied(*c)).sum();
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
    // Timecode is quantised to its own frames: a fixed send phase leaves a
    // constant bias of up to ½ timecode frame, worth at most one correction.
    // Phases sit off the timecode frame edge; a sender exactly on it is a P3
    // measurement, not a model input.
    for hz in [25.0, 30.0] {
        for send_phase in [0.1, 0.35, 0.6, 0.85] {
            let mut clock = playing_clock(60.0);
            let got = simulate(&mut clock, Source::Timecode { hz, send_phase }, 0.0, 120.0, two_hours, 11);
            let context = format!("timecode {hz} Hz phase {send_phase}");
            assert_no_flap(&got, &context);
            assert!(got.len() <= 1, "{context}: {got:?}");
        }
    }
}

// ── D8: live tempo ───────────────────────────────────────────────────────────

#[test]
fn live_tempo_is_absolute_and_continuous() {
    let clock = TransportClock::new(60.0);
    let mut tempo = LiveTempo::new(120, Beats(4.0), Bpm(120.0));
    let at = |tempo: &LiveTempo, index: i64| tempo.beat_at(&clock, clock.time_at(index)).0;
    assert_eq!(at(&tempo, 120), 4.0);
    assert!((at(&tempo, 150) - 5.0).abs() < 1e-12, "30 frames at 120 BPM is one beat");
    assert!((tempo.time_at(&clock, Beats(5.0)).0 - clock.time_at(150).0).abs() < 1e-12);

    // A ramp: re-anchor every 15 frames, BPM up 0.5 each time. The beat never jumps.
    let mut index = 150;
    for i in 0..64 {
        let before = at(&tempo, index);
        tempo.reanchor(&clock, index, Bpm(120.0 + 0.5 * i as f32));
        assert_eq!(at(&tempo, index).to_bits(), before.to_bits(), "continuous at {index}");
        index += 15;
    }
    assert!(at(&tempo, index) > at(&tempo, index - 1));
}
