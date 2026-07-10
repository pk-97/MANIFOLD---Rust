//! Headless end-to-end proof suite for the live recorder (Tier 1).
//!
//! Drives the real `LiveRecordingSession` API into the real native
//! AVAssetWriter encoder, at realistic frame counts, with adversarial
//! timing — then interrogates the output file with ffprobe/ffmpeg (an
//! independent oracle, D3). See docs/LIVE_RECORDING_PROOFS_DESIGN.md.
//!
//! `cargo test -p manifold-recording --features recording-proofs`

use std::path::PathBuf;
use std::time::Duration;

use manifold_gpu::{GpuDevice, GpuTexture};
use manifold_recording::proofs::{self, PatternWriter};
use manifold_recording::{AudioCodec, AudioFeed, LiveRecordingConfig, LiveRecordingSession};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FPS: f32 = 60.0;
/// Perfect 60fps grid step, nanoseconds (D8 — tests pace by pool
/// availability, timestamps are fabricated on this grid regardless of real
/// submission speed).
const FRAME_NS: u64 = 16_666_667;

// ---------------------------------------------------------------------
// Shared harness plumbing
// ---------------------------------------------------------------------

/// Stable output directory inside the worktree's target dir — kept
/// artifacts (not deleted) so a landing session can open them.
fn proof_output_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/proof-output");
    std::fs::create_dir_all(&dir).expect("create proof-output dir");
    dir
}

fn scratch_output(name: &str) -> PathBuf {
    proof_output_dir().join(format!("{name}.mov"))
}

fn video_only_config(output_path: PathBuf) -> LiveRecordingConfig {
    LiveRecordingConfig {
        output_path: output_path.to_string_lossy().into_owned(),
        hdr: false,
        audio_device: None,
        audio_codec: AudioCodec::Aac,
    }
}

fn grid_elapsed(n: u32) -> Duration {
    Duration::from_nanos(n as u64 * FRAME_NS)
}

fn is_strictly_increasing(values: &[i64]) -> bool {
    values.windows(2).all(|w| w[1] > w[0])
}

/// Submit one synthetic frame through the production capture sequence,
/// transcribed from content_pipeline.rs:2547-2621: acquire slot (D8 pacing —
/// spin on `acquire_texture` with a 200µs sleep, fine in a test, banned on
/// the content thread) → dispatch the pattern shader into the Rgba16Float
/// source texture → the REAL `encode_format_conversion` into the pool
/// texture → `add_completed_handler` signals the fence → commit →
/// `submit_frame_at` with the scripted timestamp.
fn submit_paced_frame(
    session: &mut LiveRecordingSession,
    device: &GpuDevice,
    pattern: &PatternWriter,
    src: &GpuTexture,
    frame_index: u32,
    elapsed: Duration,
) {
    loop {
        if let Some((tex_idx, pool_slot, fence)) = session.acquire_texture() {
            let mut encoder = device.create_encoder("recording-proofs frame");
            pattern.encode(&mut encoder, src, frame_index);
            session.encode_format_conversion(&mut encoder, src, session.pool_texture(tex_idx));
            let fence_for_signal = fence.clone();
            encoder.add_completed_handler(move || fence_for_signal.signal());
            encoder.commit();
            session.submit_frame_at(pool_slot, fence, elapsed);
            return;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn push_sine_chunk(
    producer: &mut impl ringbuf::traits::Producer<Item = f32>,
    phase: &mut f32,
    sample_rate: u32,
    channels: u16,
    num_frames: u32,
) {
    const FREQ_HZ: f32 = 440.0;
    const AMPLITUDE: f32 = 0.25;
    let mut buf = Vec::with_capacity(num_frames as usize * channels as usize);
    for _ in 0..num_frames {
        let sample = (*phase * std::f32::consts::TAU).sin() * AMPLITUDE;
        for _ in 0..channels {
            buf.push(sample);
        }
        *phase += FREQ_HZ / sample_rate as f32;
        if *phase >= 1.0 {
            *phase -= 1.0;
        }
    }
    producer.push_slice(&buf);
}

// ---------------------------------------------------------------------
// Test 1 — nominal_video_only
// ---------------------------------------------------------------------

#[test]
fn nominal_video_only() {
    let _guard = proofs::gpu_guard();
    let device = proofs::test_device();
    let pattern = PatternWriter::new(&device);
    let src = proofs::synthetic_source_texture(&device, WIDTH, HEIGHT);

    let out = scratch_output("nominal_video_only");
    let config = video_only_config(out.clone());
    let mut session =
        LiveRecordingSession::new_with_audio_feed(config, &device, WIDTH, HEIGHT, FPS, AudioFeed::None)
            .expect("session creation");

    for frame_index in 0..600u32 {
        submit_paced_frame(
            &mut session,
            &device,
            &pattern,
            &src,
            frame_index,
            grid_elapsed(frame_index),
        );
    }

    let result = session.stop();
    assert_eq!(result.frames_recorded, 600, "expected 600 recorded: {result:?}");
    assert_eq!(result.frames_dropped, 0, "expected 0 dropped: {result:?}");

    let report = proofs::probe(&out, true).expect("probe");
    assert!(
        report.codec.contains("prores"),
        "expected prores codec, got {}",
        report.codec
    );
    assert_eq!(report.video_frame_count, 600);
    assert_eq!(report.pts.len(), 600);
    assert!(is_strictly_increasing(&report.pts), "PTS not strictly increasing: {:?}", report.pts);
    assert!(
        (report.video_duration_s - 10.0).abs() <= 0.05,
        "video_duration_s {} not within ±50ms of 10.0",
        report.video_duration_s
    );
    let expected_indices: Vec<u32> = (0..600).collect();
    assert_eq!(report.frame_indices, expected_indices, "frame identity did not survive the encode");

    println!(
        "nominal_video_only: {} frames, codec={}, video_duration_s={:.3}, kept at {}",
        report.frame_indices.len(),
        report.codec,
        report.video_duration_s,
        out.display()
    );
}

// ---------------------------------------------------------------------
// Test 2 — nominal_with_audio
// ---------------------------------------------------------------------

#[test]
fn nominal_with_audio() {
    let _guard = proofs::gpu_guard();
    let device = proofs::test_device();
    let pattern = PatternWriter::new(&device);
    let src = proofs::synthetic_source_texture(&device, WIDTH, HEIGHT);

    let sample_rate = 48_000u32;
    let channels = 2u16;
    // Sized to comfortably hold the full 10s of synthetic audio (960,000
    // interleaved floats) plus headroom. D8's unpaced submission means the
    // test thread pushes all 10s of audio while racing 600 unthrottled
    // video-frame submissions on the SAME thread — the recording thread's
    // 2ms drain cadence can't be assumed to keep up in real time under that
    // load, and `push_slice` silently drops samples that don't fit. A
    // smaller (e.g. 1s) capacity measurably overflows here.
    let ring = ringbuf::HeapRb::<f32>::new((sample_rate as usize) * (channels as usize) * 12);
    let (mut producer, consumer) = ringbuf::traits::Split::split(ring);

    let out = scratch_output("nominal_with_audio");
    let config = video_only_config(out.clone());
    let mut session = LiveRecordingSession::new_with_audio_feed(
        config,
        &device,
        WIDTH,
        HEIGHT,
        FPS,
        AudioFeed::Injected {
            consumer,
            sample_rate,
            channels,
        },
    )
    .expect("session creation with injected audio");

    // 48000/60 = 800 exactly — one audio chunk per video frame, 600 frames
    // = 480,000 sample-frames = exactly 10.0s. (Simpler than literal ~10ms
    // chunking; the gate is on final durations, not chunk granularity.)
    let samples_per_frame = sample_rate / 60;
    let mut phase = 0.0f32;

    for frame_index in 0..600u32 {
        push_sine_chunk(&mut producer, &mut phase, sample_rate, channels, samples_per_frame);
        submit_paced_frame(
            &mut session,
            &device,
            &pattern,
            &src,
            frame_index,
            grid_elapsed(frame_index),
        );
    }

    let result = session.stop();
    assert_eq!(result.frames_recorded, 600, "expected 600 recorded: {result:?}");
    assert_eq!(result.frames_dropped, 0, "expected 0 dropped: {result:?}");

    let report = proofs::probe(&out, true).expect("probe");
    assert!(report.codec.contains("prores"));
    assert_eq!(report.pts.len(), 600);
    assert!(is_strictly_increasing(&report.pts));
    assert!((report.video_duration_s - 10.0).abs() <= 0.05);
    let expected_indices: Vec<u32> = (0..600).collect();
    assert_eq!(report.frame_indices, expected_indices);

    let audio_duration_s = report
        .audio_duration_s
        .expect("audio stream present in probe report");
    assert!(
        (audio_duration_s - 10.0).abs() <= 0.05,
        "audio_duration_s {audio_duration_s} not within ±50ms of 10.0"
    );
    assert!(
        (audio_duration_s - report.video_duration_s).abs() <= 0.1,
        "audio/video duration mismatch: audio={audio_duration_s} video={}",
        report.video_duration_s
    );

    println!(
        "nominal_with_audio probe: {} video frames, PTS strictly increasing, \
         video_duration_s={:.3}, audio_duration_s={:.3}, kept at {}",
        report.frame_indices.len(),
        report.video_duration_s,
        audio_duration_s,
        out.display()
    );
}

// ---------------------------------------------------------------------
// Test 3 — adversarial_pts_survives (the regression fence for failure
// class 3: -16364 duplicate PTS, fixed by timescale 90000 + monotonic
// clamp, fbee1ed2).
// ---------------------------------------------------------------------

/// Scripted adversarial elapsed-time sequence over a 600-frame 60fps grid.
/// See docs/LIVE_RECORDING_PROOFS_DESIGN.md §4 test 3 for the injection
/// spec; each anomaly is applied relative to the plain grid so its effect
/// on PTS is legible in isolation.
fn scripted_elapsed(n: u32) -> Duration {
    match n {
        // n=100..110 duplicate n=99's timestamp exactly.
        100..=109 => grid_elapsed(99),
        // n=200 jumps backwards 50ms from its grid position.
        200 => grid_elapsed(200).saturating_sub(Duration::from_millis(50)),
        // n=300..302 use +5µs deltas off n=299 — below the ~11µs
        // timescale-90000 resolution, forcing near-duplicate PTS.
        300..=302 => grid_elapsed(299) + Duration::from_micros(5) * (n - 299),
        // n=400 stalls +2s, then resumes the grid's normal per-frame slope
        // (the +2s offset persists — a real wall clock can't run backward).
        400..=599 => grid_elapsed(n) + Duration::from_secs(2),
        _ => grid_elapsed(n),
    }
}

/// Median PTS delta over `range` — used as a robust "one frame interval in
/// PTS ticks" reference, insensitive to any single sample's rounding jitter.
fn median_pts_delta(pts: &[i64], range: std::ops::Range<usize>) -> f64 {
    let mut deltas: Vec<i64> = range.map(|i| pts[i] - pts[i - 1]).collect();
    deltas.sort_unstable();
    deltas[deltas.len() / 2] as f64
}

#[test]
fn adversarial_pts_survives() {
    let _guard = proofs::gpu_guard();
    let device = proofs::test_device();
    let pattern = PatternWriter::new(&device);
    let src = proofs::synthetic_source_texture(&device, WIDTH, HEIGHT);

    let out = scratch_output("adversarial_pts_survives");
    let config = video_only_config(out.clone());
    let mut session =
        LiveRecordingSession::new_with_audio_feed(config, &device, WIDTH, HEIGHT, FPS, AudioFeed::None)
            .expect("session creation");

    for frame_index in 0..600u32 {
        submit_paced_frame(
            &mut session,
            &device,
            &pattern,
            &src,
            frame_index,
            scripted_elapsed(frame_index),
        );
    }

    let result = session.stop();
    assert_eq!(
        result.frames_recorded, 600,
        "writer must never leave Writing under adversarial PTS (pre-fbee1ed2 \
         regression): {result:?}"
    );

    let report = proofs::probe(&out, true).expect("probe");
    assert_eq!(report.pts.len(), 600);
    assert!(is_strictly_increasing(&report.pts), "PTS not strictly increasing: {:?}", report.pts);
    let expected_indices: Vec<u32> = (0..600).collect();
    assert_eq!(report.frame_indices, expected_indices, "frame identity did not survive adversarial PTS");

    // The n=400 stall's ~2s gap must survive the monotonic clamp — the
    // clamp exists to kill duplicates, never to flatten a real gap (D9 /
    // fbee1ed2's wall-clock-fidelity contract). Expressed as a ratio
    // against a normal per-frame PTS step so this doesn't depend on the
    // encoder's actual timescale value.
    let normal_step = median_pts_delta(&report.pts, 450..550);
    assert!(normal_step > 0.0, "normal PTS step must be positive: {normal_step}");
    let gap_step = (report.pts[400] - report.pts[399]) as f64;
    let gap_seconds_estimate = (gap_step / normal_step) / 60.0;
    let expected_gap_s = 2.0 + 1.0 / 60.0; // +2s stall, plus the one normal frame step
    assert!(
        (gap_seconds_estimate - expected_gap_s).abs() <= 0.1,
        "n=400 stall gap not preserved: estimated {gap_seconds_estimate:.3}s \
         (normal_step={normal_step}, gap_step={gap_step}, expected≈{expected_gap_s:.3}s)"
    );

    println!(
        "adversarial_pts_survives: 600 frames recorded, PTS strictly increasing, \
         n=400 gap≈{gap_seconds_estimate:.3}s, kept at {}",
        out.display()
    );
}

// ---------------------------------------------------------------------
// Test 4 — pool_accounting_consistent
// ---------------------------------------------------------------------

#[test]
fn pool_accounting_consistent() {
    let _guard = proofs::gpu_guard();
    let device = proofs::test_device();
    let pattern = PatternWriter::new(&device);
    let src = proofs::synthetic_source_texture(&device, WIDTH, HEIGHT);

    let out = scratch_output("pool_accounting_consistent");
    let config = video_only_config(out.clone());
    let mut session =
        LiveRecordingSession::new_with_audio_feed(config, &device, WIDTH, HEIGHT, FPS, AudioFeed::None)
            .expect("session creation");

    // Hold the first 4 acquired slots' fences un-signaled — a gated
    // fence-signal simulating a slow encoder — until frame 100. Because the
    // recording thread drains its channel strictly FIFO, this stalls
    // encoding at the very first held frame, so the pool's other 4 slots
    // also can't be released while stalled: real backpressure, real drops,
    // exactly like a slow/backlogged encoder in production. Every attempt
    // is accounted for exactly once (recorded or dropped) — the invariant
    // this test gates.
    const HOLD_UNTIL_FRAME: u32 = 4;
    const RELEASE_AT_FRAME: u32 = 100;
    let mut held_fences: Vec<std::sync::Arc<manifold_recording::GpuFence>> = Vec::new();

    const FRAME_COUNT: u32 = 200;
    // Bounded retry (not the nominal tests' unbounded spin, not a single
    // non-blocking attempt): during the active stall (frames ~4..~99, while
    // the recording thread is blocked FIFO-head on a held fence), even this
    // bound is exhausted every time — genuine drops. After the release
    // point, it gives the recording thread's catch-up a real chance to
    // land within the same frame's acquire, so the test actually exercises
    // recovery rather than cascading every remaining frame into a drop.
    const ACQUIRE_RETRIES: u32 = 25;
    for frame_index in 0..FRAME_COUNT {
        let elapsed = grid_elapsed(frame_index);

        let mut acquired = session.acquire_texture();
        let mut retries_left = ACQUIRE_RETRIES;
        while acquired.is_none() && retries_left > 0 {
            std::thread::sleep(Duration::from_micros(200));
            acquired = session.acquire_texture();
            retries_left -= 1;
        }

        match acquired {
            Some((tex_idx, pool_slot, fence)) => {
                let mut encoder = device.create_encoder("pool_accounting_consistent frame");
                pattern.encode(&mut encoder, &src, frame_index);
                session.encode_format_conversion(&mut encoder, &src, session.pool_texture(tex_idx));

                if frame_index < HOLD_UNTIL_FRAME {
                    // Gate the signal — don't register a completion handler
                    // that fires it; hold the fence for manual release.
                    held_fences.push(fence.clone());
                } else {
                    let fence_for_signal = fence.clone();
                    encoder.add_completed_handler(move || fence_for_signal.signal());
                }
                encoder.commit();
                session.submit_frame_at(pool_slot, fence, elapsed);
            }
            None => {
                session.record_dropped_frame();
            }
        }

        if frame_index + 1 == RELEASE_AT_FRAME {
            for f in held_fences.drain(..) {
                f.signal();
            }
        }
    }

    let result = session.stop();
    assert_eq!(
        result.frames_recorded + result.frames_dropped,
        FRAME_COUNT,
        "accounting mismatch: {result:?}"
    );
    assert!(result.frames_recorded > 0, "expected at least some frames to survive backpressure: {result:?}");

    let report = proofs::probe(&out, true).expect("probe");
    // <= not ==: BUG-085 (docs/BUG_BACKLOG.md) — under real backpressure the
    // native encoder's async VideoToolbox append can silently drop a frame
    // AFTER `LiveRecorder_EncodeVideoFrame` already returned success, so
    // Rust's `frames_recorded` can (rarely) overstate the file's real packet
    // count by a small amount. The file itself stays valid either way — the
    // invariant this test actually gates is the Rust-side accounting sum
    // above, which BUG-085 doesn't touch.
    assert!(
        report.pts.len() as u32 <= result.frames_recorded,
        "file has MORE packets than Rust recorded — that would be real corruption \
         (not BUG-085's direction): pts.len()={} frames_recorded={}",
        report.pts.len(),
        result.frames_recorded
    );
    assert!(is_strictly_increasing(&report.pts), "PTS not strictly increasing: {:?}", report.pts);
    // Gaps are allowed/expected (dropped frames leave PTS gaps, never
    // corruption) — just confirm decoded indices are strictly increasing
    // (no reorder, no duplicate).
    assert!(
        report.frame_indices.windows(2).all(|w| w[1] > w[0]),
        "decoded frame indices not strictly increasing: {:?}",
        report.frame_indices
    );

    println!(
        "pool_accounting_consistent: submitted={FRAME_COUNT} recorded={} dropped={} kept at {}",
        result.frames_recorded,
        result.frames_dropped,
        out.display()
    );
}

// ---------------------------------------------------------------------
// Test 6 — hdr_blocked_by_bug_053
// ---------------------------------------------------------------------

/// BUG-053: HDR live recording is structurally broken today — the pool is
/// unconditionally Bgra8Unorm (session.rs) while the native HDR path wraps
/// its CVPixelBuffer as RGBA16Float and blits pool → buffer
/// (LiveRecordingPlugin.m), a format-mismatched Metal blit. This test
/// asserts the failure surfaces LOUDLY — never a silent SDR fallback and
/// never a silently "successful" HDR recording. When BUG-053 is fixed, this
/// test is replaced by an HDR twin of `nominal_video_only` (design §8).
#[test]
fn hdr_blocked_by_bug_053() {
    let _guard = proofs::gpu_guard();
    // A dedicated device, NOT the shared `test_device()` — the format-
    // mismatched blit below is expected to fault at the GPU level (observed:
    // MTLCommandBufferErrorDomain Code=3, GPU Address Fault), and other
    // tests in this binary share `test_device()`'s cached device. Isolating
    // this test's device means a faulted command queue can't leak into
    // unrelated tests.
    let device = GpuDevice::new();

    let out = scratch_output("hdr_blocked_by_bug_053");
    let mut config = video_only_config(out);
    config.hdr = true;

    match LiveRecordingSession::new_with_audio_feed(config, &device, WIDTH, HEIGHT, FPS, AudioFeed::None) {
        Err(e) => {
            // Loud failure at construction — session creation itself
            // refused the mismatched HDR configuration.
            println!("hdr_blocked_by_bug_053: session creation failed loudly: {e}");
        }
        Ok(mut session) => {
            // Native encoder creation doesn't validate the pixel-format
            // mismatch up front (it only surfaces at the per-frame Metal
            // blit — LiveRecordingPlugin.m checks cmdBuf.status after
            // waitUntilCompleted and returns LR_ERR_BLIT_FAILED rather than
            // crashing). Drive exactly one frame and require the failure to
            // be visible in the result — never a silently "successful" HDR
            // recording.
            let pattern = PatternWriter::new(&device);
            let src = proofs::synthetic_source_texture(&device, WIDTH, HEIGHT);
            submit_paced_frame(&mut session, &device, &pattern, &src, 0, Duration::ZERO);
            let result = session.stop();
            assert_ne!(
                result.frames_recorded, 1,
                "BUG-053: HDR recording silently succeeded — pool format \
                 mismatch (Bgra8Unorm vs the HDR path's RGBA16Float \
                 CVPixelBuffer) should fail loudly, not encode cleanly: \
                 {result:?}"
            );
            println!(
                "hdr_blocked_by_bug_053: session created but first-encode failed \
                 loudly: {result:?}"
            );
        }
    }
}
