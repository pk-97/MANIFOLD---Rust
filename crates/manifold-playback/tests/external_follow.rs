//! External sources steer the beat by tempo trim, read at the screen instant
//! (FRAME_EXACT_TRANSPORT_DESIGN.md P3, section 5 (Proof plan), P3 proofs).
//!
//! Every test plays a seeded source model through the engine and measures,
//! after a warm-up: phase error against Live's true beat at the frame's
//! screen instant, beat speed against the music's over the same frame, the
//! speed change between frames, frames where the beat went backward outside
//! a seek, and seeks (an index move other than `frames`, or an epoch bump).
//! Bounds are D7's test-bound columns unless a test says otherwise.

mod support;

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, Bpm, Seconds};
use manifold_playback::external_follow::{ExternalFollower, ExternalSample, FollowEvent, FollowProfile};
use manifold_playback::engine::PlaybackEngine;
use manifold_playback::renderer::{ClipRenderer, StubRenderer};
use manifold_playback::sync_calibration::CalibratedSource;
use support::sync_sources::*;

const WARM: f64 = 5.0;

fn assert_bounds(what: &str, m: &Meter, phase_ms: f64, speed: f64, per_frame: f64) {
    let r = m.report();
    assert!(m.max_phase_ms <= phase_ms, "{what}: {r} (phase bound {phase_ms} ms)");
    assert!(m.max_speed <= speed, "{what}: {r} (speed bound {}%)", speed * 100.0);
    assert!(m.max_speed_change <= per_frame, "{what}: {r} (per-frame bound {}%)", per_frame * 100.0);
    assert_eq!(m.backward, 0, "{what}: {r}");
    assert!(m.seeks.is_empty(), "{what}: {r} at {:?}", m.seeks);
}

/// Test 1: Map 120, Link 128: Link's bounds, no seek.
#[test]
fn link_follows_with_steady_beat_speed() {
    let live = Live::new(Tempo::constant(128.0));
    let mut link = LinkReader::new(live.clone(), 11);
    let (m, _) = follow(engine_with_map(120.0, None), &mut link, &live, 600.0, WARM, 1);
    assert_bounds("Link 128 over a 120 map", &m, 1.0, 0.0015, 0.0003);
}

/// Test 2: MIDI clock at 120 and 174, with Link's tempo latched and alone.
#[test]
fn clk_follows_without_stair_steps() {
    for with_link in [false, true] {
        for bpm in [120.0, 174.0] {
            let live = Live::new(Tempo::constant(bpm));
            let mut clk = ClkReader::new(clock_arrivals(&live, 600.0, 21, &[], 0.0));
            if with_link {
                clk = clk.with_link(LinkReader::new(live.clone(), 22));
            }
            let (m, _) = follow(engine_with_map(120.0, None), &mut clk, &live, 600.0, WARM, 2);
            let what = format!("CLK {bpm}, Link {}", if with_link { "latched" } else { "absent" });
            if with_link {
                assert_bounds(&what, &m, 1.0, 0.002, 0.0003);
            } else {
                assert_bounds(&what, &m, 1.5, 0.003, 0.0008);
            }
        }
    }
}

/// Test 3: D20's tempo-change table under MIDI clock, nine seeded runs each.
#[test]
fn clk_tempo_change_follows_through_the_estimate() {
    // (label, tempo, bound with Link latched, bound alone), all ms.
    let cases = [
        ("120 -> 140", Tempo::constant(120.0).jump(10.0, 140.0), 8.0, 25.0),
        ("128 -> 174", Tempo::constant(128.0).jump(10.0, 174.0), 8.0, 35.0),
        ("174 -> 128", Tempo::constant(174.0).jump(10.0, 128.0), 8.0, 55.0),
        ("140 -> 70", Tempo::constant(140.0).jump(10.0, 70.0), 20.0, 240.0),
        ("128 -> 120", Tempo::constant(128.0).jump(10.0, 120.0), 8.0, 20.0),
        ("ramp 120 -> 128 over 16 beats", Tempo::constant(120.0).ramp(10.0, 7.75, 128.0), 8.0, 20.0),
        ("ramp 120 -> 174 over 8 s", Tempo::constant(120.0).ramp(10.0, 8.0, 174.0), 8.0, 20.0),
        ("ramp 174 -> 120 over 4 s", Tempo::constant(174.0).ramp(10.0, 4.0, 120.0), 8.0, 20.0),
    ];
    for with_link in [false, true] {
        for (label, tempo, latched_bound, alone_bound) in cases.iter().cloned() {
            for seed in 0..9u64 {
                let live = Live::new(tempo.clone());
                let mut clk = ClkReader::new(clock_arrivals(&live, 30.0, 100 + seed, &[], 0.0));
                if with_link {
                    clk = clk.with_link(LinkReader::new(live.clone(), 200 + seed));
                }
                let (m, _) = follow(engine_with_map(120.0, None), &mut clk, &live, 30.0, WARM, seed);
                let bound = if with_link { latched_bound } else { alone_bound };
                let what = format!("{label}, seed {seed}, Link {}", if with_link { "latched" } else { "absent" });
                assert!(m.max_phase_ms <= bound, "{what}: {} (bound {bound} ms)", m.report());
                assert_eq!((m.backward, m.seeks.len()), (0, 0), "{what}: {}", m.report());
            }
        }
    }
}

/// Settle time under 1 ms after a relocation at `at`, and the overshoot.
fn settle(live: &Live, mut reader: impl Reader, at: f64, secs: f64, sign: f64) -> (f64, f64, Meter) {
    let mut driver = Driver::new(engine_with_map(120.0, None), 4);
    set_authority(&mut driver.engine, Some(reader.kind()));
    driver.engine.play();
    let mut meter = Meter::default();
    let (mut last_over, mut overshoot) = (at, 0.0f64);
    let mut before_jump = true;
    while driver.wall() < secs {
        let shown = driver.step(Some(&mut reader), 0);
        let truth = live.beat(shown.deadline);
        if before_jump && shown.deadline >= at {
            before_jump = false;
            meter.music_jumped();
        }
        meter.observe(shown, truth, live.bpm(shown.deadline), shown.deadline >= WARM);
        if shown.deadline >= at {
            let err_ms = (truth - shown.beat) * 60.0 / live.bpm(shown.deadline) * 1000.0;
            if err_ms.abs() >= 1.0 {
                last_over = shown.deadline;
            }
            overshoot = overshoot.max(-err_ms * sign);
        }
    }
    (last_over - at, overshoot, meter)
}

/// Test 4: Link source steps: trimmed, no seek, settled within D6's times.
#[test]
fn phase_step_settles_by_trim() {
    for (step_ms, within) in [(5.0, 1.0), (10.0, 2.2), (20.0, 4.6), (-20.0, 4.7), (40.0, 9.3)] {
        let live = Live::new(Tempo::constant(120.0)).relocate(20.0, step_ms / 1000.0);
        let (settled, overshoot, m) = settle(&live, LinkReader::new(live.clone(), 41), 20.0, 40.0, f64::signum(step_ms));
        let what = format!("Link step {step_ms} ms");
        assert!(m.seeks.is_empty() && m.backward == 0, "{what}: {}", m.report());
        assert!(settled <= within, "{what}: under 1 ms after {settled:.2} s (bound {within} s)");
        assert!(overshoot < 0.6, "{what}: overshoot {overshoot:.3} ms");
        assert!(m.max_speed <= 0.006, "{what}: {}", m.report());
    }
}

/// Test 5: A lasting relocation seeks once after the row's hold; a small one is trimmed.
#[test]
fn relocate_seeks_once() {
    for (kind, hold, slack) in [(Kind::Link, 0.1, 2.0 * STEP), (Kind::Osc, 0.25, 0.15)] {
        for step_ms in [300.0, -500.0, 40.0] {
            let live = Live::new(Tempo::constant(120.0)).relocate(20.0, step_ms / 1000.0);
            let mut reader: Box<dyn Reader> = match kind {
                Kind::Link => Box::new(LinkReader::new(live.clone(), 51)),
                _ => Box::new(OscReader::new(osc_arrivals(&live, 40.0, 52, 0.0))),
            };
            let mut driver = Driver::new(engine_with_map(120.0, None), 5);
            set_authority(&mut driver.engine, Some(kind));
            driver.engine.play();
            let mut seeks = Vec::new();
            let mut epochs = Vec::new();
            let mut last_epoch = None;
            while driver.wall() < 40.0 {
                let shown = driver.step(Some(reader.as_mut()), 0);
                if shown.deadline > WARM && shown.jumped {
                    seeks.push(shown.deadline);
                }
                if shown.deadline > WARM && last_epoch.is_some_and(|e| e != shown.epoch) {
                    epochs.push(shown.deadline);
                }
                last_epoch = Some(shown.epoch);
            }
            let what = format!("{kind:?} relocated {step_ms} ms");
            if step_ms.abs() > 50.0 {
                assert_eq!(seeks.len(), 1, "{what}: seeks at {seeks:?}");
                let after = seeks[0] - 20.0;
                assert!(after >= hold - 1e-9 && after <= hold + slack, "{what}: sought {after:.3} s after the move");
                assert_eq!(epochs.len(), 1, "{what}: the seek bumps the epoch once ({epochs:?})");
            } else {
                assert!(seeks.is_empty(), "{what}: trimmed, but sought at {seeks:?}");
            }
        }
    }
}

/// Test 6: AbletonOSC, Live 128 over a 120 map, listener lag 100 ms, 10 minutes.
#[test]
fn abletonosc_fallback_stays_within_bound() {
    let live = Live::new(Tempo::constant(128.0));
    let mut osc = OscReader::new(osc_arrivals(&live, 600.0, 61, 0.1));
    let (m, _) = follow(engine_with_map(120.0, None), &mut osc, &live, 600.0, WARM, 6);
    assert_bounds("AbletonOSC 128 over a 120 map", &m, 8.0, 0.006, 0.0005);
}

/// Test 7: Timecode on six metros and three start phases: unquantised seconds
/// within the unquantised bounds, LiveMTC 25 fps labels within 21 ms.
#[test]
fn timecode_follows_song_seconds() {
    for metro in [0.040, 0.0401, 0.0333, 0.0334, 0.020, 0.050] {
        for phase in [0.0, 0.013, 0.027] {
            for labels in [false, true] {
                let live = Live::new(Tempo::constant(120.0));
                let mut tc = TimecodeReader::new(timecode_arrivals(&live, 120.0, 71, metro, phase, labels), 120.0);
                let (m, _) = follow(engine_with_map(120.0, None), &mut tc, &live, 120.0, WARM, 7);
                let what = format!("timecode metro {metro}, phase {phase}, labels {labels}");
                if labels {
                    assert!(m.max_phase_ms <= 21.0, "{what}: {}", m.report());
                    assert!(m.seeks.is_empty() && m.backward == 0, "{what}: {}", m.report());
                } else {
                    assert_bounds(&what, &m, 2.0, 0.001, 0.0001);
                }
            }
        }
    }
}

/// Play `first` until `switch`, then `second` (None: Internal) until `end`.
/// Returns the beat increments around the switch and the meter after it.
fn hand_over(live: &Live, mut first: Option<Box<dyn Reader>>, mut second: Option<Box<dyn Reader>>, switch: f64, end: f64) -> (f64, f64, Meter) {
    let mut driver = Driver::new(engine_with_map(120.0, None), 8);
    set_authority(&mut driver.engine, first.as_ref().map(|r| r.kind()));
    driver.engine.play();
    let mut meter = Meter::default();
    let (mut prev, mut before_inc, mut at_inc) = (None::<Shown>, f64::NAN, f64::NAN);
    let mut switched = false;
    while driver.wall() < end {
        if !switched && driver.wall() >= switch {
            set_authority(&mut driver.engine, second.as_ref().map(|r| r.kind()));
            switched = true;
        }
        let source = if switched { &mut second } else { &mut first };
        let shown = driver.step(source.as_mut().map(|r| &mut **r as &mut dyn Reader), 0);
        if let Some(p) = prev {
            let inc = (shown.beat - p.beat) / f64::from(shown.frames);
            if !switched {
                before_inc = inc;
            } else if at_inc.is_nan() {
                at_inc = inc;
            }
        }
        meter.observe(shown, live.beat(shown.deadline), live.bpm(shown.deadline), shown.deadline >= switch);
        prev = Some(shown);
    }
    (before_inc, at_inc, meter)
}

/// Test 8: Handovers between sources keep the beat continuous.
#[test]
fn handover_keeps_the_beat_continuous() {
    // MIDI clock stops mid-play and AbletonOSC takes over (Live 128, map 120).
    let live = Live::new(Tempo::constant(128.0));
    let clk = ClkReader::new(clock_arrivals(&live, 30.0, 81, &[], 0.0));
    let osc = OscReader::new(osc_arrivals(&live, 90.0, 82, 0.1));
    let (before, at, m) = hand_over(&live, Some(Box::new(clk)), Some(Box::new(osc)), 30.0, 90.0);
    assert!((at / before - 1.0).abs() <= 0.006, "CLK -> AbletonOSC: increment {before} -> {at}");
    assert_bounds("after CLK -> AbletonOSC", &m, 8.0, 0.006, 0.0005);

    // Link to MIDI clock, both from the same Live.
    let live = Live::new(Tempo::constant(128.0));
    let link = LinkReader::new(live.clone(), 83);
    let clk = ClkReader::new(clock_arrivals(&live, 90.0, 84, &[], 0.0)).with_link(LinkReader::new(live.clone(), 85));
    let (before, at, m) = hand_over(&live, Some(Box::new(link)), Some(Box::new(clk)), 30.0, 90.0);
    assert!((at / before - 1.0).abs() <= 0.006, "Link -> CLK: increment {before} -> {at}");
    assert_bounds("after Link -> CLK", &m, 1.0, 0.002, 0.0003);

    // Internal to Link, agreeing: Live at the map's tempo, on the same beat.
    let live = Live::new(Tempo::constant(120.0));
    let link = LinkReader::new(live.clone(), 86);
    let (before, at, m) = hand_over(&live, None, Some(Box::new(link)), 30.0, 90.0);
    assert!((at / before - 1.0).abs() <= 0.006, "Internal -> Link: increment {before} -> {at}");
    assert!(m.seeks.is_empty() && m.backward == 0, "Internal -> Link: {}", m.report());
}

/// Test 11: Link at 128 over a 120 map hands back to Internal: neither time nor
/// the beat moves, no temporal reset; the beat then runs at the map's tempo,
/// and the next Play keeps the beat. Under Link, every tick advanced the
/// index by exactly its frames.
#[test]
fn hand_back_keeps_time_and_beat() {
    let live = Live::new(Tempo::constant(128.0));
    let mut link = LinkReader::new(live.clone(), 111);
    let mut driver = Driver::new(engine_with_map(120.0, None), 11);
    set_authority(&mut driver.engine, Some(Kind::Link));
    driver.engine.play();
    let mut prev: Option<Shown> = None;
    let mut index_jumps = 0;
    while driver.wall() < 60.0 {
        let shown = driver.step(Some(&mut link), 0);
        if shown.deadline > WARM && shown.jumped {
            index_jumps += 1;
        }
        prev = Some(shown);
    }
    assert_eq!(index_jumps, 0, "under Link the index advances by exactly its frames");
    let last = prev.expect("ticked");
    set_authority(&mut driver.engine, None);
    let back = driver.step(None, 0);
    assert!(!back.jumped, "hand-back moved the index or bumped the epoch");
    assert_eq!(back.time, last.time + f64::from(back.frames) * STEP, "time is kept");
    let inc = back.beat - last.beat;
    assert!(inc > 0.0 && inc <= f64::from(back.frames) * 128.0 / 60.0 * STEP * 1.01, "the beat is kept: {} -> {}", last.beat, back.beat);
    let mut p = back;
    for _ in 0..300 {
        let s = driver.step(None, 0);
        assert!(!s.jumped);
        let rate = (s.beat - p.beat) / (f64::from(s.frames) * STEP) * 60.0;
        assert!((rate - 120.0).abs() < 1e-6, "after hand-back the beat runs at the map's 120, not {rate}");
        p = s;
    }
    driver.engine.pause();
    let paused = driver.step(None, 0);
    driver.engine.play();
    let resumed = driver.step(None, 0);
    assert!((resumed.beat - paused.beat).abs() <= 2.0 * STEP * 2.0, "Play keeps the beat: {} -> {}", paused.beat, resumed.beat);
}

fn video_project(starts: &[f64]) -> Project {
    let mut project = Project::default();
    for (i, &start) in starts.iter().enumerate() {
        let mut layer = Layer::new(format!("V{i}"), LayerType::Video, i as i32);
        layer.clips.push(TimelineClip::new_video(format!("video-{i}"), Beats(start), Beats(5000.0), manifold_core::Seconds::ZERO));
        project.timeline.layers.push(layer);
    }
    project.timeline.reindex_layers();
    project
}

/// Test 12: Non-warped video elapsed time follows real elapsed time through a Link
/// tempo change: one clip started 120 s before it, one 30 s before.
#[test]
fn video_elapsed_survives_tempo_changes() {
    let change = 124.0;
    let starts = [8.0, (change - 30.0) * 2.0];
    let live = Live::new(Tempo::constant(120.0).jump(change, 140.0));
    let mut link = LinkReader::new(live.clone(), 121);
    let mut driver = Driver::new(engine_with_map(120.0, Some(video_project(&starts))), 12);
    set_authority(&mut driver.engine, Some(Kind::Link));
    driver.engine.play();
    let mut since: [Option<u32>; 2] = [None, None];
    let mut worst = [0.0f64; 2];
    while driver.wall() < change + 60.0 {
        let shown = driver.step(Some(&mut link), 0);
        for (i, start) in starts.iter().enumerate() {
            if shown.beat >= *start {
                let frames = since[i].get_or_insert(0);
                *frames += shown.frames;
                let clip = driver.engine.project().expect("project").timeline.layers[i].clips[0].clone();
                let elapsed = driver.engine.get_clip_source_elapsed_seconds(&clip).0;
                let truth = f64::from(*frames - 1) * STEP;
                if shown.deadline > change - 1.0 {
                    worst[i] = worst[i].max((elapsed - truth).abs());
                }
            }
        }
    }
    for (i, w) in worst.iter().enumerate() {
        assert!(*w <= STEP, "clip {i}: video elapsed off by {:.1} ms through the tempo change", w * 1000.0);
    }

    // Ten minutes under LiveMTC timecode labels: the start is latched on the
    // activation frame, sub-frame (D8), so the offset from whole frames is
    // that frame's phase error, inside the label bound (test 7); after it
    // the elapsed time drifts by no more than 1 ms.
    let live = Live::new(Tempo::constant(120.0));
    let mut tc = TimecodeReader::new(timecode_arrivals(&live, 600.0, 122, 0.040, 0.0, true), 120.0);
    let mut driver = Driver::new(engine_with_map(120.0, Some(video_project(&[8.0]))), 13);
    set_authority(&mut driver.engine, Some(Kind::Timecode));
    driver.engine.play();
    let (mut frames, mut first, mut drift) = (None::<u32>, None::<f64>, 0.0f64);
    while driver.wall() < 600.0 {
        let shown = driver.step(Some(&mut tc), 0);
        if shown.beat >= 8.0 {
            let f = frames.get_or_insert(0);
            *f += shown.frames;
            let clip = driver.engine.project().expect("project").timeline.layers[0].clips[0].clone();
            let elapsed = driver.engine.get_clip_source_elapsed_seconds(&clip).0;
            let offset = elapsed - f64::from(*f - 1) * STEP;
            let first = *first.get_or_insert(offset);
            drift = drift.max((offset - first).abs());
        }
    }
    let first = first.expect("the clip started");
    assert!(first.abs() <= 0.021, "timecode: activation offset {:.2} ms", first * 1000.0);
    assert!(drift <= 0.001, "timecode: video elapsed drifted by {:.2} ms", drift * 1000.0);
}

/// Test 13: With Live on Link, a tempo jump under MIDI clock stays within the
/// latched bound; a Link session Live is not part of never causes a seek.
#[test]
fn guard_latches_and_holds_through_tempo_changes() {
    let live = Live::new(Tempo::constant(128.0).jump(20.0, 174.0));
    let mut clk = ClkReader::new(clock_arrivals(&live, 40.0, 131, &[], 0.0)).with_link(LinkReader::new(live.clone(), 132));
    let (m, _) = follow(engine_with_map(120.0, None), &mut clk, &live, 40.0, WARM, 13);
    assert!(m.max_phase_ms <= 8.0 && m.seeks.is_empty() && m.backward == 0, "Live on Link, 128 -> 174: {}", m.report());

    for foreign in [127.0, 127.8] {
        let live = Live::new(Tempo::constant(128.0));
        let session = Live::new(Tempo::constant(foreign));
        let mut clk = ClkReader::new(clock_arrivals(&live, 180.0, 133, &[], 0.0)).with_link(LinkReader::new(session, 134));
        let (m, _) = follow(engine_with_map(120.0, None), &mut clk, &live, 180.0, WARM, 14);
        // The plan's bound: no seek in 3 minutes. 127.8 is 0.16% off, inside
        // the guard's 0.2%, so it latches and runs on its period; the trim
        // holds the gap.
        assert!(m.seeks.is_empty() && m.backward == 0, "foreign Link session at {foreign}: {}", m.report());
    }
}

/// Test 14: MIDI clock alone at 120 with delivery stalls released as a burst.
#[test]
fn clock_stall_then_burst_does_not_diverge() {
    let live = Live::new(Tempo::constant(120.0));
    let stalls = [(20.0, 0.15), (40.0, 0.2), (60.0, 0.3), (80.0, 0.6)];
    let mut clk = ClkReader::new(clock_arrivals(&live, 100.0, 141, &stalls, 0.0));
    let (m, _) = follow(engine_with_map(120.0, None), &mut clk, &live, 100.0, WARM, 15);
    assert!(m.max_phase_ms <= 30.0, "{}", m.report());
    assert!(m.seeks.is_empty() && m.backward == 0, "{}", m.report());
}

/// Test 15: AbletonOSC tempo changes with the listener 0, 100 and 200 ms late.
#[test]
fn abletonosc_tempo_change_with_listener_lag() {
    for (from, to) in [(174.0, 128.0), (128.0, 174.0), (120.0, 140.0), (140.0, 70.0)] {
        for lag in [0.0, 0.1, 0.2] {
            let live = Live::new(Tempo::constant(from).jump(20.0, to));
            let mut osc = OscReader::new(osc_arrivals(&live, 40.0, 151, lag));
            let (m, _) = follow(engine_with_map(120.0, None), &mut osc, &live, 40.0, WARM, 16);
            let what = format!("AbletonOSC {from} -> {to}, listener lag {lag}");
            assert_eq!(m.backward, 0, "{what}: {}", m.report());
            // No seek up to 100 ms of lag. At 200 ms a halving may seek once
            // by D6's catch-up rule (past 150 ms for 250 ms); no bound is set.
            assert!(lag > 0.1 || m.seeks.is_empty(), "{what}: {}", m.report());
            let bound = if lag == 0.0 {
                8.0
            } else if lag <= 0.1 {
                if from == 140.0 { 230.0 } else { 80.0 }
            } else {
                f64::INFINITY
            };
            assert!(m.max_phase_ms <= bound, "{what}: {} (bound {bound} ms)", m.report());
            if lag > 0.0 && lag <= 0.1 {
                let (late, _, _) = settle_after(&live, OscReader::new(osc_arrivals(&live, 40.0, 151, lag)), 20.0);
                assert!(late <= 3.0, "{what}: back within a frame after {late:.2} s");
            }
        }
    }
}

/// Seconds after `at` until the error stays within one frame.
fn settle_after(live: &Live, mut reader: impl Reader, at: f64) -> (f64, f64, Meter) {
    let mut driver = Driver::new(engine_with_map(120.0, None), 17);
    set_authority(&mut driver.engine, Some(reader.kind()));
    driver.engine.play();
    let mut meter = Meter::default();
    let mut last_over = at;
    while driver.wall() < 40.0 {
        let shown = driver.step(Some(&mut reader), 0);
        let truth = live.beat(shown.deadline);
        meter.observe(shown, truth, live.bpm(shown.deadline), shown.deadline >= WARM);
        if shown.deadline >= at && ((truth - shown.beat) * 60.0 / live.bpm(shown.deadline)).abs() > STEP {
            last_over = shown.deadline;
        }
    }
    (last_over - at, 0.0, meter)
}

/// Test 16: The read instant follows the measured screen delay; a source's offset
/// against Link is learned; the hidden extra delay adds on.
#[test]
fn read_instant_follows_the_measured_latency() {
    // Frames reach the screen 25 ms after their deadline (±3 ms): the read
    // instant leaves the 17 ms default and settles on 25 ms within 3 s.
    let live = Live::new(Tempo::constant(128.0));
    let mut link = LinkReader::new(live.clone(), 161);
    let mut driver = Driver::new(engine_with_map(120.0, None), 18);
    driver.screen_delay = 0.025;
    driver.present_jitter = 0.003;
    let (m, read) = run_measured(&mut driver, &mut link, &live, 30.0, 6.0);
    assert!(read.settled_by <= 3.0 && (read.last - 0.025).abs() <= 0.001, "read instant settles on 25 ms: {read:?}");
    assert!(m.max_phase_ms <= 1.0 && m.seeks.is_empty(), "read at the measured screen instant: {}", m.report());

    // MIDI clock arrives 7 ms late against Link: the learned offset closes it.
    let live = Live::new(Tempo::constant(128.0));
    let mut clk = ClkReader::new(clock_arrivals(&live, 120.0, 162, &[], 0.007)).with_link(LinkReader::new(live.clone(), 163));
    let mut driver = Driver::new(engine_with_map(120.0, None), 19);
    driver.screen_delay = 0.025;
    driver.present_jitter = 0.003;
    let (m, _) = run_measured(&mut driver, &mut clk, &live, 120.0, 40.0);
    let offset = driver.calibration.offset(CalibratedSource::MidiClock);
    assert!((offset - 0.007).abs() <= 0.001, "CLK offset learned: {:.2} ms", offset * 1000.0);
    assert!(m.max_phase_ms <= 1.0 && m.seeks.is_empty(), "CLK 7 ms late, offset learned: {}", m.report());

    // The display adds 10 ms nobody reports; the hidden override covers it.
    let live = Live::new(Tempo::constant(128.0));
    let mut link = LinkReader::new(live.clone(), 164);
    let mut driver = Driver::new(engine_with_map(120.0, None), 20);
    driver.screen_delay = 0.035;
    driver.present_jitter = 0.003;
    driver.extra_delay = 0.010;
    driver.calibration.set_extra_delay_ms(10.0);
    let (m, read) = run_measured(&mut driver, &mut link, &live, 30.0, 6.0);
    assert!(read.settled_by <= 3.0 && (read.last - 0.035).abs() <= 0.001, "measured 25 ms plus 10 ms extra: {read:?}");
    assert!(m.max_phase_ms <= 1.0 && m.seeks.is_empty(), "read at 35 ms: {}", m.report());
}

/// The read delay's last value and the last wall second it moved.
#[derive(Debug, Default)]
struct ReadDelay {
    last: f64,
    settled_by: f64,
}

fn run_measured(driver: &mut Driver, reader: &mut dyn Reader, live: &Live, secs: f64, warm: f64) -> (Meter, ReadDelay) {
    set_authority(&mut driver.engine, Some(reader.kind()));
    driver.engine.play();
    let mut meter = Meter::default();
    let mut read = ReadDelay { last: driver.calibration.read_delay(), settled_by: 0.0 };
    while driver.wall() < secs {
        let shown = driver.step(Some(reader), 0);
        let delay = driver.calibration.read_delay();
        if delay != read.last {
            read = ReadDelay { last: delay, settled_by: shown.deadline };
        }
        let r = shown.deadline + driver.screen_delay;
        meter.observe(shown, live.beat(r), live.bpm(r), shown.deadline >= warm);
    }
    (meter, read)
}

/// A source for one hour: name, reader, its music, profile, phase bound in ms.
struct HourRun<'a>(&'static str, Box<dyn Reader>, &'a Live, FollowProfile, f64);

/// One-hour runs, follower only (section 5 (Proof plan)): each source model
/// drives the real filter, guard and projections into the real follower, no
/// engine, for one simulated hour at 60 fps at a steady tempo. Zero seeks,
/// zero catch-up opens after warm-up, D7's bounds throughout.
#[test]
fn no_false_seeks_or_catch_up_in_an_hour() {
    const HOUR: f64 = 3600.0;
    let live = Live::new(Tempo::constant(128.0));
    let timecode_live = Live::new(Tempo::constant(120.0));
    let runs: [HourRun; 5] = [
        HourRun("Link", Box::new(LinkReader::new(live.clone(), 901)), &live, FollowProfile::FAST, 1.0),
        HourRun("CLK alone", Box::new(ClkReader::new(clock_arrivals(&live, HOUR, 902, &[], 0.0))), &live, FollowProfile::FAST, 1.5),
        HourRun(
            "CLK, Link latched",
            Box::new(ClkReader::new(clock_arrivals(&live, HOUR, 903, &[], 0.0)).with_link(LinkReader::new(live.clone(), 904))),
            &live,
            FollowProfile::FAST,
            1.0,
        ),
        HourRun("AbletonOSC", Box::new(OscReader::new(osc_arrivals(&live, HOUR, 905, 0.1))), &live, FollowProfile::SLOW, 8.0),
        HourRun(
            "timecode",
            Box::new(TimecodeReader::new(timecode_arrivals(&timecode_live, HOUR, 906, 0.040, 0.013, false), 120.0)),
            &timecode_live,
            FollowProfile::SLOW,
            2.0,
        ),
    ];
    for HourRun(what, mut reader, live, profile, bound_ms) in runs {
        let mut follower = ExternalFollower::starting_at(profile, Seconds(0.0), Beats(0.0), Bpm(120.0));
        follower.reseed();
        let mut rng = Rng(907);
        let (mut seeks, mut opens, mut max_phase_ms, mut was_open) = (0u32, 0u32, 0.0f64, false);
        let ticks = (HOUR / STEP) as u64;
        for tick in 1..=ticks {
            // The shown frame's deadline is its grid time; the tick wakes
            // up to 40% of a frame later.
            let deadline = tick as f64 * STEP;
            let wake = deadline + rng.range(0.0, 0.4) * STEP;
            let t = Seconds(deadline);
            if let Some(s) = reader.read(deadline, wake) {
                let sample = ExternalSample { beat: s.beat, bpm: s.bpm, fresh: s.fresh, exact: s.exact };
                if let FollowEvent::Seek { beat, .. } = follower.observe(t, sample) {
                    seeks += 1;
                    follower.seek_to(t, beat, Bpm(s.bpm as f32));
                }
            }
            if deadline < WARM {
                continue;
            }
            let open = follower.catch_up_open();
            opens += u32::from(open && !was_open);
            was_open = open;
            let phase = (live.beat(deadline) - follower.beat_at(t).0) * 60.0 / live.bpm(deadline);
            max_phase_ms = max_phase_ms.max(phase.abs() * 1000.0);
        }
        let report = format!("{what}: phase {max_phase_ms:.2} ms (bound {bound_ms}), seeks {seeks}, catch-up opens {opens}");
        assert_eq!((seeks, opens), (0, 0), "{report}");
        assert!(max_phase_ms <= bound_ms, "{report}");
    }
}

/// Test 17: A seed that moves the beat is a placement (D8): timecode 0.3 s
/// off the parked position, Link-started play with MANIFOLD parked mid-bar,
/// MIDI clock Continue mid-song. Each active clip's video time matches its
/// beat, and no clip stops and restarts in one tick.
#[test]
fn a_seed_that_moves_the_beat_relatches_clips() {
    let starts = [4.0, 17.0];
    // Live 0.3 s ahead of MANIFOLD parked at 8 s.
    let tc_live = Live { start: -8.3, ..Live::new(Tempo::constant(120.0)) };
    // Live on Link at beat 16, a bar line; MANIFOLD parked mid-bar at beat 18.
    let link_live = Live { start: -16.0 * 60.0 / 128.0, ..Live::new(Tempo::constant(128.0)) };
    // Live continues from beat 32, where MANIFOLD is parked.
    let clk_live = Live { start: -16.0, ..Live::new(Tempo::constant(120.0)) };
    let cases: [(&str, Box<dyn Reader>, &Live, f64); 3] = [
        ("timecode 0.3 s ahead", Box::new(TimecodeReader::new(timecode_arrivals(&tc_live, 5.0, 171, 0.040, 0.0, false), 120.0)), &tc_live, 8.0),
        ("Link-started play, parked mid-bar", Box::new(LinkReader::new(link_live.clone(), 172)), &link_live, 9.0),
        ("CLK Continue mid-song", Box::new(ClkReader::new(continue_arrivals(&clk_live, 5.0, 173, 32.0))), &clk_live, 16.0),
    ];
    let mut failures = Vec::new();
    for (what, mut reader, live, parked) in cases {
        let mut driver = Driver::new(engine_with_map(120.0, Some(video_project(&starts))), 17);
        set_authority(&mut driver.engine, Some(reader.kind()));
        driver.engine.seek_to(Seconds(parked));
        driver.engine.play();
        let ids: [String; 2] = [0, 1].map(|i| driver.engine.project().expect("project").timeline.layers[i].clips[0].id.to_string());
        let mut counts = clip_counts(&driver.engine, &ids);
        let (mut worst, mut measured) = (0.0f64, 0u32);
        while driver.wall() < 2.0 {
            let shown = driver.step(Some(reader.as_mut()), 0);
            let now = clip_counts(&driver.engine, &ids);
            for (i, (before, after)) in counts.iter().zip(&now).enumerate() {
                if after.0 > before.0 && after.1 > before.1 {
                    failures.push(format!("{what}: clip {i} stopped and restarted in one tick at {:.3} s", shown.deadline));
                }
            }
            counts = now;
            let bpm = live.bpm(shown.deadline);
            for (i, start) in starts.iter().enumerate() {
                if !stub_video(&driver.engine).is_active(&ids[i]) {
                    continue;
                }
                let clip = driver.engine.project().expect("project").timeline.layers[i].clips[0].clone();
                let elapsed = driver.engine.get_clip_source_elapsed_seconds(&clip).0;
                let truth = (shown.beat - start) * 60.0 / bpm;
                worst = worst.max((elapsed - truth).abs());
                measured += 1;
            }
        }
        if measured == 0 {
            failures.push(format!("{what}: no clip was active"));
        }
        if worst > STEP / 2.0 {
            failures.push(format!("{what}: video time off its beat by {:.1} ms", worst * 1000.0));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// (starts, stops) per clip on the video stub.
fn clip_counts(engine: &PlaybackEngine, ids: &[String; 2]) -> [(usize, usize); 2] {
    let stub = stub_video(engine);
    [0, 1].map(|i| (stub.start_count_for(&ids[i]), stub.stop_count_for(&ids[i])))
}

fn stub_video(engine: &PlaybackEngine) -> &StubRenderer {
    engine.renderers()[1].as_any().downcast_ref::<StubRenderer>().expect("renderer 1 is the video stub")
}
