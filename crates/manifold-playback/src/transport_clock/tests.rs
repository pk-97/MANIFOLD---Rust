use super::*;
use manifold_core::{Beats, Bpm};
use manifold_core::tempo::{TempoMap, TempoMapConverter};
use manifold_core::types::TempoPointSource;

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

#[test]
fn non_finite_and_absurd_input_is_safe() {
    let mut clock = playing_clock(60.0);
    clock.advance(600);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        clock.locate(Seconds(bad));
        clock.seek_playing(Seconds(bad));
        clock.locate_nearest(Seconds(bad));
        assert_eq!(clock.index(), 600, "{bad} is ignored");
    }
    assert_eq!(clock.advance(1), 1, "non-finite input set no hold");

    for huge in [1e300, -1e300] {
        clock.locate(Seconds(huge));
        assert!(clock.time().0.is_finite());
        clock.advance(u32::MAX);
        clock.advance(u32::MAX);
        clock.seek_playing(Seconds(huge));
        clock.advance(1);
        assert!(clock.time().0.is_finite());
    }

    clock.set_export_frame(i64::MAX, u32::MAX);
    assert!(clock.time().0.is_finite());
    clock.set_export_frame(i64::MIN, 0);
    clock.advance(u32::MAX);
    assert!(clock.time().0.is_finite());
}
