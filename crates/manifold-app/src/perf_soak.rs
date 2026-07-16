//! `cargo xtask perf-soak <project> --seconds N [--start <beats>]
//! [--update-baseline]` — PERF_BUDGET_GATE_DESIGN.md P1.
//!
//! Loads a REAL `.manifold` project through the same load path the app uses
//! (`manifold_io::loader::load_project_with` + `install_embedded_presets`,
//! same call `fixtures.rs`'s `project_scene` makes), builds a headless
//! `ContentThread` (`headless_harness::headless_content_thread` — the same
//! construction `journey_proof.rs`'s export harness and the BUG-035/037
//! regression guards already use), and drives it frame-by-frame through the
//! REAL, unmodified `tick_frame`/`FrameTimer::wait_for_deadline` pair — the
//! exact pacing and per-frame work path the live app runs on stage. No new
//! timing framework: per-frame wall time comes from `manifold-profiler`'s
//! existing `FrameRecord.wall_time_ms` (the same collector the in-app
//! backtick-key profiler uses), read back from the `frames.jsonl` it already
//! writes. This tool only adds: the headless drive loop, min/p50 (missing
//! from `SessionSummary`), and the baseline-JSON gate (D3/D4).
//!
//! Exit codes: 0 = pass, 1 = threshold failure (I1/I2), 2 = usage error,
//! 3 = run failure (load/tick error).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use manifold_profiler::FrameRecord;

use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

/// Entry dispatched from `main()` when `argv[1] == "perf-soak"`. `args` is
/// the argument slice starting at `"perf-soak"`. Never returns normally —
/// every path ends in `std::process::exit` (mirrors `ui_snapshot::run`'s
/// convention).
pub fn run(args: &[String]) -> ! {
    let project_path = match args.get(1) {
        Some(p) if !p.starts_with("--") => p.clone(),
        _ => usage_exit("missing <project> argument"),
    };

    let seconds = match arg_value(args, "--seconds") {
        Some(s) => match s.parse::<f64>() {
            Ok(v) if v > 0.0 => v,
            _ => usage_exit("--seconds must be a positive number"),
        },
        None => usage_exit("--seconds N is required"),
    };

    let start_beats = match arg_value(args, "--start") {
        Some(s) => match s.parse::<f64>() {
            Ok(v) => Some(v),
            Err(_) => usage_exit("--start must be a number of beats"),
        },
        None => None,
    };

    let update_baseline = args.iter().any(|a| a == "--update-baseline");
    let profile_mode = args.iter().any(|a| a == "--profile");

    // I4: a profiled run never reaches the baseline write and never sets a
    // failing exit code — rejected outright rather than silently ignoring
    // one of the two flags (no-silent-fallbacks).
    if profile_mode && update_baseline {
        usage_exit("--profile cannot be combined with --update-baseline (I4: profiled runs never write a baseline)");
    }

    let result = if profile_mode {
        run_profile(&project_path, seconds, start_beats)
    } else {
        run_soak(&project_path, seconds, start_beats, update_baseline)
    };

    match result {
        // I4: a profiled run always reports, never judges — `run_profile`
        // itself only ever returns `Ok(true)` on success (see its doc).
        Ok(gate_passed) => std::process::exit(if gate_passed { 0 } else { 1 }),
        Err(e) => {
            eprintln!("perf-soak: {e}");
            std::process::exit(3);
        }
    }
}

fn usage_exit(msg: &str) -> ! {
    eprintln!("perf-soak: {msg}");
    eprintln!(
        "usage: cargo xtask perf-soak <project.manifold> --seconds N \
         [--start <beats>] [--update-baseline] [--profile]"
    );
    std::process::exit(2);
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// Returns `Ok(true)` if the gate passed, `Ok(false)` if it failed a
/// threshold (I1/I2) — the process still exits cleanly in both cases, only
/// the exit code differs. `Err` is a run failure (load, tick, or IO error).
fn run_soak(
    project_path_str: &str,
    seconds: f64,
    start_beats: Option<f64>,
    update_baseline: bool,
) -> Result<bool, String> {
    let project_path = Path::new(project_path_str);

    // Same load path `fixtures.rs`'s `project_scene` uses — the app's real
    // `ProjectIOService::open_project_from_path` route, with the
    // embedded-preset install hook so project-local forked presets resolve
    // correctly (BUG-036).
    let project = manifold_io::loader::load_project_with(
        project_path,
        crate::project_io::install_embedded_presets,
    )
    .map_err(|e| format!("failed to load project '{}': {e}", project_path.display()))?;

    let width = project.settings.output_width.max(1) as u32;
    let height = project.settings.output_height.max(1) as u32;
    let frame_rate = project.settings.frame_rate as f64;
    let bpm = project.settings.bpm;

    let mut ct = headless_content_thread(project, width, height);
    ct.timer.set_target_fps(frame_rate);
    // Same real-time thread scheduling `ContentThread::run()` applies before
    // its own loop — without it `wait_for_deadline`'s `mach_wait_until` calls
    // pace at roughly half rate on a normally-scheduled thread (see
    // `apply_realtime_thread_policy`'s doc comment for the measured gap).
    crate::content_thread::apply_realtime_thread_policy(frame_rate);

    if let Some(beats) = start_beats {
        ct.handle_command(ContentCommand::SeekToBeat(manifold_core::Beats(beats)));
    }
    ct.handle_command(ContentCommand::Play);

    let gpu_name = ct
        .content_pipeline
        .native_device()
        .map(|d| d.device_name())
        .unwrap_or_else(|| "unknown".to_string());

    ct.profiler = Some(manifold_profiler::ProfileSession::new(
        project_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string()),
        project_path.display().to_string(),
        (width, height),
        frame_rate as f32,
        gpu_name,
    ));

    // Unbounded (not the production bounded-4 channel): this harness never
    // reads it for its own purposes, and an unbounded `send` never blocks —
    // draining on a background thread just keeps memory bounded over a long
    // soak (same convention `journey_proof.rs`'s `run_headless_export` uses).
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("perf-soak-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .map_err(|e| format!("spawn drain thread: {e}"))?;

    eprintln!(
        "perf-soak: soaking '{}' for {seconds:.1}s at {frame_rate:.1} fps \
         ({width}x{height}, bpm={:.1}{})",
        project_path.display(),
        bpm.0,
        start_beats.map(|b| format!(", start={b:.1} beats")).unwrap_or_default(),
    );

    // Real-time pacing, D5: the SAME `FrameTimer::wait_for_deadline` +
    // `tick_frame` pair the production `ContentThread::run()` loop calls —
    // no separate sleep/pacing logic invented here.
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);
    }

    let session_dir = ct
        .profiler
        .as_mut()
        .expect("profiler set above")
        .stop_and_dump()
        .map_err(|e| format!("profiler dump failed: {e}"))?;

    drop(state_tx);
    drain.join().map_err(|_| "drain thread panicked".to_string())?;

    let (stats, worst_frame_breakdown) = load_stats(&session_dir)?;
    eprintln!(
        "perf-soak: {} frames — min={:.2}ms p50={:.2}ms p95={:.2}ms max={:.2}ms",
        stats.frame_count, stats.min_ms, stats.p50_ms, stats.p95_ms, stats.max_ms
    );
    if let Some(ref w) = worst_frame_breakdown {
        eprintln!(
            "perf-soak: worst frame #{} @ beat {:.2} bar {} = {:.2}ms \
             (midi={:.2} sync={:.2} engine={:.2} render={:.2} gpu_poll={:.2} cleanup={:.2})",
            w.index,
            w.beat,
            w.bar,
            w.wall_time_ms,
            w.content_thread.midi_input_ms,
            w.content_thread.sync_controllers_ms,
            w.content_thread.engine_tick_ms,
            w.content_thread.render_content_ms,
            w.content_thread.gpu_poll_ms,
            w.content_thread.cleanup_ms,
        );
    }

    let baseline_path = baseline_path_for(project_path);
    let machine = current_machine();

    // Stats JSON: written every run (not flag-gated — only the BASELINE
    // write is flag-gated per I3/D4). Sits next to the profiling session
    // for a human/agent to read the acceptance-demo evidence from.
    let stats_json = serde_json::json!({
        "mode": "project",
        "project": project_path.display().to_string(),
        "machine": machine,
        "seconds": seconds,
        "start_beats": start_beats,
        "frame_count": stats.frame_count,
        "min_ms": stats.min_ms,
        "p50_ms": stats.p50_ms,
        "p95_ms": stats.p95_ms,
        "max_ms": stats.max_ms,
        "worst_frame": worst_frame_breakdown,
        "profiling_session_dir": session_dir.display().to_string(),
    });
    let stats_path = session_dir.join("perf_soak_stats.json");
    std::fs::write(&stats_path, serde_json::to_string_pretty(&stats_json).unwrap())
        .map_err(|e| format!("write {}: {e}", stats_path.display()))?;
    eprintln!("perf-soak: stats written to {}", stats_path.display());

    // I1 — hard fail: any frame over 20ms (max_ms > 20 <=> some frame > 20ms).
    const HARD_FAIL_MS: f64 = 20.0;
    let hard_fail = stats.max_ms > HARD_FAIL_MS;
    if hard_fail {
        eprintln!(
            "perf-soak: FAIL (I1) — max frame {:.2}ms exceeds the {HARD_FAIL_MS}ms hard budget",
            stats.max_ms
        );
    }

    if update_baseline {
        // D4/I3: baseline write is flag-gated — this is the ONLY place the
        // baseline file is written.
        let baseline = serde_json::json!({
            "machine": machine,
            "project": project_path.display().to_string(),
            "seconds": seconds,
            "start_beats": start_beats,
            "min_ms": stats.min_ms,
            "p50_ms": stats.p50_ms,
            "p95_ms": stats.p95_ms,
            "max_ms": stats.max_ms,
            "recorded_at": iso_now(),
        });
        if let Some(parent) = baseline_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&baseline_path, serde_json::to_string_pretty(&baseline).unwrap())
            .map_err(|e| format!("write {}: {e}", baseline_path.display()))?;
        eprintln!("perf-soak: baseline written to {}", baseline_path.display());
        return Ok(!hard_fail);
    }

    // D3 — regression fail: p95 > baseline p95 * 1.15. No baseline yet is a
    // run failure, not a silent pass (no-silent-fallbacks) — the executor
    // must create one deliberately with --update-baseline first.
    let baseline_raw = std::fs::read_to_string(&baseline_path).map_err(|e| {
        format!(
            "no baseline at {} ({e}) — run once with --update-baseline first",
            baseline_path.display()
        )
    })?;
    let baseline: serde_json::Value = serde_json::from_str(&baseline_raw)
        .map_err(|e| format!("parse {}: {e}", baseline_path.display()))?;
    let baseline_p95 = baseline["p95_ms"].as_f64().ok_or("baseline missing p95_ms")?;
    let baseline_machine = baseline["machine"].as_str().unwrap_or("unknown");
    if baseline_machine != machine {
        eprintln!(
            "perf-soak: WARNING — baseline was recorded on '{baseline_machine}', this run is on \
             '{machine}'. D4: baselines are only meaningful on the machine that recorded them; \
             this comparison is stated, not authoritative."
        );
    }

    let regression_ratio = if baseline_p95 > 0.0 { stats.p95_ms / baseline_p95 } else { 1.0 };
    const REGRESSION_BAND: f64 = 1.15;
    let regressed = regression_ratio > REGRESSION_BAND;
    if regressed {
        eprintln!(
            "perf-soak: FAIL (I2) — p95 {:.2}ms is {:.1}% above baseline {:.2}ms (band: {:.0}%)",
            stats.p95_ms,
            (regression_ratio - 1.0) * 100.0,
            baseline_p95,
            (REGRESSION_BAND - 1.0) * 100.0
        );
    } else {
        eprintln!(
            "perf-soak: p95 {:.2}ms vs baseline {:.2}ms ({:+.1}%) — within the {:.0}% band",
            stats.p95_ms,
            baseline_p95,
            (regression_ratio - 1.0) * 100.0,
            (REGRESSION_BAND - 1.0) * 100.0
        );
    }

    let passed = !hard_fail && !regressed;
    if passed {
        eprintln!("perf-soak: PASS");
    }
    Ok(passed)
}

/// Sampler capacity in spans (two counter samples per span). Sized generously
/// against the Liveschool fixture's per-frame dispatch count (~an order of
/// magnitude more dispatches than `freeze_profile`'s single-preset runs, D6);
/// the capacity check below reports actual usage/overflow rather than
/// silently truncating if a heavier project needs more.
const PROFILE_SAMPLER_MAX_SPANS: usize = 8192;

/// Worst-frame count reported in the attribution JSON (D6/P2 default).
const PROFILE_WORST_FRAMES_K: usize = 5;

/// One node's accumulated attribution within a single profiled frame, keyed
/// by the scoped tag (`"{scope}:s{idx}"`) that both the CPU `StepProfile` and
/// the GPU `GpuProfiledSpan` carry — the D6 join key.
struct ProfiledNode {
    type_id: String,
    gpu_ms: f64,
    cpu_us: f64,
}

/// One profiled frame's attribution: every node's share plus whatever GPU
/// time no executor's tag claimed (compositor/blend/tonemap passes — D6's
/// "compositor/untagged" row, never dropped).
struct ProfiledFrame {
    index: u64,
    total_gpu_ms: f64,
    /// Dispatches that ran unprofiled because the sampler buffer filled up
    /// (summed across both command buffers this frame).
    overflow: usize,
    /// Spans actually recorded this frame (summed across both command
    /// buffers) — the D6 capacity check compares this against
    /// `PROFILE_SAMPLER_MAX_SPANS` / 2 (max_spans).
    spans_used: usize,
    untagged_ms: f64,
    nodes: std::collections::HashMap<String, ProfiledNode>,
}

/// `cargo xtask perf-soak <project> --seconds N [--start <beats>] --profile`
/// — PERF_BUDGET_GATE_DESIGN.md P2 / D6 attribution pass. Re-runs the same
/// real-time-paced window as `run_soak`, but with per-dispatch GPU
/// attribution profiling on: forces `composite_serial` (one shared
/// compositor command buffer for the sampler — D6 correction), joins GPU
/// spans back to CPU step costs by the scoped tag `"{scope}:s{idx}"`, and
/// reports the `PROFILE_WORST_FRAMES_K` frames with the highest total GPU
/// time as a per-node breakdown.
///
/// I4: this function's return is `Ok(true)` on every successful run —
/// profiled mode reports, it never judges pass/fail — and it never touches
/// the baseline file (see `run()`'s `--update-baseline` rejection above).
/// `Err` is a run failure (load/tick/device error), matching `run_soak`.
fn run_profile(project_path_str: &str, seconds: f64, start_beats: Option<f64>) -> Result<bool, String> {
    let project_path = Path::new(project_path_str);

    let project = manifold_io::loader::load_project_with(
        project_path,
        crate::project_io::install_embedded_presets,
    )
    .map_err(|e| format!("failed to load project '{}': {e}", project_path.display()))?;

    let width = project.settings.output_width.max(1) as u32;
    let height = project.settings.output_height.max(1) as u32;
    let frame_rate = project.settings.frame_rate as f64;
    let bpm = project.settings.bpm;

    let mut ct = headless_content_thread(project, width, height);
    ct.timer.set_target_fps(frame_rate);
    crate::content_thread::apply_realtime_thread_policy(frame_rate);

    if let Some(beats) = start_beats {
        ct.handle_command(ContentCommand::SeekToBeat(manifold_core::Beats(beats)));
    }
    ct.handle_command(ContentCommand::Play);

    ct.content_pipeline
        .set_profiling(true, PROFILE_SAMPLER_MAX_SPANS);
    if !ct.content_pipeline.profiling_sampler_ready() {
        return Err(
            "GPU dispatch profiling unsupported on this device (counter sampling at stage \
             boundaries not available)"
                .to_string(),
        );
    }
    // GeneratorRenderer lives on PlaybackEngine::renderers, not on
    // ContentPipeline (see ContentPipeline::take_step_profiles's doc) — its
    // profiling flag is set directly here, before any generator is installed,
    // so `install_layer_generator`'s chain-insertion-time stamp (D6
    // correction) sees it on from the very first frame.
    for renderer in ct.engine.renderers_mut() {
        if let Some(gen_renderer) = renderer
            .as_any_mut()
            .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
        {
            gen_renderer.set_profiling(true);
        }
    }

    eprintln!(
        "perf-soak --profile: profiling '{}' for {seconds:.1}s at {frame_rate:.1} fps \
         ({width}x{height}, bpm={:.1}{}) — forced composite_serial (D6)",
        project_path.display(),
        bpm.0,
        start_beats.map(|b| format!(", start={b:.1} beats")).unwrap_or_default(),
    );

    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("perf-soak-profile-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .map_err(|e| format!("spawn drain thread: {e}"))?;

    let mut frames: Vec<ProfiledFrame> = Vec::new();
    let mut frame_idx: u64 = 0;
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        ct.timer.wait_for_deadline();
        ct.tick_frame(&state_tx);

        let gpu_profiles = ct.content_pipeline.take_gpu_profiles();
        let mut cpu_profiles = ct.content_pipeline.take_step_profiles();
        // GeneratorRenderer lives on PlaybackEngine::renderers, not on
        // ContentPipeline — drained directly here (see
        // ContentPipeline::take_step_profiles's doc).
        for renderer in ct.engine.renderers_mut() {
            if let Some(gen_renderer) = renderer
                .as_any_mut()
                .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
            {
                cpu_profiles.extend(gen_renderer.take_step_profiles());
            }
        }

        let cpu_by_tag: std::collections::HashMap<&str, &manifold_renderer::node_graph::StepProfile> =
            cpu_profiles.iter().map(|p| (p.tag.as_str(), p)).collect();

        let mut frame = ProfiledFrame {
            index: frame_idx,
            total_gpu_ms: 0.0,
            overflow: 0,
            spans_used: 0,
            untagged_ms: 0.0,
            nodes: std::collections::HashMap::new(),
        };
        for (_cb_label, profile) in &gpu_profiles {
            frame.total_gpu_ms += profile.total_ms;
            frame.overflow += profile.overflow;
            frame.spans_used += profile.spans.len();
            for span in &profile.spans {
                // A span whose tag matches no live executor step this frame
                // (empty scope, or a compositor-owned pass — blend/tonemap/
                // LED slicer — with no `Executor` behind it at all) is
                // reported explicitly, never silently dropped (D6).
                match cpu_by_tag.get(span.tag.as_str()) {
                    Some(cpu) => {
                        let entry = frame.nodes.entry(span.tag.clone()).or_insert_with(|| {
                            ProfiledNode {
                                type_id: cpu.type_id.clone(),
                                gpu_ms: 0.0,
                                cpu_us: cpu.cpu_nanos as f64 / 1000.0,
                            }
                        });
                        entry.gpu_ms += span.millis;
                    }
                    None => frame.untagged_ms += span.millis,
                }
            }
        }
        frames.push(frame);
        frame_idx += 1;
    }

    drop(state_tx);
    drain.join().map_err(|_| "drain thread panicked".to_string())?;

    if frames.is_empty() {
        return Err("no frames recorded — soak duration too short?".to_string());
    }

    // D6 capacity check: report, never silently truncate. `max_spans` is the
    // sampler's capacity in spans; a frame using >= it means dispatches were
    // dropped (already visible per-frame as `overflow`, surfaced here as one
    // whole-run verdict too).
    let sampler_capacity_spans = ct.content_pipeline.profiling_sampler_capacity().unwrap_or(0);
    let max_frame_spans_used = frames.iter().map(|f| f.spans_used).max().unwrap_or(0);
    let any_overflow = frames.iter().any(|f| f.overflow > 0);
    if any_overflow {
        eprintln!(
            "perf-soak --profile: WARNING — sampler capacity ({sampler_capacity_spans} spans) \
             overflowed on at least one frame (max used {max_frame_spans_used}); some \
             dispatches ran unprofiled. Increase PROFILE_SAMPLER_MAX_SPANS."
        );
    } else {
        eprintln!(
            "perf-soak --profile: capacity OK — {max_frame_spans_used}/{sampler_capacity_spans} \
             spans on the busiest frame"
        );
    }

    // Worst-K frames by total GPU time.
    let mut ranked: Vec<&ProfiledFrame> = frames.iter().collect();
    ranked.sort_by(|a, b| b.total_gpu_ms.partial_cmp(&a.total_gpu_ms).unwrap_or(std::cmp::Ordering::Equal));
    let worst: Vec<serde_json::Value> = ranked
        .iter()
        .take(PROFILE_WORST_FRAMES_K)
        .map(|f| {
            let mut node_rows: Vec<(&String, &ProfiledNode)> = f.nodes.iter().collect();
            node_rows.sort_by(|a, b| {
                b.1.gpu_ms.partial_cmp(&a.1.gpu_ms).unwrap_or(std::cmp::Ordering::Equal)
            });
            let share_denom = if f.total_gpu_ms > 0.0 { f.total_gpu_ms } else { 1.0 };
            let mut nodes_json: Vec<serde_json::Value> = node_rows
                .iter()
                .map(|(tag, n)| {
                    serde_json::json!({
                        "tag": tag,
                        "type_id": n.type_id,
                        "gpu_ms": n.gpu_ms,
                        "cpu_us": n.cpu_us,
                        "share_of_frame": n.gpu_ms / share_denom,
                    })
                })
                .collect();
            // D6: spans no executor owns (compositor blend/tonemap/LED
            // slicer passes) are reported explicitly, never dropped.
            nodes_json.push(serde_json::json!({
                "tag": "compositor/untagged",
                "type_id": "compositor/untagged",
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

    let profile_json = serde_json::json!({
        "mode": "project",
        "profile": true,
        "project": project_path.display().to_string(),
        "seconds": seconds,
        "start_beats": start_beats,
        "forced_composite_serial": true,
        "frames_measured": frames.len(),
        "sampler_capacity_spans": sampler_capacity_spans,
        "max_frame_spans_used": max_frame_spans_used,
        "capacity_overflow": any_overflow,
        "worst_frames": worst,
    });

    let out_dir = Path::new("target/perf-profile");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let stem = project_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let out_path = out_dir.join(format!("{stem}-profile.json"));
    std::fs::write(&out_path, serde_json::to_string_pretty(&profile_json).unwrap())
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;
    eprintln!("perf-soak --profile: attribution JSON written to {}", out_path.display());
    if let Some(worst) = ranked.first() {
        eprintln!(
            "perf-soak --profile: worst frame #{} = {:.3}ms GPU ({} nodes + untagged {:.3}ms)",
            worst.index,
            worst.total_gpu_ms,
            worst.nodes.len(),
            worst.untagged_ms
        );
    }

    // I4: profiled mode reports, never judges — always Ok(true) on success.
    Ok(true)
}

struct Stats {
    frame_count: usize,
    min_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

/// Read `summary.json` (for `max_ms`/`p95_ms`, already computed) and
/// `frames.jsonl` (for `min_ms`/`p50_ms`, missing from `SessionSummary`, and
/// the worst frame's own per-section breakdown — `worst_frame.index` only
/// names which frame; the section ms live in that frame's own `FrameRecord`).
fn load_stats(session_dir: &Path) -> Result<(Stats, Option<FrameRecord>), String> {
    let summary_raw = std::fs::read_to_string(session_dir.join("summary.json"))
        .map_err(|e| format!("read summary.json: {e}"))?;
    let summary: manifold_profiler::SessionSummary =
        serde_json::from_str(&summary_raw).map_err(|e| format!("parse summary.json: {e}"))?;

    let frames_raw = std::fs::read_to_string(session_dir.join("frames.jsonl"))
        .map_err(|e| format!("read frames.jsonl: {e}"))?;
    let frames: Vec<FrameRecord> = frames_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("parse frames.jsonl line: {e}")))
        .collect::<Result<_, _>>()?;

    if frames.is_empty() {
        return Err("no frames recorded — soak duration too short?".to_string());
    }

    let mut wall_times: Vec<f64> = frames.iter().map(|f| f.wall_time_ms).collect();
    wall_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min_ms = wall_times[0];
    let p50_ms = wall_times[wall_times.len() / 2];

    let worst_frame = summary
        .worst_frame
        .as_ref()
        .and_then(|w| frames.iter().find(|f| f.index == w.index).cloned());

    Ok((
        Stats { frame_count: frames.len(), min_ms, p50_ms, p95_ms: summary.p95_frame_ms, max_ms: summary.max_frame_ms },
        worst_frame,
    ))
}

/// `docs/perf-baselines/<project-stem>.json` (D4) — machine-tagged, one file
/// per project fixture, checked in deliberately via `--update-baseline`.
fn baseline_path_for(project_path: &Path) -> PathBuf {
    let stem = project_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let sanitized: String =
        stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    PathBuf::from("docs/perf-baselines").join(format!("{sanitized}.json"))
}

/// Best-effort machine tag (D4: "Peter's rig is THE machine"). Falls back to
/// "unknown-machine" rather than failing the whole run over a cosmetic field.
fn current_machine() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-machine".to_string())
}

fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}
