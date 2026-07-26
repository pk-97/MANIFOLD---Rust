//! RT washout probe: capture internal RT textures (refl_full, irr_full,
//! moments) via render_scene capture statics at sampled frames, plus
//! composited output. Reports hit-fraction time series across 360 frames.
//!
//! MANIFOLD_RT_PROBE=1. Output: /tmp/rt_washout/*.png + stderr.
//!   cargo run --features perf-soak --bin manifold -- manifold rt-washout <project>

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::generators::registry::GeneratorRegistry;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, readback_raw_halves,
};
use manifold_renderer::node_graph::primitives::{WashoutCap, WASHOUT_CAPTURE_NOW, WASHOUT_QUEUE};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::render_target::RenderTarget;

const W: u32 = 1920;
const H: u32 = 1080;

fn read_tex(cap: &WashoutCap, device: &GpuDevice, frame: u32) {
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
        sum_luma += luma as f64; sum_luma_sq += (luma * luma) as f64;
    }
    let hit_frac = n_hits as f64 / pixel_count as f64;
    let ml = if pixel_count > 0 { sum_luma / pixel_count as f64 } else { 0.0 };
    let vl = if pixel_count > 0 { (sum_luma_sq / pixel_count as f64) - ml*ml } else { 0.0 };

    let dir = PathBuf::from("/tmp/rt_washout");
    let _ = std::fs::create_dir_all(&dir);
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
    let png = dir.join(format!("{}_{:04}.png", cap.label, frame));
    std::fs::write(&png, encode_rgba8_png(&rgba8, W, H))
        .unwrap_or_else(|e| eprintln!("[WASHOUT] write {}: {e}", png.display()));
    eprintln!("[WASHOUT] {} f={frame:04} hit={hit_frac:.6} luma={ml:.6} var={vl:.6} {}", cap.label, png.display());
}

pub fn run(args: &[String]) -> ! {
    unsafe { std::env::set_var("MANIFOLD_RT_PROBE", "1"); }

    let project_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => { eprintln!("usage"); std::process::exit(2); }
    };
    let project = manifold_io::loader::load_project_with(&project_path, crate::project_io::install_embedded_presets)
        .unwrap_or_else(|e| { eprintln!("FAILED: {e}"); std::process::exit(1); });

    let layer = project.timeline.layers.iter().find(|l| {
        l.gen_params().is_some_and(|gp| gp.params.get("8_rt_enabled").is_some())
    }).unwrap_or_else(|| { eprintln!("No RT layer"); std::process::exit(1); });
    let gp = layer.gen_params().unwrap();
    println!("=== RT WASHOUT PROBE (texture captures) ===");
    println!("type={}", gp.generator_type().as_str());

    let device = Arc::new(GpuDevice::new());
    let format = GpuTextureFormat::Rgba16Float;
    let registry = GeneratorRegistry::new(format);
    let mut runtime = registry.create_with_override(
        Arc::clone(&device), gp.generator_type(), gp.graph_def().as_ref(),
        W, H, false, Some(&gp.params), None,
    ).unwrap_or_else(|| { eprintln!("build failed"); std::process::exit(1); });

    let target = RenderTarget::new(&device, W, H, format, "washout-target");
    let pm = ParamManifest::from_params(gp.params.iter().cloned().collect());

    for frame in 0..360 {
        let capture = frame == 30 || frame == 59 || frame == 70 || frame == 90
            || frame == 150 || frame == 359;
        if capture { WASHOUT_CAPTURE_NOW.store(true, Ordering::Relaxed); }

        let ctx = PresetContext {
            time: frame as f64 / 60.0, beat: 0.0, dt: 1.0 / 60.0,
            width: W, height: H, output_width: W, output_height: H,
            aspect: W as f32 / H as f32,
            owner_key: 0, is_clip_level: false,
            frame_count: frame as i64, anim_progress: 1.0, trigger_count: 0,
        };
        let mut enc = device.create_encoder("f");
        { let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
          runtime.render(&mut gpu, &target.texture, &ctx, &pm); }
        enc.commit_and_wait_completed();

        if capture {
            let caps = { let mut q = WASHOUT_QUEUE.lock().unwrap(); std::mem::take(&mut *q) };
            for c in &caps { read_tex(c, &device, frame); }
        }
    }
    println!("=== DONE ===");
    std::process::exit(0);
}
