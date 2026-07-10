//! `render-generator-preset` — headless look-dev render of ONE generator
//! preset at an arbitrary resolution, warm-up length, and outer-card param
//! overrides. The factory-thumbnail bin (`generate_preset_thumbnails.rs`)
//! renders every preset square at 256px with defaults — right for the
//! browser, useless for judging a look. This bin is the iteration loop for
//! shader work inside a preset: edit JSON → render → Read the PNG.
//!
//! Run:
//!   cargo run -p manifold-renderer --bin render-generator-preset -- \
//!       BlackHole --size 1280x720 --frames 90 --out /tmp/bh.png \
//!       --param cam_dist=31.75 --param tilt=15
//!
//! Params are outer-card ids from the preset's `presetMetadata.params`;
//! anything not overridden renders at its declared default. Output is the
//! same Reinhard-tonemapped, straight-alpha-over-black PNG convention the
//! save-time thumbnail path uses (linear HDR graph output → viewable PNG).

use std::path::PathBuf;

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTexture};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

struct Args {
    preset: String,
    width: u32,
    height: u32,
    frames: u32,
    out: PathBuf,
    overrides: Vec<(String, f32)>,
    /// Fire a clip trigger every N frames (0 = never) — trigger_count
    /// advances as frame/N so trigger-responsive presets can be exercised
    /// headlessly.
    trigger_every: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let preset = argv.next().ok_or("usage: render-generator-preset <PresetId> [--size WxH] [--frames N] [--out PATH] [--param id=value ...]")?;
    let mut args = Args {
        preset,
        width: 1280,
        height: 720,
        frames: 90,
        out: PathBuf::from("/tmp/preset-render.png"),
        overrides: Vec::new(),
        trigger_every: 0,
    };
    while let Some(flag) = argv.next() {
        let value = argv
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--size" => {
                let (w, h) = value
                    .split_once('x')
                    .ok_or_else(|| format!("--size wants WxH, got {value}"))?;
                args.width = w.parse().map_err(|e| format!("bad width: {e}"))?;
                args.height = h.parse().map_err(|e| format!("bad height: {e}"))?;
            }
            "--frames" => {
                args.frames = value.parse().map_err(|e| format!("bad frames: {e}"))?;
            }
            "--out" => args.out = PathBuf::from(value),
            "--triggers" => {
                args.trigger_every = value.parse().map_err(|e| format!("bad triggers: {e}"))?;
            }
            "--param" => {
                let (id, v) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--param wants id=value, got {value}"))?;
                let v: f32 = v.parse().map_err(|e| format!("bad value for {id}: {e}"))?;
                args.overrides.push((id.to_string(), v));
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/generator-presets")
        .join(format!("{}.json", args.preset));
    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", json_path.display()));
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse preset JSON");

    // Manifest seeded from the preset's own outer-card specs so overrides
    // route through the same bindings the inspector card uses.
    let mut params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    for (id, v) in &args.overrides {
        match params.iter_mut().find(|p| p.id() == id) {
            Some(p) => p.value = *v,
            None => {
                eprintln!("error: preset has no outer param '{id}'");
                std::process::exit(2);
            }
        }
    }
    let manifest = ParamManifest::from_params(params);

    let device = GpuDevice::new();
    let registry = PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        &device,
        args.width,
        args.height,
        format,
        None,
    )
    .expect("generator build failed");

    let target = RenderTarget::new(
        &device,
        args.width,
        args.height,
        format,
        "look-dev-target",
    );

    const DT: f32 = 1.0 / 60.0;
    for frame in 0..args.frames.max(1) {
        let time = frame as f64 * DT as f64;
        let ctx = PresetContext {
            time,
            beat: time * 2.0, // 120 bpm
            dt: DT,
            width: args.width,
            height: args.height,
            output_width: args.width,
            output_height: args.height,
            aspect: args.width as f32 / args.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: (frame as f32 / args.frames.max(1) as f32).min(1.0),
            trigger_count: if args.trigger_every > 0 {
                frame / args.trigger_every
            } else {
                0
            },
        };
        let mut enc = device.create_encoder("look-dev-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        enc.commit_and_wait_completed();
    }

    let rgba = readback_tonemapped_rgba8(&device, &target.texture, args.width, args.height);
    image::save_buffer(
        &args.out,
        &rgba,
        args.width,
        args.height,
        image::ExtendedColorType::Rgba8,
    )
    .expect("write PNG");
    println!("OK {} ({}x{}, {} frames)", args.out.display(), args.width, args.height, args.frames);
}

/// Same convention as `preset_thumbnail::readback_tonemapped_rgba8` (private
/// there): Reinhard tonemap + straight-alpha composite over opaque black.
fn readback_tonemapped_rgba8(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
    let bytes_per_row = w * 8;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("look-dev-readback");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

    let tonemap = |v: f32| -> u8 {
        let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
        (ldr * 255.0).round() as u8
    };
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in halves.chunks_exact(4) {
        let r = f16::from_bits(px[0]).to_f32();
        let g = f16::from_bits(px[1]).to_f32();
        let b = f16::from_bits(px[2]).to_f32();
        let a = f16::from_bits(px[3]).to_f32().clamp(0.0, 1.0);
        out.push(tonemap(r * a));
        out.push(tonemap(g * a));
        out.push(tonemap(b * a));
        out.push(255);
    }
    out
}
