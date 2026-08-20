//! P2 hot-mute acceptance demo — computed pixel proof (BUG-bk1s).
//!
//! Builds a real headless ContentThread with two stacked generator layers,
//! exports three still frames through the production ExportFrame path, and
//! asserts that muting the top layer hides it from the composite.
//!
//! Why this is a unit-test module inside the binary crate instead of an
//! `tests/` integration test: the harness we need,
//! `headless_harness::headless_content_thread`, is `pub(crate)` and gated
//! `test|perf-soak`. Reaching the real content thread from outside the crate
//! would require widening that visibility and exposing test-only construction
//! as a public API. Keeping the probe here keeps the harness boundary clean.
#![cfg(all(test, target_os = "macos"))]

use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, ClipId, LayerId, PresetTypeId};
use manifold_media::still_exporter::StillFormat;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

const W: u32 = 320;
const H: u32 = 180;
const PATH_A: &str = "/tmp/mute_probe_a.png";
const PATH_B: &str = "/tmp/mute_probe_b.png";
const PATH_C: &str = "/tmp/mute_probe_c.png";

/// Two visually distinct, deterministic generator layers at beat 0.
/// The transport is stopped, so the beat is frozen and every frame is
/// comparable.
///
/// - Bottom: Plasma (pattern 0) — large-area colour that dominates the frame.
/// - Top: BasicShapes (fill=1 solid shape) — a high-contrast overlay.
///
/// Both read the `time` uniform, neither uses per-frame RNG, so at a fixed
/// beat they are frame-stable. BasicShapes defaults to a bright solid shape
/// that visibly changes the composite mean when muted.
fn probe_project() -> (Project, LayerId, ClipId) {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);

    let mut bottom = Layer::new_generator(
        "Plasma".to_string(),
        PresetTypeId::from_string("Plasma".to_string()),
        0,
    );
    bottom.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(8.0)));
    project.timeline.layers.push(bottom);

    let mut top = Layer::new_generator(
        "BasicShapes".to_string(),
        PresetTypeId::from_string("BasicShapes".to_string()),
        1,
    );
    top.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(8.0)));
    let top_layer_id = top.layer_id.clone();
    let top_clip_id = top.clips[0].id.clone();
    project.timeline.layers.push(top);

    (project, top_layer_id, top_clip_id)
}

fn state_channel() -> (Sender<ContentState>, Receiver<ContentState>) {
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
        format: StillFormat::Png,
    });
    assert!(!shutdown, "ExportFrame triggered shutdown");
}

fn cleanup() {
    for p in [PATH_A, PATH_B, PATH_C] {
        let _ = std::fs::remove_file(p);
    }
}

/// Decode an RGBA8 PNG to raw bytes using the `png` crate already in the tree.
fn read_png_rgba8(path: &str) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {}", path, e));
    let reader = std::io::BufReader::new(file);
    let decoder = png::Decoder::new(reader);
    let mut reader = decoder.read_info().unwrap_or_else(|e| panic!("decode {}: {}", path, e));
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("read {}: {}", path, e));
    (w, h, buf)
}

fn frame_mean_rgba(pixels: &[u8]) -> [f64; 4] {
    assert_eq!(pixels.len() % 4, 0);
    let n = pixels.len() / 4;
    assert!(n > 0);
    let mut sums = [0u64; 4];
    for chunk in pixels.chunks_exact(4) {
        sums[0] += chunk[0] as u64;
        sums[1] += chunk[1] as u64;
        sums[2] += chunk[2] as u64;
        sums[3] += chunk[3] as u64;
    }
    [
        sums[0] as f64 / (n as f64 * 255.0),
        sums[1] as f64 / (n as f64 * 255.0),
        sums[2] as f64 / (n as f64 * 255.0),
        sums[3] as f64 / (n as f64 * 255.0),
    ]
}

fn wait_for_png(path: &str, timeout: Duration) -> (u32, u32, Vec<u8>) {
    let start = Instant::now();
    loop {
        if Path::new(path).exists() {
            // Give the encoder thread a moment to finish closing the file.
            std::thread::sleep(Duration::from_millis(10));
            return read_png_rgba8(path);
        }
        if start.elapsed() >= timeout {
            panic!("timed out waiting for {}", path);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn mute_visibility_pixel_probe() {
    cleanup();

    let (project, top_layer_id, top_clip_id) = probe_project();
    let mut ct = headless_content_thread(project, W, H);
    let (state_tx, _state_rx) = state_channel();

    // Pin engine time to the frame count so wall-clock pauses between
    // assertions don't advance the beat or compress driver-based motion.
    ct.timer.set_frame_clocked(true);

    // Let pipelines warm and the reconciler settle on the stopped playhead.
    drive_frames(&mut ct, 10, &state_tx);

    // A: both layers visible.
    request_export(&mut ct, PATH_A);
    // ExportFrame needs two ticks: submit readback, then read + encode.
    drive_frames(&mut ct, 4, &state_tx);
    let (w_a, h_a, img_a) = wait_for_png(PATH_A, Duration::from_secs(30));

    // B: mute the top layer.
    let top_layer_id_mute = top_layer_id.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |p| {
        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&top_layer_id_mute) {
            layer.is_muted = true;
        }
    })));
    drive_frames(&mut ct, 5, &state_tx);
    request_export(&mut ct, PATH_B);
    drive_frames(&mut ct, 4, &state_tx);
    let (w_b, h_b, img_b) = wait_for_png(PATH_B, Duration::from_secs(30));

    // C: remove the top layer's clip entirely.
    let top_layer_id2 = top_layer_id.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |p| {
        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&top_layer_id2) {
            layer.clips.retain(|c| c.id != top_clip_id);
        }
    })));
    drive_frames(&mut ct, 5, &state_tx);
    request_export(&mut ct, PATH_C);
    drive_frames(&mut ct, 4, &state_tx);
    let (w_c, h_c, img_c) = wait_for_png(PATH_C, Duration::from_secs(30));

    assert_eq!((w_a, h_a), (w_b, h_b));
    assert_eq!((w_a, h_a), (w_c, h_c));

    let mean_a = frame_mean_rgba(&img_a);
    let mean_b = frame_mean_rgba(&img_b);
    let mean_c = frame_mean_rgba(&img_c);

    // ±1/255 per-channel tolerance: muting and deleting the top layer must
    // produce the same visual result.
    let tolerance = 1.0 / 255.0;
    for ch in 0..4 {
        let diff_bc = (mean_b[ch] - mean_c[ch]).abs();
        assert!(
            diff_bc <= tolerance,
            "B/C channel {} differs by {} (>{}) — muted layer still visible?",
            ch,
            diff_bc,
            tolerance
        );
    }

    // The top layer must actually contribute to A; a margin large enough to
    // survive anti-aliasing / tonemap noise but small enough that any real
    // overlay clears it. 0.05 (~13/255) is comfortably above the tolerance.
    let min_visible_margin = 0.05;
    let mut visible_channels = 0;
    for ch in 0..4 {
        if (mean_a[ch] - mean_b[ch]).abs() > min_visible_margin {
            visible_channels += 1;
        }
    }
    assert!(
        visible_channels >= 1,
        "top layer made no detectable contribution to the composite: A={:?} B={:?}",
        mean_a,
        mean_b
    );

    eprintln!(
        "[mute-visibility-probe] means A={:?} B={:?} C={:?}",
        mean_a, mean_b, mean_c
    );
}
