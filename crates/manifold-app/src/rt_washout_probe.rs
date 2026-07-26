//! RT washout probe: drive generator directly via PresetRuntime, capture the
//! composited render target at sampled frames, and report hit-fraction time
//! series to diagnose where traced reflection content dies after stillness.
//!
//! MANIFOLD_RT_PROBE=1 to enable. Output: /tmp/rt_washout/*.png + stderr.
//!
//!   cargo run --features perf-soak --bin manifold -- manifold rt-washout <project>

use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::generators::registry::GeneratorRegistry;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, readback_raw_halves,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::render_target::RenderTarget;

const W: u32 = 1920;
const H: u32 = 1080;

fn capture_and_report(device: &GpuDevice, target: &manifold_gpu::GpuTexture, frame: u32) {
    let raw = readback_raw_halves(device, target, W, H);
    let pixel_count = (W * H) as usize;
    let mut n_hits = 0usize;
    let mut sum_luma = 0.0f64; let mut sum_luma_sq = 0.0f64;
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base+1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base+2], raw[base+3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base+4], raw[base+5]])).to_f32();
        let _a = half::f16::from_bits(u16::from_le_bytes([raw[base+6], raw[base+7]])).to_f32();
        if r > 0.03 || g > 0.03 || b > 0.03 { n_hits += 1; }
        let luma = 0.2126 * r.max(0.0) + 0.7152 * g.max(0.0) + 0.0722 * b.max(0.0);
        sum_luma += luma as f64; sum_luma_sq += (luma * luma) as f64;
    }
    let hit_frac = n_hits as f64 / pixel_count as f64;
    let mean_luma = sum_luma / pixel_count as f64;
    let var_luma = (sum_luma_sq / pixel_count as f64) - (mean_luma * mean_luma);

    // Tonemap and write PNG.
    let mut rgba8 = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let base = i * 8;
        let r = half::f16::from_bits(u16::from_le_bytes([raw[base], raw[base+1]])).to_f32();
        let g = half::f16::from_bits(u16::from_le_bytes([raw[base+2], raw[base+3]])).to_f32();
        let b = half::f16::from_bits(u16::from_le_bytes([raw[base+4], raw[base+5]])).to_f32();
        rgba8.push(linear_to_srgb8(r.max(0.0)));
        rgba8.push(linear_to_srgb8(g.max(0.0)));
        rgba8.push(linear_to_srgb8(b.max(0.0)));
        rgba8.push(255u8);
    }
    let dir = PathBuf::from("/tmp/rt_washout");
    let _ = std::fs::create_dir_all(&dir);
    let png_path = dir.join(format!("composite_f{:04}.png", frame));
    std::fs::write(&png_path, encode_rgba8_png(&rgba8, W, H))
        .unwrap_or_else(|e| eprintln!("[WASHOUT] write {}: {e}", png_path.display()));
    eprintln!(
        "[WASHOUT] composite f={frame:04} hit={hit_frac:.6} luma={mean_luma:.6} var={var_luma:.6} {}",
        png_path.display(),
    );
}

pub fn run(args: &[String]) -> ! {
    unsafe { std::env::set_var("MANIFOLD_RT_PROBE", "1"); }

    let project_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => { eprintln!("usage: manifold rt-washout <project>"); std::process::exit(2); }
    };
    let project = manifold_io::loader::load_project_with(&project_path, crate::project_io::install_embedded_presets)
        .unwrap_or_else(|e| { eprintln!("FAILED: {e}"); std::process::exit(1); });

    let layer = project.timeline.layers.iter().find(|l| {
        l.gen_params().is_some_and(|gp| gp.params.get("8_rt_enabled").is_some())
    }).unwrap_or_else(|| { eprintln!("No layer with RT params"); std::process::exit(1); });
    let gp = layer.gen_params().unwrap();
    let manifest = &gp.params;
    println!("=== RT WASHOUT PROBE ===");
    println!("type={} layers=3", gp.generator_type().as_str());

    let device = Arc::new(GpuDevice::new());
    let format = GpuTextureFormat::Rgba16Float;
    let registry = GeneratorRegistry::new(format);
    let mut runtime = registry.create_with_override(
        Arc::clone(&device), gp.generator_type(), gp.graph_def().as_ref(),
        W, H, false, Some(manifest), None,
    ).unwrap_or_else(|| { eprintln!("build failed"); std::process::exit(1); });

    let target = RenderTarget::new(&device, W, H, format, "washout-target");
    let pm = ParamManifest::from_params(manifest.iter().cloned().collect());

    println!("=== Rendering 360 frames ===");
    for frame in 0..360 {
        let ctx = PresetContext {
            time: frame as f64 / 60.0, beat: 0.0, dt: 1.0 / 60.0,
            width: W, height: H, output_width: W, output_height: H,
            aspect: W as f32 / H as f32,
            owner_key: 0, is_clip_level: false,
            frame_count: frame as i64, anim_progress: 1.0, trigger_count: 0,
        };
        let mut enc = device.create_encoder("washout-frame");
        { let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
          runtime.render(&mut gpu, &target.texture, &ctx, &pm); }
        enc.commit_and_wait_completed();

        if frame == 30 || frame == 59 || frame == 70 || frame == 90 || frame == 150 || frame == 359 {
            capture_and_report(&device, &target.texture, frame);
        }
    }
    println!("=== DONE ===");
    std::process::exit(0);
}
