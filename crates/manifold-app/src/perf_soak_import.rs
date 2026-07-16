//! Bare-glb import-graph frame loop for
//! `cargo xtask perf-soak <file.glb|.gltf> [--size WxH] [--frames N]
//! [--profile]` — PERF_BUDGET_GATE_DESIGN.md D7 / P2b.
//!
//! Sibling to `perf_soak.rs`'s project-mode loop, NOT a variant of it: this
//! module never touches the project loader, the content thread, or
//! `ContentPipeline`. It drives the EXACT import-graph construction
//! `render_import.rs` (whole file, main + module doc) already proves —
//! `assemble_import_graph` -> `PresetRuntime::from_def_with_device` at
//! `Rgba16Float` into a `RenderTarget` — and reuses that file's
//! io_pending/byte-stability convergence loop verbatim for warmup (D7:
//! "never a wrapper project", the D7-rejected alternative). The two modes
//! share only the stats-JSON emitter shape, the D6 attribution
//! plumbing/constants, and the on-disk output convention
//! (`target/perf-profile/...json`) — nothing else.
//!
//! Import mode is report-only (D7/I3/I4 untouched by it): it never reads or
//! writes a baseline and never sets a threshold exit code. `run_import`
//! returns `Ok(true)` on every successful run; `Err` is a run failure (import
//! error, convergence failure/cap) — the caller in `perf_soak.rs` maps that
//! to exit code 3, matching project mode's run-failure convention.

use std::path::Path;
use std::time::Instant;

use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuDevice;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::readback_raw_halves;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

/// D7 extension dispatch: `.glb`/`.gltf` (case-insensitive) route to this
/// module's loop; everything else stays on `perf_soak.rs`'s P1 project soak.
pub fn is_glb_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".glb") || lower.ends_with(".gltf")
}

const DT: f32 = 1.0 / 60.0;
const DEFAULT_FRAMES: u32 = 300;
/// D7: warmup cap, exit non-zero if convergence never lands by then — same
/// cap `render_import.rs`'s `frames_max` default uses.
const WARMUP_CAP: u32 = 300;
/// D7 / `render_import.rs`: 3 consecutive byte-stable, non-io-pending frames.
const STABLE_STREAK: u32 = 3;
/// D6 default (shared semantics, not the same constant value as project
/// mode's `PROFILE_SAMPLER_MAX_SPANS` — an import graph carries far fewer
/// dispatches than a Liveschool-scale project frame; sized generously against
/// `freeze_profile.rs`'s single-preset sampler (2048), same order of
/// magnitude as one import graph).
const PROFILE_SAMPLER_MAX_SPANS: usize = 2048;
/// D6 default (K worst frames by GPU time).
const PROFILE_WORST_FRAMES_K: usize = 5;

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or_else(|| format!("--size wants WxH, got {s}"))?;
    let width: u32 = w.parse().map_err(|e| format!("bad --size width: {e}"))?;
    let height: u32 = h.parse().map_err(|e| format!("bad --size height: {e}"))?;
    Ok((width, height))
}

fn mk_ctx(frame: u32, width: u32, height: u32) -> PresetContext {
    let time = frame as f64 * DT as f64;
    PresetContext {
        time,
        beat: time * 2.0, // 120 bpm, matches render_import.rs
        dt: DT,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: (frame as f32 / 60.0).min(1.0),
        trigger_count: 0,
    }
}

fn stats4(xs: &[f64]) -> (f64, f64, f64, f64) {
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (s[0], s[s.len() / 2], s[s.len() * 95 / 100], s[s.len() - 1])
}

/// D7 / P2b entry point. `args` is the full `perf-soak` argv slice (`args[0]
/// == "perf-soak"`, `args[1] == glb_path_str`) — same convention
/// `perf_soak::run` uses, so `--size`/`--frames`/`--profile` are found the
/// same way `--seconds`/`--start` are on the project side.
pub fn run_import(glb_path_str: &str, args: &[String]) -> Result<bool, String> {
    let glb_path = Path::new(glb_path_str);

    let (width, height) = match arg_value(args, "--size") {
        Some(s) => parse_size(&s)?,
        None => (1920, 1080),
    };
    let frames: u32 = match arg_value(args, "--frames") {
        Some(s) => {
            let v: u32 = s.parse().map_err(|e| format!("--frames must be a positive integer: {e}"))?;
            if v == 0 {
                return Err("--frames must be > 0".to_string());
            }
            v
        }
        None => DEFAULT_FRAMES,
    };
    let profile_mode = args.iter().any(|a| a == "--profile");

    // EXACT render_import.rs construction — never a wrapper project, never
    // through the loader/content thread (D7).
    let (def, report) = assemble_import_graph(glb_path)
        .map_err(|e| format!("import error for {}: {e}", glb_path.display()))?;
    eprintln!("perf-soak (import): {} -> {report:?}", glb_path.display());

    let params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    let manifest = ParamManifest::from_params(params);

    let device = std::sync::Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&device),
        width,
        height,
        format,
        Some(&manifest),
    )
    .map_err(|e| format!("import graph build failed for {}: {e:?}", glb_path.display()))?;

    let target = RenderTarget::new(&device, width, height, format, "perf-soak-import-target");

    // D7 warmup: render_import.rs's io_pending/byte-stability convergence
    // loop, ported verbatim (cap 300, exit non-zero on cap; the loop's own
    // per-frame sleep is NOT carried here — that sleep exists so a
    // background decode thread gets wall time between polls during warmup,
    // same reasoning as render_import.rs's module doc; it stays confined to
    // warmup and never leaks into the measured window below).
    let mut prev_raw: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut warmup_frame = 0u32;
    let mut converged = false;
    while warmup_frame < WARMUP_CAP {
        let ctx = mk_ctx(warmup_frame, width, height);
        let mut enc = device.create_encoder("perf-soak-import-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        enc.commit_and_wait_completed();

        let io_pending = runtime.io_pending();
        let raw = readback_raw_halves(&device, &target.texture, width, height);
        let byte_stable = prev_raw.as_deref() == Some(raw.as_slice());
        prev_raw = Some(raw);
        if byte_stable && !io_pending {
            stable_count += 1;
        } else {
            stable_count = 0;
        }
        warmup_frame += 1;
        std::thread::sleep(std::time::Duration::from_millis(50));
        if stable_count >= STABLE_STREAK {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err(format!(
            "convergence failed after {WARMUP_CAP} warmup frames ({width}x{height}) — a \
             background texture decode may be stuck (render-import's WARNING case)"
        ));
    }
    eprintln!("perf-soak (import): converged after {warmup_frame} warmup frames");

    let stats_json = if profile_mode {
        run_profiled(&mut runtime, &device, &target, &manifest, width, height, frames, warmup_frame)?
    } else {
        run_measured(&mut runtime, &device, &target, &manifest, width, height, frames, warmup_frame)?
    };

    let out_dir = Path::new("target/perf-profile");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let stem = glb_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "import".to_string());
    let suffix = if profile_mode { "profile" } else { "stats" };
    let out_path = out_dir.join(format!("{stem}-import-{suffix}.json"));
    std::fs::write(&out_path, serde_json::to_string_pretty(&stats_json).unwrap())
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;
    eprintln!("perf-soak (import): stats written to {}", out_path.display());

    // D7/I3/I4: import mode never reaches a baseline write and never returns
    // a failing threshold verdict — every successful run reports `Ok(true)`.
    Ok(true)
}

/// D7 unprofiled measured window: NO readbacks, NO pacing sleep, back-to-back
/// frames — the honest sustained-load measurement (D7: real-time pacing's
/// decode-readahead rationale doesn't apply, import graphs carry no
/// wall-clock-coupled media). Reports min/p50/p95/max of true GPU time
/// (`commit_and_wait_completed_timed`) and of CPU encode wall time.
#[allow(clippy::too_many_arguments)]
fn run_measured(
    runtime: &mut PresetRuntime,
    device: &std::sync::Arc<GpuDevice>,
    target: &RenderTarget,
    manifest: &ParamManifest,
    width: u32,
    height: u32,
    frames: u32,
    frame_base: u32,
) -> Result<serde_json::Value, String> {
    let mut cpu_ms = vec![0.0f64; frames as usize];
    let mut gpu_ms = vec![0.0f64; frames as usize];
    for i in 0..frames {
        let ctx = mk_ctx(frame_base + i, width, height);
        let mut enc = device.create_encoder("perf-soak-import-timed");
        let t0 = Instant::now();
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            runtime.render(&mut gpu, &target.texture, &ctx, manifest);
        }
        cpu_ms[i as usize] = t0.elapsed().as_secs_f64() * 1000.0;
        // No readback, no sleep here — the measured window is back-to-back
        // (D7 forbidden-moves list).
        gpu_ms[i as usize] = enc.commit_and_wait_completed_timed() * 1000.0;
    }

    let (g_min, g50, g95, gmax) = stats4(&gpu_ms);
    let (c_min, c50, c95, cmax) = stats4(&cpu_ms);
    eprintln!(
        "perf-soak (import): {frames} frames @ {width}x{height} — gpu min={g_min:.3} \
         p50={g50:.3} p95={g95:.3} max={gmax:.3} ms | cpu-encode min={c_min:.3} p50={c50:.3} \
         p95={c95:.3} max={cmax:.3} ms"
    );

    Ok(serde_json::json!({
        "mode": "import",
        "size": [width, height],
        "frame_count": frames,
        "warmup_frames": frame_base,
        "min_ms": g_min,
        "p50_ms": g50,
        "p95_ms": g95,
        "max_ms": gmax,
        "cpu_encode_ms": {
            "min": c_min,
            "p50": c50,
            "p95": c95,
            "max": cmax,
        },
    }))
}

/// D6/D7 profiled attribution pass on import mode: identical semantics to
/// project mode's `--profile` (K=5 worst frames by GPU time, per-dispatch
/// attribution, shares-not-totals honesty) — but the import graph is a
/// single `PresetRuntime`/`Executor` with one command buffer per frame, so
/// there is no `composite_serial` forcing to do (D6's correction applies to
/// the multi-executor project-compositor shape only; nothing here has that
/// shape). I4 still holds: this function never touches a baseline and always
/// returns `Ok`-shaped JSON, never a threshold verdict.
#[allow(clippy::too_many_arguments)]
fn run_profiled(
    runtime: &mut PresetRuntime,
    device: &std::sync::Arc<GpuDevice>,
    target: &RenderTarget,
    manifest: &ParamManifest,
    width: u32,
    height: u32,
    frames: u32,
    frame_base: u32,
) -> Result<serde_json::Value, String> {
    let Some(sampler) = device.create_timestamp_sampler(PROFILE_SAMPLER_MAX_SPANS) else {
        return Err(
            "GPU dispatch profiling unsupported on this device (counter sampling at stage \
             boundaries not available)"
                .to_string(),
        );
    };
    runtime.set_profiling(true);
    runtime.set_profile_scope("import");

    struct ProfiledFrame {
        index: u32,
        total_gpu_ms: f64,
        overflow: usize,
        spans_used: usize,
        untagged_ms: f64,
        nodes: std::collections::HashMap<String, (String, f64, f64)>, // tag -> (type_id, gpu_ms, cpu_us)
    }

    let mut recorded: Vec<ProfiledFrame> = Vec::with_capacity(frames as usize);
    for i in 0..frames {
        let ctx = mk_ctx(frame_base + i, width, height);
        let mut enc = device.create_encoder("perf-soak-import-profiled");
        enc.enable_dispatch_profiling(sampler.clone(), device);
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            runtime.render(&mut gpu, &target.texture, &ctx, manifest);
        }
        let profile = enc.commit_and_wait_profiled(device);
        let cpu_profiles = runtime.take_step_profiles();
        let cpu_by_tag: std::collections::HashMap<&str, &manifold_renderer::node_graph::StepProfile> =
            cpu_profiles.iter().map(|p| (p.tag.as_str(), p)).collect();

        let mut frame = ProfiledFrame {
            index: frame_base + i,
            total_gpu_ms: profile.total_ms,
            overflow: profile.overflow,
            spans_used: profile.spans.len(),
            untagged_ms: 0.0,
            nodes: std::collections::HashMap::new(),
        };
        for span in &profile.spans {
            match cpu_by_tag.get(span.tag.as_str()) {
                Some(cpu) => {
                    let entry = frame
                        .nodes
                        .entry(span.tag.clone())
                        .or_insert_with(|| (cpu.type_id.clone(), 0.0, cpu.cpu_nanos as f64 / 1000.0));
                    entry.1 += span.millis;
                }
                // D6: a span whose tag matches no live executor step this
                // frame is reported explicitly, never silently dropped.
                None => frame.untagged_ms += span.millis,
            }
        }
        recorded.push(frame);
    }

    let sampler_capacity_spans = sampler.max_spans();
    let max_frame_spans_used = recorded.iter().map(|f| f.spans_used).max().unwrap_or(0);
    let any_overflow = recorded.iter().any(|f| f.overflow > 0);
    if any_overflow {
        eprintln!(
            "perf-soak (import) --profile: WARNING — sampler capacity ({sampler_capacity_spans} \
             spans) overflowed on at least one frame (max used {max_frame_spans_used}); some \
             dispatches ran unprofiled. Increase PROFILE_SAMPLER_MAX_SPANS."
        );
    } else {
        eprintln!(
            "perf-soak (import) --profile: capacity OK — {max_frame_spans_used}/\
             {sampler_capacity_spans} spans on the busiest frame"
        );
    }

    let mut ranked: Vec<&ProfiledFrame> = recorded.iter().collect();
    ranked.sort_by(|a, b| b.total_gpu_ms.partial_cmp(&a.total_gpu_ms).unwrap_or(std::cmp::Ordering::Equal));
    let worst: Vec<serde_json::Value> = ranked
        .iter()
        .take(PROFILE_WORST_FRAMES_K)
        .map(|f| {
            let mut node_rows: Vec<(&String, &(String, f64, f64))> = f.nodes.iter().collect();
            node_rows.sort_by(|a, b| (b.1).1.partial_cmp(&(a.1).1).unwrap_or(std::cmp::Ordering::Equal));
            let share_denom = if f.total_gpu_ms > 0.0 { f.total_gpu_ms } else { 1.0 };
            let mut nodes_json: Vec<serde_json::Value> = node_rows
                .iter()
                .map(|(tag, (type_id, gpu_ms, cpu_us))| {
                    serde_json::json!({
                        "tag": tag,
                        "type_id": type_id,
                        "gpu_ms": gpu_ms,
                        "cpu_us": cpu_us,
                        "share_of_frame": gpu_ms / share_denom,
                    })
                })
                .collect();
            nodes_json.push(serde_json::json!({
                "tag": "untagged",
                "type_id": "untagged",
                "gpu_ms": f.untagged_ms,
                "cpu_us": 0.0,
                "share_of_frame": f.untagged_ms / share_denom,
            }));
            serde_json::json!({
                "frame_index": f.index,
                "total_gpu_ms": f.total_gpu_ms,
                "overflow_dispatches": f.overflow,
                "spans_used": f.spans_used,
                "nodes": nodes_json,
            })
        })
        .collect();

    if let Some(w) = ranked.first() {
        eprintln!(
            "perf-soak (import) --profile: worst frame #{} = {:.3}ms GPU ({} nodes + untagged \
             {:.3}ms)",
            w.index,
            w.total_gpu_ms,
            w.nodes.len(),
            w.untagged_ms
        );
    }

    Ok(serde_json::json!({
        "mode": "import",
        "profile": true,
        "size": [width, height],
        "frame_count": frames,
        "warmup_frames": frame_base,
        "sampler_capacity_spans": sampler_capacity_spans,
        "max_frame_spans_used": max_frame_spans_used,
        "capacity_overflow": any_overflow,
        "worst_frames": worst,
    }))
}
