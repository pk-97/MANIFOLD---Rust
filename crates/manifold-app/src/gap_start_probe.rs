//! Gap-start black-frame probe (session 2026-08-27).
//!
//! Drives the REAL headless ContentThread with Peter's fbTest repro project
//! (38 generator clips, 0.25 beats on / 0.25 beats off at 120bpm ≈ 7.5
//! frames of clip alternating with 7.5 frames of gap) and reads back the
//! compositor output texture around each clip boundary. Peter's bug: with
//! StylizedFeedback enabled on the layer, the first frame after each gap
//! renders black.
//!
//! Export cadence: ExportFrame is single-slot (one in-flight at a time).
//! Each export needs ≥3 ticks (submit readback → poll → encode thread writes
//! PNG). The proven pattern (from mute_visibility_probe) is:
//!   request_export → drive_frames(4) → wait_for_png
//! The PNG captures the render from the first tick after the export command.
//! This limits us to one capture per4 ticks; we position captures to
//! straddle each gap→clip boundary.
#![cfg(all(test, target_os = "macos"))]

use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use manifold_core::{Beats, Seconds};

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

const FB_TEST: &str = "/Users/peterkiemann/Downloads/fbTest.manifold";

fn state_channel() -> (Sender<ContentState>, crossbeam_channel::Receiver<ContentState>) {
    crossbeam_channel::unbounded()
}

fn drive_frames(ct: &mut ContentThread, n: usize, state_tx: &Sender<ContentState>) {
    for _ in 0..n {
        ct.tick_frame(state_tx);
    }
}

fn request_export(ct: &mut ContentThread, path: &str) {
    let shutdown = ct.handle_command(ContentCommand::ExportFrame {
        path: path.to_string(),
        format: manifold_media::still_exporter::StillFormat::Png,
    });
    assert!(!shutdown, "ExportFrame triggered shutdown");
}

fn read_png_rgba8(path: &str) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {}", path, e));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().unwrap_or_else(|e| panic!("decode {}: {}", path, e));
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("read {}: {}", path, e));
    buf
}

fn frame_mean_rgba(pixels: &[u8]) -> [f64; 4] {
    let n = pixels.len() / 4;
    let mut sums = [0u64; 4];
    for chunk in pixels.chunks_exact(4) {
        for (i, s) in sums.iter_mut().enumerate() {
            *s += chunk[i] as u64;
        }
    }
    [
        sums[0] as f64 / (n as f64 * 255.0),
        sums[1] as f64 / (n as f64 * 255.0),
        sums[2] as f64 / (n as f64 * 255.0),
        sums[3] as f64 / (n as f64 * 255.0),
    ]
}

/// Wait for a PNG file to appear and return its mean RGBA.
fn wait_for_png(path: &str) -> [f64; 4] {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !Path::new(path).exists() {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {path}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Margin for the encode thread to finish closing the file.
    std::thread::sleep(Duration::from_millis(20));
    frame_mean_rgba(&read_png_rgba8(path))
}

/// Capture one frame: export → drive4 ticks → read PNG.
/// Returns (actual_tick, mean_rgba). The PNG captures the render from the
/// first tick after the export command (i.e. tick+1 relative to when the
/// command was issued).
fn capture_frame(
    ct: &mut ContentThread,
    state_tx: &Sender<ContentState>,
    path: &str,
    tick_before_export: usize,
) -> (usize, [f64; 4]) {
    request_export(ct, path);
    drive_frames(ct, 4, state_tx);
    let m = wait_for_png(path);
    // The render was captured on the first tick after the export command.
    // The export command was issued at tick_before_export, so the captured
    // render is from tick_before_export + 1.
    (tick_before_export + 1, m)
}

#[test]
fn gap_start_black_frame_probe() {
    // ── Plasma control ───────────────────────────────────────────────
    let ctrl_dir = "/tmp/gap_probe_ctrl";
    let _ = std::fs::remove_dir_all(ctrl_dir);
    std::fs::create_dir_all(ctrl_dir).unwrap();
    {
        let mut project = manifold_core::project::Project::default();
        project.settings.bpm = manifold_core::Bpm(120.0);
        let mut layer = manifold_core::layer::Layer::new_generator(
            "Plasma".to_string(),
            manifold_core::PresetTypeId::from_string("Plasma".to_string()),
            0,
        );
        layer
            .clips
            .push(manifold_core::clip::TimelineClip::new_generator(
                Beats(0.0),
                Beats(8.0),
            ));
        project.timeline.layers.push(layer);
        let mut ct: ContentThread = headless_content_thread(project, 320, 180);
        let (state_tx, _rx) = state_channel();
        ct.timer.set_frame_clocked(true);
        let (_, m) = capture_frame(&mut ct, &state_tx, &format!("{ctrl_dir}/ctrl.png"), 0);
        eprintln!("[gap-probe] control (Plasma) mean: {m:?}");
        assert!(
            m[0] + m[1] + m[2] > 0.01,
            "control rendered black — the probe drive is broken"
        );
    }

    // ── fbTest probe ─────────────────────────────────────────────────
    assert!(
        Path::new(FB_TEST).exists(),
        "fbTest fixture missing at {FB_TEST}"
    );
    let project = manifold_io::loader::load_project(Path::new(FB_TEST))
        .expect("fbTest loads through the real loader");
    let mut ct: ContentThread = headless_content_thread(project, 320, 180);
    let (state_tx, _rx) = state_channel();
    ct.timer.set_frame_clocked(true);

    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.handle_command(ContentCommand::Play);

    let dir = "/tmp/gap_probe";
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();

    // Clip cadence: 15 frames per cycle (7.5 clip / 7.5 gap at 120 bpm).
    // Clip starts at frames 0, 15, 30, 45.
    //
    // Export cadence: 4 ticks per capture (proven pattern). The PNG
    // captures the render from the first tick after the export command.
    //
    // Strategy: warm up5 ticks, then12 captures ×4 ticks = 48 ticks.
    // Captured ticks: 6, 10, 14, 18, 22, 26, 30, 34, 38, 42, 46, 50.
    //
    // Boundary windows:
    //   Window1 (frame15): tick14 (gap), tick18 (mid-clip)
    //   Window2 (frame30): tick26 (gap), tick30 (clip start!), tick34 (mid-clip)
    //   Window3 (frame45): tick42 (gap), tick46 (clip+1), tick50 (mid-clip)

    drive_frames(&mut ct, 5, &state_tx); // warmup: ticks 0-4

    // Each capture_frame exports at the current tick, drives4 ticks, and
    // the PNG captures the first of those4 ticks. We track the tick
    // count so filenames reflect the actual captured frame.
    let mut results: Vec<(usize, &str, [f64; 4])> = Vec::new();
    // Each capture_frame exports at the current tick, drives4 ticks, and
    // the PNG captures the first of those4 ticks (tick+1).
    // After warmup at tick5, the sequence is:
    //   export at 5 → captures tick6, ends at tick9
    //   export at 9 → captures tick10, ends at tick13
    //   ...
    //   export at 45 → captures tick46, ends at tick49
    //   export at 49 → captures tick50, ends at tick53
    let capture_specs: Vec<(&str, &str)> = vec![
        ("warmup1", "warmup"),
        ("warmup2", "warmup"),
        ("gap_before_clip1", "gap→clip@15"),
        ("clip1_mid", "mid-clip"),
        ("gap_before_clip2", "gap→clip@30"),
        ("gap_mid2", "gap"),
        ("clip2_start", "clip start@30"),
        ("clip2_mid", "mid-clip"),
        ("gap_before_clip3", "gap→clip@45"),
        ("gap_mid3", "gap"),
        ("clip3_start", "clip start@45"),
        ("clip3_mid", "mid-clip"),
    ];

    let mut tick = 5usize; // current tick (warmup drove 0-4)
    for (label, desc) in &capture_specs {
        let path = format!("{dir}/{label}.png");
        let (captured_tick, m) = capture_frame(&mut ct, &state_tx, &path, tick);
        results.push((captured_tick, desc, m));
        tick += 4;
    }

    eprintln!("[gap-probe] frame: mean RGBA (clip starts at 0, 15, 30, 45)");
    for &(captured_tick, desc, m) in &results {
        eprintln!(
            "[gap-probe] tick {captured_tick:2} ({desc}): [{:.4} {:.4} {:.4} {:.4}]",
            m[0], m[1], m[2], m[3],
        );
    }

    // ── Bug detection ────────────────────────────────────────────────
    // The bug signature: a clip-start frame reads near-zero RGB while
    // subsequent frames show content. Check each boundary window.
    let gap_means: Vec<f64> = results
        .iter()
        .filter(|(t, _, _)| *t == 14 || *t == 26 || *t == 42)
        .map(|(_, _, m)| m[0] + m[1] + m[2])
        .collect();
    let clip_start_means: Vec<f64> = results
        .iter()
        .filter(|(t, _, _)| *t == 30 || *t == 46)
        .map(|(_, _, m)| m[0] + m[1] + m[2])
        .collect();
    let clip_mid_means: Vec<f64> = results
        .iter()
        .filter(|(t, _, _)| *t == 18 || *t == 34 || *t == 50)
        .map(|(_, _, m)| m[0] + m[1] + m[2])
        .collect();

    eprintln!("[gap-probe] gap RGB sums:       {gap_means:?}");
    eprintln!("[gap-probe] clip-start RGB sums: {clip_start_means:?}");
    eprintln!("[gap-probe] clip-mid RGB sums:   {clip_mid_means:?}");

    // Sanity: gap frames should be near-black (no content).
    for (i, &sum) in gap_means.iter().enumerate() {
        assert!(
            sum < 0.1,
            "gap frame {i} (expected black) has RGB sum {sum:.4}"
        );
    }

    let _ = Seconds::ZERO;
}
