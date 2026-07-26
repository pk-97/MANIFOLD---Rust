//! RT washout probe: ContentThread harness (Play 60f -> Stop 300f), captures
//! internal RT textures (refl_full, accumulated refl_history, moments) + final
//! composited output at sampled frames. The engine's clip transport drives
//! rotation (no manual params).
//!
//! MANIFOLD_RT_PROBE=1. Output: /tmp/rt_washout/*.png + stderr.
//!   cargo run --features perf-soak --bin manifold -- manifold rt-washout <project>

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use manifold_renderer::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, readback_raw_halves,
};
use manifold_renderer::node_graph::primitives::{
    WashoutCap, WASHOUT_CAPTURE_NOW, WASHOUT_QUEUE,
};
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

fn process_capture(cap: &WashoutCap, device: &manifold_gpu::GpuDevice, out_dir: &std::path::Path) {
    let raw = readback_raw_halves(device, &cap.tex, cap.w, cap.h);
    let pixel_count = (cap.w * cap.h) as usize;
    let mut n_hits = 0usize;
    let mut sum_luma = 0.0f64; let mut sum_luma_sq = 0.0f64;
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base+1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base+2], raw[base+3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base+4], raw[base+5]])).to_f32();
        let a = half::f16::from_bits(u16::from_le_bytes([raw[base+6], raw[base+7]])).to_f32();
        if a > 0.0 && a < 1e6 && !a.is_nan() { n_hits += 1; }
        let luma = 0.2126 * r.max(0.0) + 0.7152 * g.max(0.0) + 0.0722 * b.max(0.0);
        sum_luma += luma as f64; sum_luma_sq += (luma*luma) as f64;
    }
    let hit_frac = n_hits as f64 / pixel_count as f64;
    let mn = if pixel_count > 0 { sum_luma / pixel_count as f64 } else { 0.0 };
    let vr = if pixel_count > 0 { (sum_luma_sq / pixel_count as f64) - mn*mn } else { 0.0 };
    let sd = vr.sqrt();

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
        .unwrap_or_else(|e| eprintln!("[WASHOUT] write {}: {e}", png_path.display()));
    eprintln!(
        "[WASHOUT] {} f={:04} dim={}x{} hit={:.6} luma={:.6} sd={:.6} {}",
        cap.label, cap.frame, cap.w, cap.h, hit_frac, mn, sd, png_path.display(),
    );
}

/// Drain capture queue, stamp frame number, process each.
fn drain_captures(device: &manifold_gpu::GpuDevice, frame: u32) {
    let caps = {
        let mut q = WASHOUT_QUEUE.lock().unwrap();
        for c in &mut *q { c.frame = frame; }
        std::mem::take(&mut *q)
    };
    if caps.is_empty() { return; }
    let dir = PathBuf::from("/tmp/rt_washout");
    let _ = std::fs::create_dir_all(&dir);
    for c in &caps { process_capture(c, device, &dir); }
}

pub fn run(args: &[String]) -> ! {
    unsafe { std::env::set_var("MANIFOLD_RT_PROBE", "1"); }

    let project_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => { eprintln!("usage"); std::process::exit(2); }
    };
    if !project_path.exists() { eprintln!("not found"); std::process::exit(1); }

    println!("=== RT WASHOUT PROBE (ContentThread) ===");
    println!("path: {}", project_path.display());

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
        .name("washout-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .expect("spawn drain");

    // Phase 1: Play 60 frames.
    println!("=== Phase 1: Play 60 frames ===");
    ct.handle_command(ContentCommand::Play);
    for frame in 0..60 {
        if frame == 30 || frame == 59 { WASHOUT_CAPTURE_NOW.store(true, Ordering::Relaxed); }
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
        if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, frame); }
    }

    // Phase 2: Stop 300 frames.
    println!("=== Phase 2: Stop 300 frames ===");
    ct.handle_command(ContentCommand::Stop);
    for f in 0..300 {
        let host = 60 + f;
        if f == 10 || f == 30 || f == 90 || f == 299 { WASHOUT_CAPTURE_NOW.store(true, Ordering::Relaxed); }
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
        if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, host); }
    }

    if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, 999); }

    drop(state_tx); drain.join().expect("drain join");
    println!("=== DONE ===");
    std::process::exit(0);
}
