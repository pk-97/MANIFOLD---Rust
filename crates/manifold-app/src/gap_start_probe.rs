//! Gap-start black-frame probe — bisect edition (session 2026-08-27).
//!
//! Drives the REAL headless ContentThread with Peter's fbTest repro project
//! and bisects which texture is black on the black frame: the clip's generator
//! output, the layer scratch buffer, or the chain's output.
//!
//! Export cadence: ExportFrame is single-slot (one in-flight at a time).
//! We use the proven pattern (mute_visibility_probe): export → drive4 ticks →
//! read PNG. For the bisect, we additionally do synchronous GPU readbacks of
//! the three candidate textures on every tick 43..49.
#![cfg(all(test, target_os = "macos"))]

use std::path::Path;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use manifold_core::{Beats, LayerId, Seconds};
use manifold_gpu::GpuTexture;

use manifold_renderer::generator_renderer::GeneratorRenderer;

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
    std::thread::sleep(Duration::from_millis(20));
    frame_mean_rgba(&read_png_rgba8(path))
}

/// Synchronous GPU readback of a texture → mean RGBA.
///
/// Uses the same pattern as `readback_texture_via_buffer` in manifold-gpu:
/// create shared buffer, blit-copy on a fresh encoder, commit+wait, read
/// mapped pointer. The device reference must NOT be held across ticks —
/// grab it fresh each call.
fn readback_mean_rgba(device: &manifold_gpu::GpuDevice, tex: &GpuTexture) -> [f64; 4] {
    let bpp = tex.format.bytes_per_pixel();
    let w = tex.width;
    let h = tex.height;
    let bytes_per_row = bpp * w;
    let total = (bytes_per_row * h) as u64;
    let shared_buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("bisect-readback");
    enc.copy_texture_to_buffer(tex, &shared_buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = shared_buf
        .mapped_ptr()
        .expect("shared buffer must have mapped pointer");

    // Decode to mean RGBA. Format is Rgba16Float (8 bpp): 4× f16 per pixel.
    let mut sums = [0.0f64; 4];
    let n = (w * h) as f64;
    for row in 0..h as usize {
        let src_row = row * bytes_per_row as usize;
        for col in 0..w as usize {
            let src_px = src_row + col * bpp as usize;
            for (ch, s) in sums.iter_mut().enumerate() {
                let bits = unsafe {
                    let lo = *ptr.add(src_px + ch * 2);
                    let hi = *ptr.add(src_px + ch * 2 + 1);
                    u16::from_le_bytes([lo, hi])
                };
                let f = f16_to_f32(bits);
                *s += f as f64;
            }
        }
    }
    [sums[0] / n, sums[1] / n, sums[2] / n, sums[3] / n]
}

/// IEEE 754 half-precision → f32.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as i32;
    let frac = (bits & 0x3ff) as u32;
    if exp == 0 {
        if frac == 0 {
            f32::from_bits(sign << 31)
        } else {
            // Subnormal
            let mut val = frac as f32 / 1024.0;
            let mut shift = 1;
            while shift < 10 && (frac & (1 << shift)) == 0 {
                shift += 1;
            }
            val /= (1u32 << (shift - 1)) as f32;
            val *= 2f32.powf(-14.0);
            if sign != 0 { -val } else { val }
        }
    } else if exp == 31 {
        if frac == 0 {
            if sign != 0 { f32::NEG_INFINITY } else { f32::INFINITY }
        } else {
            f32::NAN
        }
    } else {
        f32::from_bits((sign << 31) | (((exp + 112) as u32) << 23) | (frac << 13))
    }
}

/// Capture one frame: export → drive4 ticks → read PNG.
fn capture_frame(
    ct: &mut ContentThread,
    state_tx: &Sender<ContentState>,
    path: &str,
    tick_before_export: usize,
) -> (usize, [f64; 4]) {
    request_export(ct, path);
    drive_frames(ct, 4, state_tx);
    let m = wait_for_png(path);
    (tick_before_export + 1, m)
}

/// Find the layer_id of the layer with StylizedFeedback effects.
fn find_stylized_feedback_layer(ct: &ContentThread) -> Option<(LayerId, Vec<String>)> {
    let project = ct.engine.project()?;
    for layer in &project.timeline.layers {
        if let Some(effects) = &layer.effects {
            for fx in effects {
                if fx.effect_type().as_str().contains("StylizedFeedback")
                    || fx.effect_type().as_str().contains("stylized")
                    || fx.effect_type().as_str().contains("feedback")
                {
                    let clip_ids: Vec<String> = layer.clips.iter().map(|c| c.id.to_string()).collect();
                    return Some((layer.layer_id.clone(), clip_ids));
                }
            }
        }
    }
    // Fallback: just find any layer with effects
    for layer in &project.timeline.layers {
        if let Some(effects) = &layer.effects
            && !effects.is_empty()
        {
            let clip_ids: Vec<String> = layer.clips.iter().map(|c| c.id.to_string()).collect();
            return Some((layer.layer_id.clone(), clip_ids));
        }
    }
    None
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

    // Find the layer with StylizedFeedback effects
    let (layer_id, clip_ids) = find_stylized_feedback_layer(&ct)
        .expect("fbTest must have a layer with StylizedFeedback or effects");
    let layer_id_str = layer_id.to_string();
    eprintln!("[gap-probe] target layer: {layer_id_str}, clips: {clip_ids:?}");

    // Clip cadence: 15 frames per cycle (7.5 clip / 7.5 gap at 120 bpm).
    // Clip starts at frames 0, 15, 30, 45.
    //
    // Bisect strategy: capture EVERY tick 43..49 (the clip-4 boundary at
    // frame 45). For each tick, after rendering, synchronously readback:
    //   (a) clip generator output texture
    //   (b) layer scratch buffer
    //   (c) chain output texture
    // Plus export a PNG for the full compositor output.
    //
    // The export cadence (4 ticks per capture) means we can only export
    // one PNG every4 ticks. So we export at tick 43 (captures tick44)
    // and tick 47 (captures tick48). For the intermediate ticks, we
    // do in-process readback only.

    // Warm up: drive to tick 42 (before the bisect window)
    drive_frames(&mut ct, 42, &state_tx);

    // Bisect: capture ticks 43..49 (7 ticks)
    eprintln!("[gap-probe] bisect window: ticks 43..49");
    eprintln!("[gap-probe] tick | output_mean | clip_gen_mean | scratch_mean | chain_mean");

    let mut prev_output_identity: Option<usize> = None;

    for tick in 43..=49 {
        // Drive one tick to render this frame
        ct.tick_frame(&state_tx);

        // Now readback the three candidate textures
        let device = ct
            .content_pipeline
            .native_device()
            .expect("native device must be available");

        // (a) Clip generator output texture
        let clip_gen_mean = if let Some(clip_id) = clip_ids.first() {
            let renderers = ct.engine.renderers();
            let clip_tex = renderers.iter().find_map(|r| {
                if let Some(gr) = r
                    .as_any()
                    .downcast_ref::<GeneratorRenderer>()
                {
                    gr.get_clip_texture(clip_id)
                } else {
                    None
                }
            });
            if let Some(tex) = clip_tex {
                readback_mean_rgba(device, tex)
            } else {
                [f64::NAN; 4]
            }
        } else {
            [f64::NAN; 4]
        };

        // (b) Layer scratch buffer
        let scratch_mean = ct
            .content_pipeline
            .layer_scratch_texture(&layer_id_str)
            .map(|tex| readback_mean_rgba(device, tex))
            .unwrap_or([f64::NAN; 4]);

        // (c) Chain output texture
        let chain_mean = ct
            .content_pipeline
            .chain_output_texture(&layer_id_str)
            .map(|tex| readback_mean_rgba(device, tex))
            .unwrap_or([f64::NAN; 4]);

        // (d) Export output (compositor final)
        let output_tex = ct.content_pipeline.export_output_texture();
        let output_mean = readback_mean_rgba(device, output_tex);

        // (e) Chain-internal bisect: source, step outputs, output slot identity
        let chain_info = ct.content_pipeline.chain_debug_info(&layer_id_str);
        let output_changed = if let Some(ref info) = chain_info {
            match prev_output_identity {
                Some(prev) => info.output.map(GpuTexture::identity_key) != Some(prev),
                None => false,
            }
        } else {
            false
        };

        if let Some(ref info) = chain_info {
            let src_mean = info
                .source
                .map(|tex| readback_mean_rgba(device, tex))
                .unwrap_or([f64::NAN; 4]);
            let out_mean = info
                .output
                .map(|tex| readback_mean_rgba(device, tex))
                .unwrap_or([f64::NAN; 4]);
            eprintln!(
                "[gap-probe] {tick:3}  chain src=[{:.4} {:.4} {:.4} {:.4}] out=[{:.4} {:.4} {:.4} {:.4}] out_id={:x?} changed={output_changed}",
                src_mean[0], src_mean[1], src_mean[2], src_mean[3],
                out_mean[0], out_mean[1], out_mean[2], out_mean[3],
                info.output.map(GpuTexture::identity_key),
            );
            for step in &info.step_outputs {
                let step_mean = step
                    .texture
                    .map(|tex| readback_mean_rgba(device, tex))
                    .unwrap_or([f64::NAN; 4]);
                eprintln!(
                    "[gap-probe] {tick:3}  step {:2} port={:<20} res={:?} mean=[{:.4} {:.4} {:.4} {:.4}]",
                    step.step_idx, step.port_name, step.resource_id.0,
                    step_mean[0], step_mean[1], step_mean[2], step_mean[3],
                );
            }
            prev_output_identity = info.output.map(GpuTexture::identity_key);
        }

        // Release device before next tick
        let _ = device;

        eprintln!(
            "[gap-probe] {tick:3}  | [{:.4} {:.4} {:.4} {:.4}] | [{:.4} {:.4} {:.4} {:.4}] | [{:.4} {:.4} {:.4} {:.4}] | [{:.4} {:.4} {:.4} {:.4}]",
            output_mean[0], output_mean[1], output_mean[2], output_mean[3],
            clip_gen_mean[0], clip_gen_mean[1], clip_gen_mean[2], clip_gen_mean[3],
            scratch_mean[0], scratch_mean[1], scratch_mean[2], scratch_mean[3],
            chain_mean[0], chain_mean[1], chain_mean[2], chain_mean[3],
        );
    }

    // Drive remaining ticks for any pending exports
    drive_frames(&mut ct, 4, &state_tx);

    // Report summary
    eprintln!("[gap-probe] bisect complete — check table above for which texture is black on the black frame");

    let _ = Seconds::ZERO;
}
