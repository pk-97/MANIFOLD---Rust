//! RT washout probe: load project, drive play then pause, capture RT textures
//! at sampled frames to find where traced reflection hits die after stillness.
//!
//! MANIFOLD_RT_PROBE=1 to enable. Writes PNGs + stats to /tmp/rt_washout/.
//!
//!   cargo run --features perf-soak --bin manifold -- manifold rt-washout <project>

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use manifold_renderer::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, readback_raw_halves,
};
use manifold_renderer::node_graph::primitives::{
    WashoutCapture, WASHOUT_CAPTURE_FRAMES, WASHOUT_CAPTURE_QUEUE, WASHOUT_FRAME,
};
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

/// Read back a captured texture from GPU and write analysis.
fn process_capture(cap: &WashoutCapture, device: &manifold_gpu::GpuDevice, out_dir: &std::path::Path) {
    let raw = readback_raw_halves(device, &cap.tex, cap.w, cap.h);
    let pixel_count = (cap.w * cap.h) as usize;
    let mut rgba_f32 = vec![0.0f32; pixel_count * 4];

    // Decode pairs of f16 bytes → f32 RGBA.
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base + 1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base + 2], raw[base + 3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base + 4], raw[base + 5]])).to_f32();
        let a = half::f16::from_bits(u16::from_le_bytes([raw[base + 6], raw[base + 7]])).to_f32();
        rgba_f32[i * 4] = r;
        rgba_f32[i * 4 + 1] = g;
        rgba_f32[i * 4 + 2] = b;
        rgba_f32[i * 4 + 3] = a;
    }

    // Hit-fraction: alpha channel is hit distance for refl/irr textures.
    let mut n_hits = 0usize;
    let mut n_total = 0usize;
    let mut sum_luma = 0.0f64;
    let mut sum_luma_sq = 0.0f64;
    for px in rgba_f32.chunks_exact(4) {
        n_total += 1;
        let hit_dist = px[3];
        if hit_dist > 0.0 && hit_dist < 1e6 && !hit_dist.is_nan() {
            n_hits += 1;
        }
        let r = px[0].max(0.0);
        let g = px[1].max(0.0);
        let b = px[2].max(0.0);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        sum_luma += luma as f64;
        sum_luma_sq += (luma * luma) as f64;
    }
    let hit_frac = if n_total > 0 { n_hits as f64 / n_total as f64 } else { 0.0 };
    let mean_luma = if n_total > 0 { sum_luma / n_total as f64 } else { 0.0 };
    let var_luma = if n_total > 0 {
        (sum_luma_sq / n_total as f64) - (mean_luma * mean_luma)
    } else {
        0.0
    };

    // Tonemap for PNG: encode to srgb-like 8-bit.
    let mut rgba8 = Vec::with_capacity(pixel_count * 4);
    for px in rgba_f32.chunks_exact(4) {
        let tone = |v: f32| linear_to_srgb8(v.max(0.0));
        rgba8.push(tone(px[0]));
        rgba8.push(tone(px[1]));
        rgba8.push(tone(px[2]));
        // Alpha channel as hit-distance visualization: scale 0..1 → 0..255.
        let a_vis = (px[3].clamp(0.0, 1.0) * 255.0) as u8;
        rgba8.push(a_vis);
    }

    // Write PNG.
    let png_path = out_dir.join(format!("f{:04}_{}.png", cap.frame, cap.label));
    let png_bytes = encode_rgba8_png(&rgba8, cap.w, cap.h);
    std::fs::write(&png_path, &png_bytes).unwrap_or_else(|e| {
        eprintln!("[WASHOUT] failed to write {}: {e}", png_path.display());
    });

    eprintln!(
        "[WASHOUT] {} frame={} dims={}x{} hit_frac={:.6} mean_luma={:.6} var_luma={:.6} png={}",
        cap.label, cap.frame, cap.w, cap.h, hit_frac, mean_luma, var_luma, png_path.display(),
    );
}

/// Drain capture queue, reading back every pending capture.
fn drain_captures(device: &manifold_gpu::GpuDevice) {
    let caps = {
        let mut q = WASHOUT_CAPTURE_QUEUE.lock().unwrap();
        std::mem::take(&mut *q)
    };
    if caps.is_empty() {
        return;
    }
    let out_dir = PathBuf::from("/tmp/rt_washout");
    let _ = std::fs::create_dir_all(&out_dir);
    for cap in &caps {
        process_capture(cap, device, &out_dir);
    }
}

/// Set capture frames relative to the current WASHOUT_FRAME baseline.
fn set_capture_frames(relative_frames: &[u32]) {
    let baseline = WASHOUT_FRAME.load(Ordering::Relaxed);
    let mut frames = WASHOUT_CAPTURE_FRAMES.lock().unwrap();
    frames.clear();
    for rf in relative_frames {
        frames.push(baseline + rf);
    }
    eprintln!(
        "[WASHOUT] capture frames set: {:?} -> {:?}",
        relative_frames,
        *frames,
    );
}

/// Entry.
pub fn run(args: &[String]) -> ! {
    // SAFETY: disposable probe — safe single-threaded access at startup.
    unsafe { std::env::set_var("MANIFOLD_RT_PROBE", "1"); }

    let project_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: manifold rt-washout <project.manifold>");
            std::process::exit(2);
        }
    };
    if !project_path.exists() {
        eprintln!("project not found: {}", project_path.display());
        std::process::exit(1);
    }

    println!("=== RT WASHOUT PROBE ===");
    println!("path: {}", project_path.display());

    // Load project.
    let real_project = match manifold_io::loader::load_project_with(
        &project_path,
        crate::project_io::install_embedded_presets,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FAILED to load project: {e}");
            std::process::exit(1);
        }
    };
    let frame_rate = real_project.settings.frame_rate as f64;
    let w = real_project.settings.output_width.max(1) as u32;
    let h = real_project.settings.output_height.max(1) as u32;
    println!("output={w}x{h} fps={frame_rate}");

    for (i, layer) in real_project.timeline.layers.iter().enumerate() {
        println!("  layer[{i}] type={:?}", layer.gen_params().map(|g| g.generator_type().clone().as_str().to_string()));
    }

    // Build ContentThread and load project.
    let empty_project = manifold_core::project::Project::default();
    let mut ct = headless_content_thread(empty_project, w, h);
    ct.timer.set_target_fps(frame_rate);
    crate::content_thread::apply_realtime_thread_policy(frame_rate);
    ct.handle_command(ContentCommand::LoadProject(Box::new(real_project)));

    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("washout-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .expect("spawn drain");

    // ── Phase 1: Rotating (play) ──
    println!("=== Phase 1: Rotating (60 frames) ===");
    ct.handle_command(ContentCommand::Play);
    // Capture mid-rotation (frame 30) and end-of-rotation (frame 59).
    set_capture_frames(&[30, 59]);

    for _ in 0..60 {
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
        if let Some(dev) = ct.content_pipeline.native_device() {
            drain_captures(dev);
        }
    }

    // ── Phase 2: Still (paused) ──
    println!("=== Phase 2: Paused (300 frames) ===");
    ct.handle_command(ContentCommand::Stop);
    // Capture at +10, +30, +90, +299 frames post-pause.
    set_capture_frames(&[10, 30, 90, 299]);

    for _ in 0..300 {
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
        if let Some(dev) = ct.content_pipeline.native_device() {
            drain_captures(dev);
        }
    }

    // Final flush.
    if let Some(dev) = ct.content_pipeline.native_device() {
        drain_captures(dev);
    }

    drop(state_tx);
    drain.join().expect("drain join");
    println!("=== DONE ===");
    std::process::exit(0);
}
