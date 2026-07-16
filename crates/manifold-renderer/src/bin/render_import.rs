//! `render-import` — headless render of ONE glb/gltf file through the
//! PRODUCTION import path (`assemble_import_graph`), for look-dev and as the
//! conformance harness's oracle binary (D2, `docs/GLB_CONFORMANCE_DESIGN.md`
//! §3). Shaped like `render_generator_preset.rs`: parse → build →
//! converged-readback → PNG, sharing the SAME output transform
//! (`headless_readback::readback_to_srgb_png`) every headless render tool in
//! this crate uses — never a local tonemap (D2).
//!
//! Run:
//!   cargo run -p manifold-renderer --bin render-import -- \
//!       tests/fixtures/gltf/DamagedHelmet.glb --out /tmp/helmet.png
//!
//! `--param id=value` overrides an outer-card param by id (same mechanism
//! `render-generator-preset` uses — the import graph's own
//! `preset_metadata.params`, e.g. `cam_dist`, `sun_int`, `env_intensity`).
//! The SAME flag also accepts a `preset_metadata.string_params` id (e.g.
//! `hdri_file` — GLB_CONFORMANCE_DESIGN.md G-P6): whether `id` names a
//! numeric card param or a string param is resolved AFTER the import graph
//! is assembled (only then do we know which `string_params`/`params` ids
//! exist), so `--param env_mode=1 --param hdri_file=/path/to.exr` works in
//! one consistent flag rather than a second `--string` flag callers have to
//! remember. `--orbit`/`--tilt` are convenience sugar for the two camera params
//! (`cam_orbit`/`cam_tilt`) every import graph carries. `--non-black-floor F`
//! (default 0.02, the DamagedHelmet-gpu-test precedent) lowers the
//! convergence floor for a DELIBERATELY dim render (e.g. a lights-off pass
//! that zeroes `sun_int`/`env_intensity`) — without it, a legitimately dark
//! frame and a frame stuck on a mid-decode black texture are
//! indistinguishable (BUG-100/BUG-117), so the default stays conservative
//! and callers who know their scene is meant to be dark opt out explicitly.
//!
//! Exit codes: 0 = PNG written after convergence; 2 = never converged
//! (prints the last non-black fraction); 3 = import error (parse/build
//! failure — prints the `ImportReport` if one was produced, then the error).

use std::path::PathBuf;

use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, non_black_fraction, readback_raw_halves, readback_tonemapped_rgba8,
};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

struct Args {
    glb: PathBuf,
    width: u32,
    height: u32,
    out: PathBuf,
    // Raw id=value pairs from `--param`, resolved into numeric vs string
    // overrides once the import graph's `preset_metadata` is available (see
    // the module doc comment).
    overrides: Vec<(String, String)>,
    frames_max: u32,
    non_black_floor: f64,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let glb = argv
        .next()
        .ok_or("usage: render-import <file.glb> [--size WxH] [--out PATH] [--param id=value ...] [--orbit R] [--tilt R] [--frames-max N] [--non-black-floor F]")?;
    let mut args = Args {
        glb: PathBuf::from(glb),
        width: 1280,
        height: 720,
        out: PathBuf::from("/tmp/render-import.png"),
        overrides: Vec::new(),
        frames_max: 300,
        non_black_floor: 0.02,
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
            "--out" => args.out = PathBuf::from(value),
            "--frames-max" => {
                args.frames_max = value.parse().map_err(|e| format!("bad frames-max: {e}"))?;
            }
            "--non-black-floor" => {
                args.non_black_floor =
                    value.parse().map_err(|e| format!("bad non-black-floor: {e}"))?;
            }
            "--param" => {
                let (id, v) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--param wants id=value, got {value}"))?;
                args.overrides.push((id.to_string(), v.to_string()));
            }
            "--orbit" => {
                args.overrides.push(("cam_orbit".to_string(), value));
            }
            "--tilt" => {
                args.overrides.push(("cam_tilt".to_string(), value));
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

    let (def, report) = match assemble_import_graph(&args.glb) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("render-import: import error for {}: {e}", args.glb.display());
            std::process::exit(3);
        }
    };
    println!("render-import: {} -> {report:?}", args.glb.display());

    // Same outer-card override mechanism `render-generator-preset` uses: the
    // import graph carries its own `preset_metadata.params` (cam_orbit,
    // cam_tilt, cam_dist, sun_int, env_intensity, ...) AND
    // `preset_metadata.string_params` (model_file, hdri_file). `--param`
    // resolves against both — a numeric card param id sets `params`; a
    // string param id sets `string_overrides` instead (see module doc).
    let mut params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    let string_param_ids: std::collections::BTreeSet<String> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.string_params.iter().map(|s| s.id.clone()).collect())
        .unwrap_or_default();
    let mut string_overrides: std::collections::BTreeMap<String, String> = Default::default();
    for (id, v) in &args.overrides {
        if string_param_ids.contains(id) {
            string_overrides.insert(id.clone(), v.clone());
            continue;
        }
        match params.iter_mut().find(|p| p.id() == id) {
            Some(p) => {
                p.value = v
                    .parse()
                    .unwrap_or_else(|e| panic!("bad value for numeric param '{id}': {e}"))
            }
            None => {
                eprintln!("error: import graph has no outer param or string param '{id}'");
                std::process::exit(2);
            }
        }
    }
    let manifest = ParamManifest::from_params(params);

    let device = std::sync::Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
    let mut runtime = match PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&device),
        args.width,
        args.height,
        format,
        Some(&manifest),
    ) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "render-import: build failed for {} (import report: {report:?}): {e:?}",
                args.glb.display()
            );
            std::process::exit(3);
        }
    };

    if !string_overrides.is_empty() {
        runtime.apply_string_params(Some(&string_overrides));
    }

    let target = RenderTarget::new(&device, args.width, args.height, format, "render-import-target");

    // Same convergence-poll pattern as
    // `damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate`
    // (BUG-100/BUG-117): background texture decodes (base-color/normal/mr/
    // occlusion/emissive, each its own `node.gltf_texture_source` thread)
    // emit solid black every frame until their decode lands, so a frame
    // where every wired source is STILL mid-decode is byte-stable too — a
    // fixed frame count alone can't tell "converged" from "stuck at black".
    // Require byte-stability AND a non-black floor together.
    //
    // The `std::thread::sleep` below is NOT cosmetic — omitting it (an
    // earlier version of this loop did) is a real bug, found empirically
    // this session: with zero pacing, the GPU render loop can spin through
    // `STABLE_STREAK` frames in under a millisecond, faster than a
    // multi-texture background decode can swap even one map in, so 3
    // "stable" frames can land entirely inside a decode thread's dead time
    // — a genuine partial-load state (e.g. the normal map still solid-
    // default while base-color has already landed) reads as fully
    // converged. Reproduced on `DamagedHelmet.glb`: without the sleep,
    // 2 of 3 runs converged on a visibly wrong frame (a monochrome
    // "zebra-striped" partial load) at a DIFFERENT fraction than the
    // correct render. The DamagedHelmet gpu test this pattern is ported
    // from paces its polls at 50ms for exactly this reason; this loop
    // renders every frame (unlike that test's real-time poll), so the
    // sleep goes between frames instead of around the whole attempt.
    const DT: f32 = 1.0 / 60.0;
    const STABLE_STREAK: u32 = 3;
    let mut prev_raw: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut last_fraction = 0.0f64;
    let mut final_rgba = Vec::new();
    for frame in 0..args.frames_max {
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
            anim_progress: (frame as f32 / 60.0).min(1.0),
            trigger_count: 0,
        };
        let mut enc = device.create_encoder("render-import-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        enc.commit_and_wait_completed();

        // G-P6 gate-review fix: byte-stability alone can't see a decode
        // that hasn't LANDED yet — a 74 MB 4k EXR decodes for seconds while
        // `node.hdri_source` emits stable black, and the helmet (sun-lit,
        // emissive) clears the non-black floor without any environment at
        // all, so the loop declared convergence on frame 5 with the sky
        // still missing. `PresetRuntime::io_pending` surfaces the IoBridge
        // sources' in-flight decodes (`EffectNode::io_pending`); while any
        // decode is pending, stable frames don't count.
        let io_pending = runtime.io_pending();
        let raw = readback_raw_halves(&device, &target.texture, args.width, args.height);
        let byte_stable = prev_raw.as_deref() == Some(raw.as_slice());
        prev_raw = Some(raw);
        if byte_stable && !io_pending {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        if stable_count >= STABLE_STREAK {
            let rgba = readback_tonemapped_rgba8(&device, &target.texture, args.width, args.height);
            last_fraction = non_black_fraction(&rgba);
            if last_fraction > args.non_black_floor {
                converged = true;
                final_rgba = rgba;
                println!(
                    "render-import: converged on frame {frame} (non-black fraction {last_fraction:.4})"
                );
                break;
            }
        }
    }

    if !converged {
        eprintln!(
            "render-import: WARNING — never converged after {} frames (last non-black fraction \
             {last_fraction:.4}); a background texture decode may be stuck",
            args.frames_max
        );
        std::process::exit(2);
    }

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    let png = encode_rgba8_png(&final_rgba, args.width, args.height);
    std::fs::write(&args.out, &png).unwrap_or_else(|e| panic!("write {}: {e}", args.out.display()));
    println!("OK {} ({}x{})", args.out.display(), args.width, args.height);
}
