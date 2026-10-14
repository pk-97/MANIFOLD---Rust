//! Frame-exact transport through the real content thread
//! (docs/FRAME_EXACT_TRANSPORT_DESIGN.md section 5, BUG-gqhep
//! (live-transport-nondeterministic-frame-clock)).
//!
//! The headless `ContentThread` with its real timer, not `frame_clocked`,
//! driven through `tick_frame_at` with injected instants: a random Play-press
//! phase, ticks jittered ±40% of a frame and one 3-frame stall. Per transport
//! frame the engine's time, beat, clip membership and every param must match
//! a steady run bit for bit; pixels are not compared. Opens the test GPU
//! device, so it runs as one queued GPU run. Committed red while each tick
//! handed the engine measured wall time; green once the timer counts grid
//! frames and time comes from the frame index.
#![cfg(all(test, target_os = "macos"))]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use manifold_core::clip::TimelineClip;
use manifold_core::effects::{AutomationLane, AutomationPoint, ParameterDriver, SegmentShape};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform, TempoPointSource};
use manifold_core::{Beats, Bpm, PresetTypeId};
use manifold_playback::frame_grid::FrameGrid;

use crate::content_command::ContentCommand;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

const FPS: f32 = 60.0;
const PRE_ROLL_TICKS: usize = 3;
/// 16 s at 60 fps: past the 128 → 141 change at beat 32 (frame 867).
const PLAYING_TICKS: usize = 960;
const STALL_AT_TICK: usize = 500;
const SEED: u64 = 0x5EED_A9B1_44D3;
/// ms from a frame's deadline to its present, for the stand-in handler.
const SCREEN_DELAY_MS: u64 = 25;

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn point(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
    AutomationPoint { beat: Beats(beat), value, shape }
}

/// One Plasma layer. 150 BPM to beat 8 (frame 192), 128 to beat 32 (frame
/// 867), then 141. A 1/16 sync LFO, a free LFO and a curve-and-step
/// automation lane on three of Plasma's own continuous params; clip edges on
/// frames at 150 BPM and off the grid after.
fn plasma_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(150.0);
    project.settings.frame_rate = FPS;
    for (beat, bpm) in [(0.0, 150.0), (8.0, 128.0), (32.0, 141.0)] {
        project.tempo_map.add_or_replace_point(Beats(beat), Bpm(bpm), TempoPointSource::Manual, 0.001);
    }
    let mut layer = Layer::new_generator("Plasma frame-exact".into(), PresetTypeId::PLASMA, 0);
    let gp = layer.gen_params_mut().expect("generator params");
    let ids: Vec<String> = gp
        .params
        .iter()
        .filter(|p| !p.spec.is_toggle && !p.spec.is_trigger && !p.spec.whole_numbers && p.spec.max > p.spec.min)
        .map(|p| p.spec.id.clone())
        .take(3)
        .collect();
    assert_eq!(ids.len(), 3, "Plasma must expose three continuous params: {ids:?}");
    let mut free = ParameterDriver::new(ids[1].clone(), BeatDivision::Quarter, DriverWaveform::Sine);
    free.free_period_beats = Some(1.37);
    gp.drivers = Some(vec![
        ParameterDriver::new(ids[0].clone(), BeatDivision::Sixteenth, DriverWaveform::Sine),
        free,
    ]);
    gp.automation_lanes = Some(vec![AutomationLane {
        param_id: ids[2].clone().into(),
        enabled: true,
        points: vec![
            point(0.0, 0.1, SegmentShape::Curved(0.6)),
            point(1.0, 0.8, SegmentShape::Hold),
            point(1.5, 0.3, SegmentShape::Hold),
            point(2.25, 0.6, SegmentShape::Linear),
            point(8.0, 0.2, SegmentShape::Hold),
            point(12.0, 0.9, SegmentShape::Curved(-0.5)),
            point(20.0, 0.4, SegmentShape::Hold),
            point(32.0, 0.7, SegmentShape::Linear),
        ],
    }]);
    let mut spans: Vec<(f64, f64)> = (0..16).map(|i| (i as f64 * 0.5, 0.375)).collect();
    spans.extend([8.3, 9.17, 11.05, 13.333, 17.71, 21.9, 26.41, 30.07, 32.0, 33.13].map(|s| (s, 0.6)));
    layer.clips = spans
        .into_iter()
        .map(|(start, length)| TimelineClip::new_generator(Beats(start), Beats(length)))
        .collect();
    project.timeline.layers.push(layer);
    project
}

#[derive(Debug, Clone, PartialEq)]
struct Frame {
    time_bits: u64,
    beat_bits: u64,
    active_clips: usize,
    param_bits: Vec<u32>,
}

fn capture(ct: &ContentThread) -> Frame {
    let project = ct.engine.project().expect("project");
    Frame {
        time_bits: ct.engine.current_time_double().to_bits(),
        beat_bits: ct.engine.current_beat_f64().to_bits(),
        active_clips: ct.engine.active_clip_count(),
        param_bits: project
            .timeline
            .layers
            .iter()
            .filter_map(|l| l.gen_params())
            .flat_map(|gp| gp.params.iter().map(|p| p.value.to_bits()))
            .collect(),
    }
}

enum Event {
    Tick(Instant),
    Press(Instant),
}

impl Event {
    fn at(&self) -> Instant {
        match self {
            Event::Tick(at) | Event::Press(at) => *at,
        }
    }
}

fn offset(base: Instant, seconds: f64) -> Instant {
    if seconds >= 0.0 { base + Duration::from_secs_f64(seconds) } else { base - Duration::from_secs_f64(-seconds) }
}

/// Without a seed every tick sits on its deadline and Play follows the last
/// stopped tick. With one: ±40% jitter, a random press phase, one tick 3
/// frames late.
fn schedule(anchor: Instant, seed: Option<u64>) -> Vec<Event> {
    let grid = FrameGrid::new(FPS, anchor);
    let step = 1.0 / f64::from(FPS);
    let mut rng = seed.map(Rng);
    let mut jitter = || rng.as_mut().map_or(0.0, |r| (r.unit() * 0.8 - 0.4) * step);
    let mut events = Vec::new();
    let mut deadline = 0i64;
    for _ in 0..PRE_ROLL_TICKS {
        deadline += 1;
        events.push(Event::Tick(offset(grid.deadline(deadline), jitter())));
    }
    let phase = seed.map_or(0.0, |s| Rng(s ^ 0xA5A5).unit() * step);
    events.push(Event::Press(offset(grid.deadline(deadline), phase)));
    for tick in 0..PLAYING_TICKS {
        deadline += if seed.is_some() && tick == STALL_AT_TICK { 4 } else { 1 };
        events.push(Event::Tick(offset(grid.deadline(deadline), jitter())));
    }
    events.sort_by_key(Event::at);
    events
}

/// `run_paced_frame`'s GPU backpressure; the injected instant replaces its
/// deadline wait.
fn wait_for_surface(ct: &ContentThread) {
    let give_up = Instant::now() + Duration::from_secs(5);
    while !ct.content_pipeline.is_surface_ready(ct.frame_count) {
        assert!(Instant::now() < give_up, "GPU surface not ready after 5 s");
        std::thread::yield_now();
    }
}

/// Feed one run's events to the content thread; `on_frame` sees each playing
/// tick with the transport frame index it rendered. The content thread's own
/// grid timer, anchored where the schedule's grid is, counts each tick's
/// frames; the index is the engine clock's.
fn drive(ct: &mut ContentThread, anchor: Instant, events: &[Event], mut on_frame: impl FnMut(&ContentThread, i64)) {
    let (state_tx, state_rx) = crate::content_state::state_channel();
    ct.timer.reanchor_grid_at(anchor);
    let mut presented = 0u64;
    for event in events {
        let now = match event {
            Event::Press(_) => {
                assert!(!ct.handle_command(ContentCommand::Play), "Play must not shut down");
                continue;
            }
            Event::Tick(now) => *now,
        };
        wait_for_surface(ct);
        let playing = ct.engine.is_playing();
        ct.tick_frame_at(now, &state_tx);
        // D19: sources are read for the frame's screen instant, the tick's
        // deadline plus the measured delay, not when the tick woke.
        let deadline = ct.timer.tick_deadline().expect("a ticked frame has a deadline");
        let read_at = deadline + Duration::from_secs_f64(ct.calibration.read_delay());
        assert_eq!(ct.link_sample_at, read_at, "the read instant is the deadline plus L");
        // The headless pipeline has no stage window: stand in for Metal's
        // presented handler, each frame on screen 25 ms after its deadline.
        presented += 1;
        let drawable_id = presented;
        ct.issued_presents.record(drawable_id, deadline);
        let at = deadline + Duration::from_millis(SCREEN_DELAY_MS);
        assert!(!ct.handle_command(ContentCommand::FramePresented { drawable_id, at }), "a present must not shut down");
        while state_rx.try_recv().is_ok() {}
        // D5: graphs get the tick's grid frames as dt, never wall time.
        let grid_dt = f64::from(ct.engine.tick_frames()) * ct.engine.transport_clock().step().0;
        assert_eq!(ct.content_pipeline.last_render_dt().to_bits(), grid_dt.to_bits(), "render dt is whole grid frames");
        if playing {
            on_frame(ct, ct.engine.transport_frame());
        }
    }
}

fn run(seed: Option<u64>) -> BTreeMap<i64, Frame> {
    let mut ct = headless_content_thread(plasma_project(), 320, 180);
    let anchor = Instant::now();
    let events = schedule(anchor, seed);
    let mut frames = BTreeMap::new();
    drive(&mut ct, anchor, &events, |ct, index| {
        frames.insert(index, capture(ct));
    });
    // D19: the presents reached the calibration, and the read instant moved
    // from the 17 ms default to what they measured.
    let measured = ct.calibration.present_delay();
    assert!((measured - SCREEN_DELAY_MS as f64 / 1000.0).abs() < 1e-6, "the measured screen delay: {:.3} ms", measured * 1000.0);
    frames
}

fn seconds(bits: u64) -> String {
    format!("{} (bits {bits:#018x})", f64::from_bits(bits))
}

fn first_divergence(a: &BTreeMap<i64, Frame>, b: &BTreeMap<i64, Frame>) -> Option<String> {
    let (index, fa, fb) = a.iter().filter_map(|(i, fa)| b.get(i).map(|fb| (i, fa, fb))).find(|(_, fa, fb)| fa != fb)?;
    let field = if fa.time_bits != fb.time_bits {
        format!("time {} vs {}", seconds(fa.time_bits), seconds(fb.time_bits))
    } else if fa.beat_bits != fb.beat_bits {
        format!("beat {} vs {}", seconds(fa.beat_bits), seconds(fb.beat_bits))
    } else if fa.active_clips != fb.active_clips {
        format!("active clips {} vs {}", fa.active_clips, fb.active_clips)
    } else {
        let p = fa.param_bits.iter().zip(&fb.param_bits).position(|(x, y)| x != y).unwrap_or(0);
        format!("param {p}: {} vs {}", f32::from_bits(fa.param_bits[p]), f32::from_bits(fb.param_bits[p]))
    };
    let shared = a.keys().filter(|i| b.contains_key(i)).count();
    let differing = a.iter().filter(|(i, fa)| b.get(i).is_some_and(|fb| fb != *fa)).count();
    Some(format!("{differing} of {shared} shared transport frames differ; the first is frame {index}: {field}"))
}

fn distinct<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> usize {
    let mut seen: Vec<T> = Vec::new();
    for v in values {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.len()
}

#[test]
fn same_values_per_index_through_content_thread() {
    let steady = run(None);
    assert_eq!(steady.len(), PLAYING_TICKS, "one frame per steady tick");
    let last = steady.values().next_back().expect("frames");
    assert!(f64::from_bits(last.beat_bits) > 33.0, "the run must cross the 141 BPM change at beat 32");
    let moving = (0..last.param_bits.len()).filter(|&p| distinct(steady.values().map(|f| f.param_bits[p])) >= 8).count();
    assert!(moving >= 3, "both LFOs and the automation lane must move: {moving} params moved");
    let memberships = distinct(steady.values().map(|f| f.active_clips));
    assert!(memberships >= 2, "clips must start and stop: {memberships} membership sizes");
    assert_eq!(first_divergence(&steady, &run(None)), None, "the steady run must repeat bit for bit");

    let jittered = run(Some(SEED));
    let shared = steady.keys().filter(|i| jittered.contains_key(i)).count();
    assert!(shared + 3 >= PLAYING_TICKS, "the jittered run must render all but the stalled frames: {shared}");
    if let Some(divergence) = first_divergence(&steady, &jittered) {
        panic!("same arrangement through the content thread, different values per transport frame: {divergence}");
    }
}
