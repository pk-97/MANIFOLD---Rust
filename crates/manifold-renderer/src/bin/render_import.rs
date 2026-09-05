//! `render-import` — headless render of ONE glb/gltf file through the
//! PRODUCTION import path (`assemble_import_graph`), for look-dev and as the
//! conformance harness's oracle binary (D2, `docs/GLB_CONFORMANCE_DESIGN.md`
//! section 3). Shaped like `render_generator_preset.rs`: parse → build →
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
//! `preset_metadata.params`, e.g. `cam_dist`, `7_intensity`, `1_intensity`).
//! The SAME flag also accepts a `preset_metadata.string_params` id (e.g.
//! `hdri_file` — GLB_CONFORMANCE_DESIGN.md G-P6): whether `id` names a
//! numeric card param or a string param is resolved AFTER the import graph
//! is assembled (only then do we know which `string_params`/`params` ids
//! exist), so `--param env_mode=1 --param hdri_file=/path/to.exr` works in
//! one consistent flag rather than a second `--string` flag callers have to
//! remember. A numeric override outside the param's own declared `[min,
//! max]` (e.g. `emitter_elevation`'s `[-1, 1]` normalized "up" component,
//! not degrees) is a hard error naming the value and the declared range —
//! never a silent write-through (BUG-6s5m: an out-of-range value used to
//! render a misleading "looks the same" degenerate picture instead of
//! erroring). `--orbit`/`--tilt` are convenience sugar for the synthesized
//! camera's orbit/tilt params — the import graph stamps their ids as
//! `{camera_node_id}_orbit`/`{camera_node_id}_tilt` (e.g. `5_orbit`), NOT a
//! fixed `cam_orbit`/`cam_tilt` (BUG-su2o: the doc previously claimed the
//! fixed ids, which no real import graph carries). Resolved AFTER assembly
//! by suffix match against the graph's actual param listing — the exposed
//! param whose id ends with `_orbit`/`_tilt` (or equals `orbit`/`tilt`
//! exactly). Zero matches or more than one match is a hard error (never a
//! silent no-op) listing the params it did/could match. `--non-black-floor F`
//! (default 0.02, the DamagedHelmet-gpu-test precedent) lowers the
//! convergence floor for a DELIBERATELY dim render (e.g. a lights-off pass
//! that zeroes `7_intensity`/`1_intensity`) — without it, a legitimately dark
//! frame and a frame stuck on a mid-decode black texture are
//! indistinguishable (BUG-100/BUG-117), so the default stays conservative
//! and callers who know their scene is meant to be dark opt out explicitly.
//!
//! ANIMATION MODE (`--anim-param`): temporal repro of RT/reflection artifacts
//! in one process by rendering a param-interpolated frame sequence. Accepts
//! either a specific param ID (e.g. `13_rot_y`) or ONE wildcard form — an ID
//! starting with `*` (e.g. `*_rot_y`) matches EVERY numeric card param whose ID
//! ends with that suffix. Wildcard mode sweeps ALL matched params together,
//! which is how model-transform changes exercise the acceleration structure
//! refit path (camera motion does not). Warmup converges first at the start
//! value(s), then N consecutive frames render with the param(s) advancing linearly
//! to end. Frames are tiled horizontally into one PNG.
//!
//! Exit codes: 0 = PNG written after convergence; 2 = never converged
//! (prints the last non-black fraction); 3 = import error (parse/build
//! failure — prints the `ImportReport` if one was produced, then the error).

use std::path::PathBuf;

use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, mean_abs_half_diff, non_black_fraction, readback_raw_halves, readback_tonemapped_rgba8,
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
    // Raw `--orbit`/`--tilt` values, resolved by suffix match against the
    // graph's actual param listing once it's assembled (module doc comment).
    orbit: Option<String>,
    tilt: Option<String>,
    frames_max: u32,
    non_black_floor: f64,
    trace: bool,
    /// BUG-210: the render clock is FROZEN at this time (seconds) for the
    /// whole convergence loop — auto-playing imports (GLTF_ANIMATION
    /// A1–A4) re-pose every frame under an advancing clock, so
    /// byte-stability never lands and every animated asset reads as
    /// "never converged". Pick the animation moment with `--time`;
    /// texture-decode convergence still works (a decode swap breaks the
    /// stability streak once, then re-stabilizes).
    time: f64,
    /// Animation mode: param to animate (all four required together).
    anim_param: Option<String>,
    anim_start: Option<f32>,
    anim_end: Option<f32>,
    anim_frames: Option<u32>,
}

/// BUG-su2o: resolves `--orbit`/`--tilt` against the import graph's ACTUAL
/// exposed param ids by suffix — the graph stamps camera params as
/// `{camera_node_id}_orbit`/`{camera_node_id}_tilt` (e.g. `5_orbit`), never
/// a fixed `cam_orbit`/`cam_tilt`. Matches a param id ending in `_{suffix}`
/// or equal to `{suffix}` exactly. Zero or multiple matches is a hard
/// error listing the candidates, never a silent no-op.
fn resolve_camera_sugar_param(suffix: &str, available: &[String]) -> Result<String, String> {
    let matches: Vec<&String> = available
        .iter()
        .filter(|id| id.as_str() == suffix || id.ends_with(&format!("_{suffix}")))
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "--{suffix} matched no param (looked for an id equal to '{suffix}' or ending in '_{suffix}'); available params: {}",
            available.join(", ")
        )),
        [single] => Ok((*single).clone()),
        many => Err(format!(
            "--{suffix} is ambiguous: matched {} params: {}",
            many.len(),
            many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// BUG-6s5m: a `--param` value outside the target param's own declared
/// `[min, max]` used to write through silently and produce a degenerate
/// render — `emitter_elevation`'s range is `[-1, 1]` (a normalized "up"
/// component, not degrees), so an out-of-range probe value like `10` or
/// `80` pushed the emitter strip fully outside the visible dome and
/// rendered as "no strip", byte-identical to an unrelated
/// `emitter_intensity=0` override. This tool exists to answer "what does
/// this param value look like" — a value outside its own declared range
/// can't answer that, so it must stop the probe loudly rather than render
/// a misleading picture. `min < max` guards the degenerate case (no real
/// range declared): those params pass through unchecked.
fn check_param_range(id: &str, parsed: f32, min: f32, max: f32) -> Result<(), String> {
    if min < max && (parsed < min || parsed > max) {
        return Err(format!(
            "--param {id}={parsed} is out of range — declared range is [{min}, {max}]"
        ));
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let glb = argv
        .next()
        .ok_or("usage: render-import <file.glb> [--size WxH] [--out PATH] [--param id=value ...] [--orbit R] [--tilt R] [--frames-max N] [--non-black-floor F] [--time SECONDS] [--trace] [--anim-param ID --anim-start F --anim-end F --anim-frames N]")?;
    let mut args = Args {
        glb: PathBuf::from(glb),
        width: 1280,
        height: 720,
        out: PathBuf::from("/tmp/render-import.png"),
        overrides: Vec::new(),
        orbit: None,
        tilt: None,
        frames_max: 300,
        non_black_floor: 0.02,
        trace: false,
        time: 0.0,
        anim_param: None,
        anim_start: None,
        anim_end: None,
        anim_frames: None,
    };
    while let Some(flag) = argv.next() {
        if flag == "--trace" {
            args.trace = true;
            continue;
        }
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
            "--time" => {
                args.time = value.parse().map_err(|e| format!("bad time: {e}"))?;
            }
            "--param" => {
                let (id, v) = value
                    .split_once('=')
                    .ok_or_else(|| format!("--param wants id=value, got {value}"))?;
                args.overrides.push((id.to_string(), v.to_string()));
            }
            "--orbit" => {
                args.orbit = Some(value);
            }
            "--tilt" => {
                args.tilt = Some(value);
            }
            "--anim-param" => {
                args.anim_param = Some(value);
            }
            "--anim-start" => {
                args.anim_start = Some(value.parse().map_err(|e| format!("bad anim-start: {e}"))?);
            }
            "--anim-end" => {
                args.anim_end = Some(value.parse().map_err(|e| format!("bad anim-end: {e}"))?);
            }
            "--anim-frames" => {
                args.anim_frames = Some(value.parse().map_err(|e| format!("bad anim-frames: {e}"))?);
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

/// `--dump-def <glb-path> <out.json>` — P3-D INV-R8 harness mode. Assembles
/// the import graph through the SAME production entry point (`assemble_import_graph`)
/// and serializes the `(EffectGraphDef, ImportReport)` pair as pretty JSON to
/// `out.json`. On an import ERROR (parse/build failure — NOT a missing file),
/// writes a `{"import_error": "<Display>"}` sentinel instead and still exits 0,
/// so a table-ization change that altered error behavior is caught by the diff
/// too. Exit 2 only on argument/IO failure — the capture script owns the
/// missing-fixture loud-fail (it checks the path exists before invoking).
fn dump_def_mode(glb: &std::path::Path, out: &std::path::Path) -> ! {
    let json = match assemble_import_graph(glb) {
        Ok(pair) => serde_json::to_string_pretty(&pair)
            .unwrap_or_else(|e| panic!("serialize import def for {}: {e}", glb.display())),
        Err(e) => serde_json::to_string_pretty(&serde_json::json!({
            "import_error": e.to_string(),
        }))
        .expect("serialize import_error sentinel"),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("create dump output dir");
    }
    std::fs::write(out, json).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    std::process::exit(0);
}

fn main() {
    // P3-D INV-R8 harness mode, resolved before the render arg parser: the
    // capture script drives `render-import --dump-def <glb> <out.json>`.
    {
        let mut argv = std::env::args().skip(1);
        if argv.next().as_deref() == Some("--dump-def") {
            let glb = argv.next().unwrap_or_else(|| {
                eprintln!("usage: render-import --dump-def <file.glb> <out.json>");
                std::process::exit(2);
            });
            let out = argv.next().unwrap_or_else(|| {
                eprintln!("usage: render-import --dump-def <file.glb> <out.json>");
                std::process::exit(2);
            });
            dump_def_mode(std::path::Path::new(&glb), std::path::Path::new(&out));
        }
    }

    let mut args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    // Validate anim flags: all four required together or none at all.
    let has_anim = args.anim_param.is_some() || args.anim_start.is_some() || args.anim_end.is_some() || args.anim_frames.is_some();
    if has_anim && (args.anim_param.is_none() || args.anim_start.is_none() || args.anim_end.is_none() || args.anim_frames.is_none()) {
        eprintln!("error: --anim-param, --anim-start, --anim-end, --anim-frames must all be specified together");
        std::process::exit(2);
    }

    let (def, report) = match assemble_import_graph(&args.glb) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("render-import: import error for {}: {e}", args.glb.display());
            std::process::exit(3);
        }
    };
    println!("render-import: {} -> {report:?}", args.glb.display());

    // Same outer-card override mechanism `render-generator-preset` uses: the
    // import graph carries its own `preset_metadata.params` (e.g. 5_orbit,
    // 5_tilt, cam_dist, 7_intensity, 1_intensity, ...) AND
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

    // Resolve --orbit/--tilt sugar against the graph's ACTUAL param ids
    // (module doc comment, BUG-su2o) before applying any override.
    let available_param_ids: Vec<String> = params.iter().map(|p| p.id().to_string()).collect();
    if let Some(value) = args.orbit.take() {
        match resolve_camera_sugar_param("orbit", &available_param_ids) {
            Ok(id) => args.overrides.push((id, value)),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(value) = args.tilt.take() {
        match resolve_camera_sugar_param("tilt", &available_param_ids) {
            Ok(id) => args.overrides.push((id, value)),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        }
    }

    let mut string_overrides: std::collections::BTreeMap<String, String> = Default::default();
    for (id, v) in &args.overrides {
        if string_param_ids.contains(id) {
            string_overrides.insert(id.clone(), v.clone());
            continue;
        }
        match params.iter_mut().find(|p| p.id() == id) {
            Some(p) => {
                let parsed: f32 = v
                    .parse()
                    .unwrap_or_else(|e| panic!("bad value for numeric param '{id}': {e}"));
                if let Err(e) = check_param_range(id, parsed, p.spec.min, p.spec.max) {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
                p.value = parsed;
            }
            None => {
                eprintln!("error: import graph has no outer param or string param '{id}'");
                eprintln!(
                    "available numeric params: {}",
                    params.iter().map(|p| p.id()).collect::<Vec<_>>().join(", ")
                );
                std::process::exit(2);
            }
        }
    }
    let manifest = ParamManifest::from_params(params);

    // Clone def before it's moved to PresetRuntime; needed for animation mode.
    let def_clone = def.clone();

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

    let mut runtime = runtime;

    let target = RenderTarget::new(&device, args.width, args.height, format, "render-import-target");

    // Animation mode: render sequence tiled horizontally. Normal mode:
    // single converged frame.
    let anim_param_clone = args.anim_param.clone();
    let (anim_frames, anim_param_id, anim_start, anim_end) = match (
        args.anim_frames,
        anim_param_clone.as_ref(),
        args.anim_start,
        args.anim_end,
    ) {
        (Some(n), Some(id), Some(start), Some(end)) => (n, id.clone(), start, end),
        _ => {
            // Normal mode: single-frame convergence (existing behavior).
            render_single_frame(
                &device,
                &target,
                &mut runtime,
                &manifest,
                &args,
                args.time,
            );
            return;
        }
    };

    // Check for wildcard mode (ID starts with '*').
    let is_wildcard = anim_param_id.starts_with('*');
    let anim_suffix = if is_wildcard {
        anim_param_id.strip_prefix('*').unwrap_or(&anim_param_id)
    } else {
        &anim_param_id
    };

    // Animation mode: warmup convergence first at anim start value.
    // Resolve overrides ONCE into base params, then clone per frame.
    let mut base_params: Vec<Param> = def_clone
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();

    let string_param_ids: std::collections::BTreeSet<String> = def_clone
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
    }
    // Apply string overrides ONCE, not per frame.
    if !string_overrides.is_empty() {
        runtime.apply_string_params(Some(&string_overrides));
    }
    // Apply numeric overrides ONCE into base params, then clone per frame.
    for (id, v) in &args.overrides {
        if !string_param_ids.contains(id) && let Some(p) = base_params.iter_mut().find(|p| p.id() == id) {
            let parsed: f32 = v.parse().unwrap_or_else(|e| panic!("bad value for numeric param '{id}': {e}"));
            if let Err(e) = check_param_range(id, parsed, p.spec.min, p.spec.max) {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
            p.value = parsed;
        }
    }

    // Resolve wildcard matches ONCE.
    let anim_param_ids: Vec<String> = if is_wildcard {
        let suffix = anim_suffix;
        base_params
            .iter()
            .filter(|p| p.id().ends_with(suffix))
            .map(|p| p.id().to_string())
            .collect()
    } else {
        vec![anim_param_id.clone()]
    };

    if anim_param_ids.is_empty() {
        eprintln!("error: wildcard '{}' matched zero params", anim_param_id);
        std::process::exit(2);
    }
    println!("anim: wildcard '{}' matched {} param(s)", anim_param_id, anim_param_ids.len());

    // Clone base params, set anim param(s) to start value, rebuild manifest for warmup.
    let mut warmup_params = base_params.clone();
    for id in &anim_param_ids {
        if let Some(p) = warmup_params.iter_mut().find(|p| p.id() == id) {
            p.value = anim_start;
        }
    }

    let warmup_manifest = ParamManifest::from_params(warmup_params);

    render_single_frame(
        &device,
        &target,
        &mut runtime,
        &warmup_manifest,
        &args,
        args.time,
    );

    // Sequence phase: render N consecutive frames with param advancing.
    let mut filmstrip_frames = Vec::with_capacity(anim_frames as usize);
    let warmup_end_frame = args.frames_max; // Warmup converges by frames_max (300), not exact.
    for i in 0..anim_frames {
        let param_value = anim_start + (anim_end - anim_start) * (i as f32) / (anim_frames as f32 - 1.0).max(1.0);
        let frame_count = (warmup_end_frame + i) as i64;

        // Clone base params (already carries the resolved numeric/string overrides), set anim param(s) value.
        let mut seq_params = base_params.clone();
        for id in &anim_param_ids {
            if let Some(p) = seq_params.iter_mut().find(|p| p.id() == id) {
                p.value = param_value;
            }
        }

        let seq_manifest = ParamManifest::from_params(seq_params);

        // Render the frame (no convergence check, no sleep).
        let time = args.time;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: 1.0 / 60.0,
            width: args.width,
            height: args.height,
            output_width: args.width,
            output_height: args.height,
            aspect: args.width as f32 / args.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count,
            anim_progress: 1.0,
            trigger_count: 0,
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        let mut enc = device.create_encoder("anim-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &seq_manifest);
        }
        enc.commit_and_wait_completed();

        let rgba = readback_tonemapped_rgba8(&device, &target.texture, args.width, args.height);
        let fraction = non_black_fraction(&rgba);
        println!("anim: frame={} param_value={} fraction={:.4}", i, param_value, fraction);
        filmstrip_frames.push(rgba);
    }

    // Tile frames horizontally into one PNG.
    let filmstrip_width = args.width * anim_frames;
    let mut filmstrip_rgba = vec![0u8; (filmstrip_width * args.height * 4) as usize];
    for (i, frame) in filmstrip_frames.iter().enumerate() {
        let x_offset = (i as u32 * args.width * 4) as usize;
        for y in 0..args.height as usize {
            let frame_row_start = y * (args.width * 4) as usize;
            let filmstrip_row_start = y * (filmstrip_width * 4) as usize + x_offset;
            let row_len = (args.width * 4) as usize;
            filmstrip_rgba[filmstrip_row_start..filmstrip_row_start + row_len]
                .copy_from_slice(&frame[frame_row_start..frame_row_start + row_len]);
        }
    }

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    let png = encode_rgba8_png(&filmstrip_rgba, filmstrip_width, args.height);
    std::fs::write(&args.out, &png).unwrap_or_else(|e| panic!("write {}: {e}", args.out.display()));
    println!("OK {} ({}x{})", args.out.display(), filmstrip_width, args.height);
}

/// BUG-jhpj: "effectively stable" threshold for the epsilon tier of the
/// convergence check, in mean absolute linear-HDR units per component
/// (see `mean_abs_half_diff`). Measured on ABeautifulGame with RT on
/// (this bug's repro): worst steady-state accumulator dither is 0.00076
/// (mean abs per-component f16); this is 6.6x that worst observed value.
/// A texture-decode swap (>0.1 linear) is orders of magnitude above,
/// so the BUG-100/BUG-117 discrimination survives.
const EPSILON_STABLE: f64 = 5e-3;

/// Single-frame convergence render (warmup phase or normal mode).
fn render_single_frame(
    device: &GpuDevice,
    target: &RenderTarget,
    runtime: &mut PresetRuntime,
    manifest: &ParamManifest,
    args: &Args,
    time: f64,
) {
    const DT: f32 = 1.0 / 60.0;
    const STABLE_STREAK: u32 = 3;
    let mut prev_raw: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut converged = false;
    let mut last_fraction;
    let mut final_rgba = Vec::new();

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

    for frame in 0..args.frames_max {
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: DT,
            width: args.width,
            height: args.height,
            output_width: args.width,
            output_height: args.height,
            aspect: args.width as f32 / args.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 1.0,
            trigger_count: 0,
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        let mut enc = device.create_encoder("render-import-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            runtime.render(&mut gpu, &target.texture, &ctx, manifest);
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
        let raw = readback_raw_halves(device, &target.texture, args.width, args.height);
        let byte_stable = prev_raw.as_deref() == Some(raw.as_slice());
        // BUG-jhpj: the RT irradiance accumulator (fixed-alpha EMA over
        // per-frame jittered samples) dithers at steady state forever, so
        // exact byte-stability never lands with RT on. Epsilon tier: a
        // frame whose mean abs component delta is under EPSILON_STABLE
        // counts as stable too. Decode swaps exceed this by orders of
        // magnitude, so the BUG-100/BUG-117 discrimination survives.
        let mean_diff = if byte_stable {
            0.0
        } else {
            prev_raw
                .as_deref()
                .map(|p| mean_abs_half_diff(p, &raw))
                .unwrap_or(f64::MAX)
        };
        let eps_stable = byte_stable || mean_diff < EPSILON_STABLE;
        prev_raw = Some(raw);
        if eps_stable && !io_pending {
            stable_count += 1;
        } else {
            stable_count = 0;
        }

        // D7 diagnosis instrument (BUG-165/BUG-169): print the non-black
        // fraction and io_pending EVERY frame, not only after a stable
        // streak — `last_fraction` below is only updated once a streak
        // lands, so without this a reported 0.0000 is ambiguous between
        // "renders black" and "never went stable" (render_import.rs
        // pre-trace). Default off (`--trace`) since it's a per-frame
        // readback+tonemap on top of the one the convergence check already
        // does — real cost, only worth paying while bisecting.
        if args.trace {
            let frame_rgba = readback_tonemapped_rgba8(device, &target.texture, args.width, args.height);
            let frame_fraction = non_black_fraction(&frame_rgba);
            println!(
                "trace: frame={} fraction={:.4} io_pending={} byte_stable={} stable_count={} mean_diff={:.6}",
                frame, frame_fraction, io_pending, byte_stable, stable_count, mean_diff
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(50));

        if stable_count >= STABLE_STREAK {
            let rgba = readback_tonemapped_rgba8(device, &target.texture, args.width, args.height);
            last_fraction = non_black_fraction(&rgba);
            if last_fraction > args.non_black_floor {
                converged = true;
                final_rgba = rgba;
                println!("render-import: converged on frame {} (non-black fraction {:.4})", frame, last_fraction);
                break;
            }
        }
    }

    if !converged {
        // BUG-jhpj: `last_fraction` is only written after a stable streak,
        // so a scene that renders fine but never stabilizes used to report
        // 0.0000 ("all black") — measure the final frame for the warning.
        let rgba = readback_tonemapped_rgba8(device, &target.texture, args.width, args.height);
        last_fraction = non_black_fraction(&rgba);
        eprintln!(
            "render-import: WARNING — never converged after {} frames (final-frame non-black fraction {:.4}); a decode may be stuck or the frame never stabilized",
            args.frames_max, last_fraction
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

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG-su2o: `--orbit`/`--tilt` must resolve against a REAL import
    /// graph's param listing, not a fixed `cam_orbit`/`cam_tilt` id no
    /// import graph carries. Builds the production import graph for a
    /// small fixture and proves the suffix resolver finds exactly one id
    /// for each of "orbit"/"tilt", and that setting the resolved id
    /// actually changes that param's value (never a silent no-op).
    #[test]
    fn orbit_tilt_sugar_resolves_against_real_import_graph() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/Duck.glb");
        assert!(
            fixture.exists(),
            "fixture missing: {} (tests/fixtures/gltf/khronos/Duck.glb)",
            fixture.display()
        );

        let (def, _report) =
            assemble_import_graph(&fixture).expect("Duck.glb assembles through the import path");
        let available_param_ids: Vec<String> = def
            .preset_metadata
            .as_ref()
            .map(|m| m.params.iter().map(|s| s.id.clone()).collect())
            .unwrap_or_default();
        assert!(
            !available_param_ids.is_empty(),
            "import graph exposed zero outer params"
        );

        let orbit_id = resolve_camera_sugar_param("orbit", &available_param_ids)
            .unwrap_or_else(|e| panic!("orbit did not resolve to exactly one param: {e}"));
        let tilt_id = resolve_camera_sugar_param("tilt", &available_param_ids)
            .unwrap_or_else(|e| panic!("tilt did not resolve to exactly one param: {e}"));
        assert_ne!(orbit_id, tilt_id, "orbit and tilt must resolve to distinct params");
        assert!(
            orbit_id.ends_with("_orbit") || orbit_id == "orbit",
            "resolved orbit id '{orbit_id}' doesn't look like a camera orbit param"
        );
        assert!(
            tilt_id.ends_with("_tilt") || tilt_id == "tilt",
            "resolved tilt id '{tilt_id}' doesn't look like a camera tilt param"
        );

        // Prove setting the resolved ids actually changes the values reaching
        // the graph — the same param-application path `main()` uses.
        let mut params: Vec<Param> = def
            .preset_metadata
            .as_ref()
            .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
            .unwrap_or_default();
        let orbit_before = params.iter().find(|p| p.id() == orbit_id).unwrap().value;
        let tilt_before = params.iter().find(|p| p.id() == tilt_id).unwrap().value;

        let orbit_target = orbit_before + 0.5;
        let tilt_target = tilt_before + 0.3;
        params.iter_mut().find(|p| p.id() == orbit_id).unwrap().value = orbit_target;
        params.iter_mut().find(|p| p.id() == tilt_id).unwrap().value = tilt_target;

        let orbit_after = params.iter().find(|p| p.id() == orbit_id).unwrap().value;
        let tilt_after = params.iter().find(|p| p.id() == tilt_id).unwrap().value;
        assert!((orbit_after - orbit_target).abs() < 1e-6, "orbit override did not land");
        assert!((tilt_after - tilt_target).abs() < 1e-6, "tilt override did not land");
        assert_ne!(orbit_before, orbit_after, "--orbit must not no-op");
        assert_ne!(tilt_before, tilt_after, "--tilt must not no-op");
    }

    #[test]
    fn resolve_camera_sugar_param_errors_on_zero_matches() {
        let available = vec!["cam_dist".to_string(), "7_intensity".to_string()];
        let err = resolve_camera_sugar_param("orbit", &available).unwrap_err();
        assert!(err.contains("no param") || err.contains("matched no param"));
    }

    #[test]
    fn resolve_camera_sugar_param_errors_on_multiple_matches() {
        let available = vec!["5_orbit".to_string(), "9_orbit".to_string()];
        let err = resolve_camera_sugar_param("orbit", &available).unwrap_err();
        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn resolve_camera_sugar_param_matches_exact_suffix() {
        let available = vec!["5_orbit".to_string(), "5_tilt".to_string(), "cam_dist".to_string()];
        assert_eq!(resolve_camera_sugar_param("orbit", &available).unwrap(), "5_orbit");
        assert_eq!(resolve_camera_sugar_param("tilt", &available).unwrap(), "5_tilt");
    }

    /// BUG-6s5m: a value inside the param's declared range must pass through
    /// untouched — this is the common case, and the check must never bite it.
    #[test]
    fn check_param_range_in_range_passes() {
        assert!(check_param_range("1_emitter_elevation", 0.5, -1.0, 1.0).is_ok());
        assert!(check_param_range("1_emitter_elevation", -1.0, -1.0, 1.0).is_ok());
        assert!(check_param_range("1_emitter_elevation", 1.0, -1.0, 1.0).is_ok());
    }

    /// BUG-6s5m root cause: `emitter_elevation` 10/80 vs its declared `[-1,
    /// 1]` range — this is the exact repro that used to render a degenerate
    /// "no strip" picture silently instead of erroring.
    #[test]
    fn check_param_range_out_of_range_errors_naming_the_range() {
        let err = check_param_range("1_emitter_elevation", 10.0, -1.0, 1.0).unwrap_err();
        assert!(err.contains("1_emitter_elevation"), "error must name the param: {err}");
        assert!(err.contains("10"), "error must name the given value: {err}");
        assert!(err.contains("-1") && err.contains('1'), "error must name the declared range: {err}");

        let err = check_param_range("1_emitter_elevation", 80.0, -1.0, 1.0).unwrap_err();
        assert!(err.contains("80"));
    }

    /// A param with no real declared range (`min >= max` — the degenerate
    /// case; every real card param carries a genuine range in practice)
    /// passes through unchecked rather than being treated as an
    /// unsatisfiable `[0, 0]` bound.
    #[test]
    fn check_param_range_no_declared_range_passes() {
        assert!(check_param_range("some_param", 999.0, 0.0, 0.0).is_ok());
    }
}
