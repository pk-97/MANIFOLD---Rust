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

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{readback_raw_halves, readback_to_srgb_png};
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
    /// BUG-117: hard cap on frames rendered while WAITING for convergence
    /// past `--frames`, so a preset that never settles (a genuinely
    /// per-frame-varying look, or a stuck async load) can't hang the tool
    /// forever — it prints a warning and writes whatever it has instead.
    max_frames: u32,
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
        max_frames: 300,
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
            "--max-frames" => {
                args.max_frames = value.parse().map_err(|e| format!("bad max-frames: {e}"))?;
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

    let device = std::sync::Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&device),
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

    // BUG-117: async-loading primitives (large glTF, image_folder, DNN
    // plugins) leave the pre-bound output untouched until their background
    // job lands, so a fixed `--frames` count can write a PNG mid-load with
    // no warning — the same class BUG-100 hit for the azalea-import test
    // harness. Same fix, ported here: after the requested warm-up, keep
    // rendering and comparing consecutive RAW readbacks until `STABLE_STREAK`
    // of them are byte-identical, capped at `--max-frames` so a genuinely
    // per-frame-varying preset (or a stuck load) can't hang the tool forever.
    const DT: f32 = 1.0 / 60.0;
    const STABLE_STREAK: u32 = 3;
    let warmup_frames = args.frames.max(1);
    let max_frames = args.max_frames.max(warmup_frames);
    let mut prev_raw: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    for frame in 0..max_frames {
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
            anim_progress: (frame as f32 / warmup_frames as f32).min(1.0),
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

        // Convergence tracking only kicks in once the requested warm-up has
        // run — the caller asked for at least that many frames regardless
        // (e.g. to reach a specific animation beat), and a preset can
        // legitimately still be settling its warm-up transient this early.
        if frame + 1 >= warmup_frames {
            let raw = readback_raw_halves(&device, &target.texture, args.width, args.height);
            if prev_raw.as_deref() == Some(raw.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_raw = Some(raw);
            if stable_count >= STABLE_STREAK {
                converged = true;
                println!(
                    "render-generator-preset: converged on frame {frame} \
                     (stable for {STABLE_STREAK} frames)"
                );
                break;
            }
        }
    }
    if !converged {
        eprintln!(
            "render-generator-preset: WARNING — hit --max-frames={max_frames} before {STABLE_STREAK} \
             consecutive identical frames; the preset may still be loading async content \
             (glTF, image_folder, a DNN plugin) and this PNG could be an incomplete render. \
             Re-run with a higher --max-frames if this preset is expected to keep animating."
        );
    }

    // D2: the ONE shared tonemap/encode (`headless_readback::readback_to_srgb_png`)
    // — never a local Reinhard implementation here.
    let png = readback_to_srgb_png(&device, &target.texture, args.width, args.height);
    std::fs::write(&args.out, &png).unwrap_or_else(|e| panic!("write {}: {e}", args.out.display()));
    println!("OK {} ({}x{}, {} frames)", args.out.display(), args.width, args.height, args.frames);
}
