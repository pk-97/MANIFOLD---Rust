//! `recording-soak` — the pre-gig soundcheck for the live show recorder
//! (Tier 2). See docs/LIVE_RECORDING_PROOFS_DESIGN.md §5.
//!
//! Drives the real `LiveRecordingSession` at the show configuration (4K60
//! SDR ProRes + synthetic audio, 20 media-minutes by default), then verifies
//! the output with the same ffprobe/ffmpeg oracle as the Tier-1 proof suite
//! (`crate::proofs`) — reused, not reinvented (design §6 P2).
//!
//! `cargo run --release -p manifold-recording --features recording-proofs --bin recording-soak`
//!
//! PASS decision (default/unpaced mode) is anchored to the DECODED file, not
//! only Rust's `frames_recorded` counter — belt-and-suspenders against the
//! class of bug fixed by BUG-085 (docs/BUG_BACKLOG.md): the async
//! `appendPixelBuffer:` call now feeds `frames_recorded` its ground truth
//! (read from `LiveRecorder_Finalize` after the append queue drains), but
//! this binary still cross-checks the decoded frame-index sequence
//! (`find_first_gap`) as an independent oracle rather than trusting either
//! counter alone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use manifold_gpu::{GpuDevice, GpuTexture};
use manifold_recording::proofs::{self, PatternWriter};
use manifold_recording::{AudioCodec, AudioFeed, LiveRecordingConfig, LiveRecordingSession};

/// Frame-index pattern capacity (must stay below `proofs`' private
/// `INDEX_BITS = 24` — 2^24 distinct indices). A safety guard against
/// unreasonable `--minutes`/`--fps` combinations, not a normal-path limit
/// (the 20-minute default is 72,000 frames, far under this).
const MAX_FRAME_INDEX_EXCLUSIVE: u32 = 1 << 24;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Rough ProRes 422 Proxy (SDR) size estimate, derived from the design's own
/// reference point (§5: 4K60, 20 min, 72,000 frames ~= 17.5 GB):
/// bits/pixel = (17.5 * 1024^3 * 8) / (3840 * 2160 * 72000) ~= 0.2517.
/// This is only a preflight disk-space sanity check — the 1.5x safety
/// margin in `run()` absorbs estimation error, it does not need to be exact.
const PRORES_BITS_PER_PIXEL: f64 = 0.2517;
/// AAC 320kbps stereo — matches `AudioCodec::Aac`, the codec this binary uses.
const AUDIO_BITS_PER_SECOND: f64 = 320_000.0;

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("recording-soak: {e}");
            print_usage();
            return 1;
        }
    };

    // HDR is deferred behind BUG-053; refuse loudly before touching
    // anything (disk, GPU, encoder).
    if args.hdr {
        println!(
            "SOAK FAIL: --hdr blocked by BUG-053 (HDR live recording is structurally \
             broken today — the texture pool is unconditionally Bgra8Unorm while the \
             native HDR path wraps its CVPixelBuffer as RGBA16Float, a format-mismatched \
             Metal blit; see docs/BUG_BACKLOG.md BUG-053)"
        );
        return 1;
    }

    execute(&args)
}

// ---------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------

struct Args {
    width: u32,
    height: u32,
    fps: f32,
    minutes: f64,
    audio: bool,
    realtime: bool,
    keep: bool,
    output: Option<PathBuf>,
    hdr: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            width: 3840,
            height: 2160,
            fps: 60.0,
            minutes: 20.0,
            audio: true,
            realtime: false,
            keep: false,
            output: None,
            hdr: false,
        }
    }
}

impl Args {
    fn parse(mut iter: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Args::default();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--width" => args.width = next_parsed(&mut iter, "--width")?,
                "--height" => args.height = next_parsed(&mut iter, "--height")?,
                "--fps" => args.fps = next_parsed(&mut iter, "--fps")?,
                "--minutes" => args.minutes = next_parsed(&mut iter, "--minutes")?,
                "--no-audio" => args.audio = false,
                "--realtime" => args.realtime = true,
                "--keep" => args.keep = true,
                "--output" => args.output = Some(PathBuf::from(next_value(&mut iter, "--output")?)),
                "--hdr" => args.hdr = true,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => return Err(format!("unrecognized argument: {other}")),
            }
        }

        if args.width == 0 || args.height == 0 {
            return Err("--width and --height must be nonzero".into());
        }
        if args.fps <= 0.0 || args.fps.is_nan() {
            return Err("--fps must be positive".into());
        }
        if args.minutes <= 0.0 || args.minutes.is_nan() {
            return Err("--minutes must be positive".into());
        }

        Ok(args)
    }
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next().ok_or_else(|| format!("{flag} requires a value"))
}

fn next_parsed<T: std::str::FromStr>(
    iter: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    let raw = next_value(iter, flag)?;
    raw.parse::<T>()
        .map_err(|e| format!("{flag} value '{raw}' invalid: {e}"))
}

fn print_usage() {
    eprintln!(
        "recording-soak -- pre-gig soundcheck for the live show recorder\n\
         \n\
         USAGE:\n\
         \x20 recording-soak [--width 3840] [--height 2160] [--fps 60] [--minutes 20]\n\
         \x20                [--no-audio] [--realtime] [--keep] [--output <path>] [--hdr]\n\
         \n\
         Defaults are the show config: 4K60 SDR ProRes, 20 media-minutes, synthetic\n\
         48kHz stereo audio, unpaced (encodes as fast as the hardware allows -- the\n\
         encoder-stress mode). --realtime paces submissions to wall clock for a true\n\
         dress rehearsal. See docs/LIVE_RECORDING_PROOFS_DESIGN.md \u{00a7}5 and\n\
         docs/DEVELOPMENT_REFERENCE.md's \"Recorder soundcheck\" section."
    );
}

// ---------------------------------------------------------------------
// Main run
// ---------------------------------------------------------------------

fn execute(args: &Args) -> i32 {
    let total_frames_f64 = (args.minutes * 60.0 * args.fps as f64).round();
    if !(1.0..(MAX_FRAME_INDEX_EXCLUSIVE as f64)).contains(&total_frames_f64) {
        println!(
            "SOAK FAIL: frame budget {total_frames_f64} out of range (must be 1..{MAX_FRAME_INDEX_EXCLUSIVE}, \
             derived from --minutes {} * 60 * --fps {})",
            args.minutes, args.fps
        );
        return 1;
    }
    let total_frames = total_frames_f64 as u32;
    let duration_s = total_frames as f64 / args.fps as f64;

    let output_path = match resolve_output_path(args) {
        Ok(p) => p,
        Err(e) => {
            println!("SOAK FAIL: {e}");
            return 1;
        }
    };

    // -- Pre-flight: disk space --
    let estimated_bytes = estimate_bytes(args.width, args.height, total_frames, duration_s, args.audio);
    let required_bytes = (estimated_bytes as f64 * 1.5).ceil() as u64;
    match free_disk_bytes(&output_path) {
        Ok(free) if free >= required_bytes => {}
        Ok(free) => {
            println!(
                "SOAK FAIL: insufficient free disk space at {}: {:.2} GB free, need >= {:.2} GB \
                 (1.5x the estimated {:.2} GB take) -- free up space or point --output elsewhere",
                output_path.display(),
                free as f64 / GIB,
                required_bytes as f64 / GIB,
                estimated_bytes as f64 / GIB,
            );
            return 1;
        }
        Err(e) => {
            println!("SOAK FAIL: could not determine free disk space: {e}");
            return 1;
        }
    }

    eprintln!(
        "[recording-soak] {}x{} @ {}fps, {:.1} min ({total_frames} frames), audio={}, \
         mode={}, estimated ~{:.2} GB, output={}",
        args.width,
        args.height,
        args.fps,
        args.minutes,
        args.audio,
        if args.realtime { "realtime" } else { "unpaced" },
        estimated_bytes as f64 / GIB,
        output_path.display(),
    );

    let _guard = proofs::gpu_guard();
    let device = proofs::test_device();
    let pattern = PatternWriter::new(&device);
    let src = proofs::synthetic_source_texture(&device, args.width, args.height);

    let config = LiveRecordingConfig {
        output_path: output_path.to_string_lossy().into_owned(),
        hdr: false,
        audio_device: None,
        audio_codec: AudioCodec::Aac,
    };

    let sample_rate = 48_000u32;
    let channels = 2u16;

    let (mut producer, feed) = if args.audio {
        // A few seconds' headroom is enough -- audio is now paced to REAL
        // wall-clock time (see the push loop below), never bursted, so the
        // ring buffer only has to smooth over the recording thread's 2ms
        // drain cadence, not absorb a whole take at once.
        let capacity = (sample_rate as usize) * (channels as usize) * 5;
        let ring = ringbuf::HeapRb::<f32>::new(capacity);
        let (producer, consumer) = ringbuf::traits::Split::split(ring);
        (
            Some(producer),
            AudioFeed::Injected {
                consumer,
                sample_rate,
                channels,
            },
        )
    } else {
        (None, AudioFeed::None)
    };

    let mut session = match LiveRecordingSession::new_with_audio_feed(
        config,
        &device,
        args.width,
        args.height,
        args.fps,
        feed,
    ) {
        Ok(s) => s,
        Err(e) => {
            println!("SOAK FAIL: session creation failed: {e}");
            return fail_tail(&output_path);
        }
    };

    let mut phase = 0.0f32;
    // Sample-frames pushed so far, tracked against REAL wall-clock elapsed
    // time (never against video frame-loop iteration count -- see the
    // `push_realtime_audio_chunk` doc comment for why: the native audio
    // input is `expectsMediaDataInRealTime = YES` and silently drops
    // (LR_OK, no counter) samples delivered faster than real time).
    let mut audio_pushed_frames = 0u64;

    let start = Instant::now();
    let mut last_progress = Instant::now();
    let mut dropped_realtime = 0u32;

    for frame_index in 0..total_frames {
        if let Some(ref mut producer) = producer {
            push_realtime_audio_chunk(
                producer,
                &mut phase,
                sample_rate,
                channels,
                &mut audio_pushed_frames,
                start,
            );
        }

        if args.realtime {
            let target = Duration::from_secs_f64(frame_index as f64 / args.fps as f64);
            let now = start.elapsed();
            if target > now {
                std::thread::sleep(target - now);
            }
            let submitted =
                submit_frame_realtime(&mut session, &device, &pattern, &src, frame_index, start);
            if !submitted {
                dropped_realtime += 1;
            }
        } else {
            let elapsed = Duration::from_secs_f64(frame_index as f64 / args.fps as f64);
            submit_frame_unpaced(&mut session, &device, &pattern, &src, frame_index, elapsed);
        }

        // Coarse progress only -- every 5s, to stderr. Never per-frame.
        if last_progress.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "[recording-soak] progress: {}/{total_frames} frames ({:.0}%)",
                frame_index + 1,
                100.0 * (frame_index + 1) as f64 / total_frames as f64,
            );
            last_progress = Instant::now();
        }
    }

    // Audio real-time floor: unpaced VIDEO can finish encoding well before
    // `duration_s` of real wall-clock time has elapsed (that's the whole
    // point of D8's encoder-stress mode), but the native audio input is
    // `expectsMediaDataInRealTime = YES` and cannot accept audio faster than
    // real time regardless -- exactly like a real show, whose audio track
    // is bounded below by the real time an actual audio device takes to
    // produce it, however fast the GPU could in principle render. Wait out
    // the remainder here, continuing to push audio at its natural cadence,
    // so the file's audio track covers the full intended duration instead
    // of silently truncating at whatever wall time the video loop finished.
    if let Some(ref mut producer) = producer {
        let deadline = start + Duration::from_secs_f64(duration_s) + Duration::from_secs(30);
        while start.elapsed() < Duration::from_secs_f64(duration_s) && Instant::now() < deadline {
            push_realtime_audio_chunk(
                producer,
                &mut phase,
                sample_rate,
                channels,
                &mut audio_pushed_frames,
                start,
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        push_realtime_audio_chunk(
            producer,
            &mut phase,
            sample_rate,
            channels,
            &mut audio_pushed_frames,
            start,
        );
    }

    let result = session.stop();

    let report = match proofs::probe(&output_path, true) {
        Ok(r) => r,
        Err(e) => {
            println!("SOAK FAIL: probe failed: {e}");
            return fail_tail(&output_path);
        }
    };

    let file_bytes = std::fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
    let gb = file_bytes as f64 / GIB;

    if !report.codec.contains("prores") {
        println!("SOAK FAIL: expected prores codec, got '{}'", report.codec);
        return fail_tail(&output_path);
    }
    if !is_strictly_increasing(&report.pts) {
        println!("SOAK FAIL: video packet PTS not strictly increasing -- decoded PTS out of order");
        return fail_tail(&output_path);
    }

    // Audio coverage sanity gate (both modes): the decoded audio track must
    // exist and must not have catastrophically collapsed. Fixed by tracking
    // the real accepted count, which
    // self-heals the backlog on the next call. Repeated 2-minute unpaced
    // runs post-fix measured audio_duration_s at 120.006s-120.012s (<0.01%
    // off) -- the WARNING threshold below is tightened accordingly; the 50%
    // floor remains as a defense against a genuinely different collapse.
    if args.audio {
        // BUG-084/BUG-086 instrument: the native encoder's backpressure-drop
        // counter, now live. Always printed when audio is on, whether
        // or not the coverage gate below fires -- a 0 reading on a take that
        // still falls short is itself an observation (rules the gate out as
        // BUG-086's cause for that run).
        println!(
            "[recording-soak] audio_frames_dropped (native backpressure gate) = {}",
            result.audio_frames_dropped
        );
        let intended_audio_frames = (duration_s * sample_rate as f64).round() as u64;
        println!(
            "[recording-soak] audio_pushed_frames (harness ring-buffer accepted) = {audio_pushed_frames} / {intended_audio_frames} intended",
        );
        match report.audio_duration_s {
            Some(a) if a >= duration_s * 0.5 => {
                if (a - duration_s).abs() > duration_s * 0.005 {
                    eprintln!(
                        "[recording-soak] WARNING: audio_duration_s {a:.3}s is {:.3}s short of \
                         the intended {duration_s:.1}s. audio_frames_dropped={} \
                         audio_pushed_frames={audio_pushed_frames}/{intended_audio_frames} \
                         -- {} the shortfall.",
                        duration_s - a,
                        result.audio_frames_dropped,
                        if result.audio_frames_dropped > 0 {
                            "native backpressure drops correlate with"
                        } else if audio_pushed_frames < intended_audio_frames {
                            "the harness's own ring buffer under-pushed, which explains"
                        } else {
                            "neither known counter explains"
                        },
                    );
                }
            }
            Some(a) => {
                println!(
                    "SOAK FAIL: audio_duration_s {a:.1}s is less than 50% of the intended \
                     {duration_s:.1}s -- audio track catastrophically under-covers the take"
                );
                return fail_tail(&output_path);
            }
            None => {
                println!("SOAK FAIL: --no-audio was not set but no audio stream found in probe");
                return fail_tail(&output_path);
            }
        }
    }

    let audio_suffix = report
        .audio_duration_s
        .map(|a| format!(", audio {a:.1}s"))
        .unwrap_or_default();

    if args.realtime {
        // --realtime gates on FILE VALIDITY ONLY (D8's consequence): codec
        // correct and PTS strictly increasing (checked above), and decoded
        // indices strictly increasing (no reorder/duplicate -- genuine
        // corruption, distinct from an expected drop-induced gap). Drops
        // are reported, never gated -- keep-up under load is the
        // render-trace gate's job, not the soak's.
        if !report.frame_indices.windows(2).all(|w| w[1] > w[0]) {
            println!(
                "SOAK FAIL: decoded frame indices not strictly increasing (reorder or \
                 duplicate -- corruption, not an expected drop gap): {:?}",
                &report.frame_indices[..report.frame_indices.len().min(20)]
            );
            return fail_tail(&output_path);
        }
        println!(
            "SOAK PASS: {total_frames} frames submitted, {dropped_realtime} dropped \
             (--realtime, gated on file validity only), PTS monotonic, \
             {} decoded frames valid (gaps expected under real-time pacing), {gb:.2} GB{audio_suffix}",
            report.frame_indices.len(),
        );
    } else {
        // Default/unpaced mode gates on the DECODED file, not
        // `result.frames_dropped` alone (BUG-085) -- an async
        // appendPixelBuffer: drop shows up as a gap here even when Rust's
        // counter reports 0 drops.
        if let Some(gap_msg) = find_first_gap(&report.frame_indices, total_frames) {
            println!(
                "SOAK FAIL: {gap_msg} (Rust-side accounting: frames_recorded={}, \
                 frames_dropped={} -- BUG-085: this counter can overstate the file's \
                 real packet count under async encoder backpressure, so the decoded \
                 file above is authoritative)",
                result.frames_recorded, result.frames_dropped,
            );
            return fail_tail(&output_path);
        }
        if result.frames_dropped != 0 {
            println!(
                "SOAK FAIL: {} frames dropped (Rust accounting) under unpaced/default mode -- expected 0",
                result.frames_dropped
            );
            return fail_tail(&output_path);
        }
        println!(
            "SOAK PASS: {total_frames} frames, {} dropped, PTS monotonic, gap-free indices, {gb:.2} GB{audio_suffix}",
            result.frames_dropped
        );
    }

    if args.keep {
        println!("kept at: {}", output_path.display());
    } else if let Err(e) = std::fs::remove_file(&output_path) {
        eprintln!(
            "[recording-soak] warning: failed to delete {}: {e}",
            output_path.display()
        );
    }

    0
}

/// FAIL path tail: the failed file is the evidence -- never delete it, print
/// its path (only if it actually exists -- some failures happen before any
/// file is written, e.g. session creation refusing outright).
fn fail_tail(output_path: &Path) -> i32 {
    if output_path.exists() {
        println!("kept at: {}", output_path.display());
    }
    1
}

// ---------------------------------------------------------------------
// Frame submission
// ---------------------------------------------------------------------

/// Default/unpaced mode (D8): spin on `acquire_texture` until a slot frees
/// up -- never drops, exercises the encoder at 100% duty. Mirrors
/// `submit_paced_frame` in tests/recording_proofs.rs.
fn submit_frame_unpaced(
    session: &mut LiveRecordingSession,
    device: &GpuDevice,
    pattern: &PatternWriter,
    src: &GpuTexture,
    frame_index: u32,
    elapsed: Duration,
) {
    loop {
        if let Some((tex_idx, pool_slot, fence)) = session.acquire_texture() {
            let mut encoder = device.create_encoder("recording-soak frame");
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

/// `--realtime` mode: a single non-blocking `acquire_texture` attempt per
/// frame, exactly like the production capture block
/// (content_pipeline.rs:2549 -- `if let Some(...) = acquire_texture() { .. }
/// else { record_dropped_frame() }`, never blocks). The submitted timestamp
/// is the ACTUAL measured wall-clock elapsed time at submission (not a
/// synthetic grid) -- the true-dress-rehearsal reading of "paces submissions
/// to wall clock" (design §5): production always stamps real elapsed time
/// via `submit_frame`, and `--realtime` is the soak mode built to match it.
fn submit_frame_realtime(
    session: &mut LiveRecordingSession,
    device: &GpuDevice,
    pattern: &PatternWriter,
    src: &GpuTexture,
    frame_index: u32,
    start: Instant,
) -> bool {
    if let Some((tex_idx, pool_slot, fence)) = session.acquire_texture() {
        let mut encoder = device.create_encoder("recording-soak frame");
        pattern.encode(&mut encoder, src, frame_index);
        session.encode_format_conversion(&mut encoder, src, session.pool_texture(tex_idx));
        let fence_for_signal = fence.clone();
        encoder.add_completed_handler(move || fence_for_signal.signal());
        encoder.commit();
        session.submit_frame_at(pool_slot, fence, start.elapsed());
        true
    } else {
        session.record_dropped_frame();
        false
    }
}

/// Push whatever audio is due by now, paced strictly to REAL wall-clock
/// elapsed time since `start` -- never to video frame-loop iteration count.
///
/// Why the wall-clock pacing matters (found via this binary's own self-check
/// soak, not a pre-existing bug): the native audio input is
/// `expectsMediaDataInRealTime = YES` (LiveRecordingPlugin.m) and cannot
/// accept audio faster than real time regardless of how fast unpaced video
/// encodes; pushing a whole take's audio in a burst (e.g. once per unpaced
/// video frame, which can race far ahead of real time) would overwhelm it.
/// Real production audio is never bursted either -- it arrives from an
/// actual CoreAudio callback at real hardware rate -- so pacing to wall
/// clock here is the faithful synthetic equivalent, not a workaround.
///
/// `pushed_frames` advances by what `ringbuf::Producer::push_slice` actually
/// ACCEPTED, not by the intended push amount (BUG-086 root cause, found this
/// session): the ring buffer (bounded, `HeapRb`, ~5s capacity) can transiently
/// fill when a burst of real elapsed time is due at once (this binary's own
/// per-frame call cadence, not the native encoder), and the previous version
/// of this function advanced `pushed_frames` by the intended `to_push`
/// regardless of what `push_slice` actually accepted -- so any shortfall was
/// silently discarded rather than retried on the next call, a permanent loss
/// with nothing recording that it happened. Tracking the
/// real accepted count here self-heals -- the next call's `to_push`
/// naturally includes whatever didn't fit last time -- and the caller
/// reports any residual shortfall against the intended total once, at the
/// end, rather than this function trying to count it call-by-call (a
/// per-call counter double-counts backlog that's still in flight, not yet
/// lost).
fn push_realtime_audio_chunk(
    producer: &mut impl ringbuf::traits::Producer<Item = f32>,
    phase: &mut f32,
    sample_rate: u32,
    channels: u16,
    pushed_frames: &mut u64,
    start: Instant,
) {
    let target_frames = (start.elapsed().as_secs_f64() * sample_rate as f64).floor() as u64;
    let to_push = target_frames.saturating_sub(*pushed_frames);
    if to_push == 0 {
        return;
    }
    let accepted_samples =
        push_audio_chunk(producer, phase, sample_rate, channels, to_push as u32);
    let accepted_frames = accepted_samples as u64 / channels as u64;
    *pushed_frames += accepted_frames;
}

/// Synthesize `num_frames` sample-frames of a 440Hz sine into `producer`.
/// Self-contained duplicate of the equivalent test helper in
/// tests/recording_proofs.rs (that helper is private to the test binary and
/// not part of `proofs.rs`'s reusable surface -- this is genuinely new, tiny
/// glue for this binary's own synthetic audio, not a reinvention of shared
/// infrastructure).
fn push_audio_chunk(
    producer: &mut impl ringbuf::traits::Producer<Item = f32>,
    phase: &mut f32,
    sample_rate: u32,
    channels: u16,
    num_frames: u32,
) -> usize {
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
    producer.push_slice(&buf)
}

// ---------------------------------------------------------------------
// Verification helpers
// ---------------------------------------------------------------------

fn is_strictly_increasing(values: &[i64]) -> bool {
    values.windows(2).all(|w| w[1] > w[0])
}

/// The BUG-085 gate: the decoded frame-index sequence must be exactly
/// `[0..expected_count)` -- no gap, duplicate, or reorder. Returns the first
/// failure, named with numbers, or `None` if the sequence is gap-free.
fn find_first_gap(indices: &[u32], expected_count: u32) -> Option<String> {
    for i in 0..expected_count {
        match indices.get(i as usize) {
            Some(&v) if v == i => continue,
            Some(&v) => {
                return Some(format!(
                    "index gap at decoded position {i}: expected frame index {i}, decoded {v}"
                ));
            }
            None => {
                return Some(format!(
                    "index gap at decoded position {i}: expected frame index {i}, but only \
                     {} frames decoded (missing {} of {expected_count})",
                    indices.len(),
                    expected_count as usize - indices.len(),
                ));
            }
        }
    }
    if indices.len() as u32 > expected_count {
        return Some(format!(
            "decoded {} frames, expected exactly {expected_count} -- extra frames present",
            indices.len()
        ));
    }
    None
}

// ---------------------------------------------------------------------
// Output path + disk space
// ---------------------------------------------------------------------

fn resolve_output_path(args: &Args) -> Result<PathBuf, String> {
    let path = match &args.output {
        Some(p) => p.clone(),
        None => {
            let pid = std::process::id();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            std::env::temp_dir().join(format!(
                "recording-soak-{}x{}-{now}-{pid}.mov",
                args.width, args.height
            ))
        }
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create output directory {}: {e}", parent.display()))?;
    }
    Ok(path)
}

fn estimate_bytes(width: u32, height: u32, total_frames: u32, duration_s: f64, audio: bool) -> u64 {
    let video_bits = width as f64 * height as f64 * total_frames as f64 * PRORES_BITS_PER_PIXEL;
    let audio_bits = if audio { duration_s * AUDIO_BITS_PER_SECOND } else { 0.0 };
    ((video_bits + audio_bits) / 8.0).ceil() as u64
}

/// Free disk space (bytes) for the filesystem containing `path`'s parent
/// directory, via `df -k -P` (POSIX output format -- guaranteed single line,
/// avoids the long-device-name line-wrap that plain `df -k` can produce).
/// Subprocess, not a syscall binding -- consistent with this crate's
/// existing ffprobe/ffmpeg subprocess precedent (`proofs.rs`), no new
/// dependency.
fn free_disk_bytes(path: &Path) -> Result<u64, String> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output = Command::new("df")
        .args(["-k", "-P"])
        .arg(dir)
        .output()
        .map_err(|e| format!("df spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!("df failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let data_line = text
        .lines()
        .nth(1)
        .ok_or_else(|| format!("df output for {} missing data line", dir.display()))?;
    let fields: Vec<&str> = data_line.split_whitespace().collect();
    let available_kb: u64 = fields
        .get(3)
        .ok_or("df output missing Available column")?
        .parse()
        .map_err(|e| format!("df Available column parse failed: {e}"))?;
    Ok(available_kb * 1024)
}
