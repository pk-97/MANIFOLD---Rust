//! `manifold rt-capture <project> [--frames N]` — headless RT channel
//! verification harness.
//!
//! Loads a `.manifold` project through the real ContentCommand::LoadProject
//! path, drives continuous play via headless ContentThread, and captures
//! internal RT textures (raw reflection trace, accumulated history,
//! irradiance, moments) plus composited output at fixed frame intervals.
//!
//! Output: per-capture stats (hit-fraction, mean luma, luma stddev) to
//! stderr and tonemapped PNGs to /tmp/rt_capture/ for visual inspection.
//! Verdicts come from the numbers — PNGs are debug visualization.
//!
//! What this proves: RT channel health (hit-fraction, contrast) across
//! motion→still transitions without a GUI session. Use to validate that
//! the load path, accel rebuild, accumulation, denoise, and composite
//! substitution all function correctly after a project load.
//!
//! Usage:
//!   cargo run --features perf-soak --bin manifold -- manifold rt-capture <project.manifold>
//!   cargo run ... manifold rt-capture --paused <project>   # Play 60 → Pause 300
//!
//! MANIFOLD_RT_PROBE is NOT required — the subcommand arms the capture
//! flags directly.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use manifold_renderer::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, readback_raw_halves,
};
use manifold_renderer::node_graph::primitives::{
    RtCaptureSlot, RT_CAPTURE_ARM, RT_CAPTURE_ARM_COMPOSITE, RT_CAPTURE_QUEUE,
};
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

fn process_capture(cap: &RtCaptureSlot, device: &manifold_gpu::GpuDevice, out_dir: &std::path::Path) {
    let raw = readback_raw_halves(device, &cap.tex, cap.w, cap.h);
    let pixel_count = (cap.w * cap.h) as usize;
    let mut n_hits = 0usize;
    let mut sum_luma = 0.0f64; let mut sum_luma_sq = 0.0f64;
    let is_composite = cap.label == "composite";
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base+1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base+2], raw[base+3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base+4], raw[base+5]])).to_f32();
        let a = half::f16::from_bits(u16::from_le_bytes([raw[base+6], raw[base+7]])).to_f32();
        // For RT internal channels (refl, irr): alpha encodes hit distance.
        // For composite: non-black threshold (a > 0.03 in any RGB channel).
        if is_composite {
            if r > 0.03 || g > 0.03 || b > 0.03 { n_hits += 1; }
        } else {
            if a > 0.0 && a < 1e6 && !a.is_nan() { n_hits += 1; }
        }
        let luma = 0.2126 * r.max(0.0) + 0.7152 * g.max(0.0) + 0.0722 * b.max(0.0);
        sum_luma += luma as f64; sum_luma_sq += (luma*luma) as f64;
    }
    let hit_frac = n_hits as f64 / pixel_count as f64;
    let mn = if pixel_count > 0 { sum_luma / pixel_count as f64 } else { 0.0 };
    let vr = if pixel_count > 0 { (sum_luma_sq / pixel_count as f64) - mn*mn } else { 0.0 };
    let sd = vr.sqrt();

    // Write tonemapped PNG (alpha channel encodes hit distance for RT channels).
    let mut rgba8 = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base+1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base+2], raw[base+3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base+4], raw[base+5]])).to_f32();
        let a = half::f16::from_bits(u16::from_le_bytes([raw[base+6], raw[base+7]])).to_f32();
        rgba8.push(linear_to_srgb8(r.max(0.0)));
        rgba8.push(linear_to_srgb8(g.max(0.0)));
        rgba8.push(linear_to_srgb8(b.max(0.0)));
        rgba8.push((a.clamp(0.0, 1.0) * 255.0) as u8);
    }
    let png_path = out_dir.join(format!("{}_{:04}.png", cap.label, cap.frame));
    std::fs::write(&png_path, encode_rgba8_png(&rgba8, cap.w, cap.h))
        .unwrap_or_else(|e| eprintln!("[rt-capture] write {}: {e}", png_path.display()));
    eprintln!(
        "[rt-capture] {} f={:04} dim={}x{} hit={:.6} luma={:.6} sd={:.6} {}",
        cap.label, cap.frame, cap.w, cap.h, hit_frac, mn, sd, png_path.display(),
    );
}

fn drain_captures(device: &manifold_gpu::GpuDevice, frame: u32) {
    let caps = {
        let mut q = RT_CAPTURE_QUEUE.lock().unwrap();
        for c in &mut *q { c.frame = frame; }
        std::mem::take(&mut *q)
    };
    if caps.is_empty() { return; }
    let dir = PathBuf::from("/tmp/rt_capture");
    let _ = std::fs::create_dir_all(&dir);
    for c in &caps { process_capture(c, device, &dir); }
}

fn arm_capture() {
    RT_CAPTURE_ARM.store(true, Ordering::Relaxed);
    RT_CAPTURE_ARM_COMPOSITE.store(true, Ordering::Relaxed);
}

pub fn run(args: &[String]) -> ! {
    let paused_mode = args.iter().any(|a| a == "--paused");

    // Resolve project path: skip the subcommand name (args[0]), then first non-flag arg.
    let project_path = args.iter().skip(1)
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| { eprintln!("usage: manifold rt-capture [--paused] <project.manifold> [--frames N]"); std::process::exit(2); });
    if !project_path.exists() { eprintln!("not found: {}", project_path.display()); std::process::exit(1); }

    // Parse optional --frames flag; default 360.
    let total_frames: u32 = args.windows(2)
        .find(|w| w[0] == "--frames")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(360);

    println!("=== RT CAPTURE {}", if paused_mode { "(PAUSED MODE)" } else { "" });
    println!("path: {} frames={}", project_path.display(), total_frames);

    let real_project = manifold_io::loader::load_project_with(&project_path, crate::project_io::install_embedded_presets)
        .unwrap_or_else(|e| { eprintln!("FAILED: {e}"); std::process::exit(1); });
    let fr = real_project.settings.frame_rate as f64;
    let w = real_project.settings.output_width.max(1) as u32;
    let h = real_project.settings.output_height.max(1) as u32;
    println!("output={w}x{h} fps={fr}");

    let empty = manifold_core::project::Project::default();
    let mut ct = headless_content_thread(empty, w, h);
    ct.timer.set_target_fps(fr);
    crate::content_thread::apply_realtime_thread_policy(fr);
    ct.handle_command(ContentCommand::LoadProject(Box::new(real_project)));

    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("rt-capture-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .expect("spawn drain");

    // Phase 1: Play N frames (rotation, beat advancing).
    ct.handle_command(ContentCommand::Play);
    let rotation_frames = if paused_mode { 60 } else { total_frames };
    for frame in 0..rotation_frames {
        if frame == 30 || frame == 59 {
            arm_capture();
        }
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
        if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, frame); }
    }

    // Phase 2 (paused mode only): Pause, keep calling tick_frame.
    if paused_mode {
        println!("=== PAUSED phase ===");
        ct.handle_command(ContentCommand::Pause);
        for f in 0..(total_frames - rotation_frames) {
            let host = rotation_frames + f;
            if f == 10 || f == 30 || f == 90 || f == (total_frames - rotation_frames - 1) {
                arm_capture();
            }
            ct.timer.wait_for_deadline();
            ct.tick_frame(&state_tx);
            if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, host); }
        }
    }

    // Final flush.
    if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, total_frames); }
    drop(state_tx); drain.join().expect("drain join");
    println!("=== DONE ===");
    std::process::exit(0);
}
