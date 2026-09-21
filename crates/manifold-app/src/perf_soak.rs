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
//!
//! D7 / P2b: a bare `.glb`/`.gltf` input dispatches to `perf_soak_import.rs`
//! instead — a sibling frame loop over the production import graph
//! (`assemble_import_graph`), never a wrapper project. That mode is
//! report-only: exit codes there are 0 = pass, 2 = usage error (this file's
//! dispatcher, see `run()`), 3 = run failure (import/convergence); it never
//! returns exit code 1 (I3/I4 don't apply to it).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use manifold_profiler::FrameRecord;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

/// Entry dispatched from `main()` when `argv[1] == "perf-soak"`. `args` is
/// the argument slice starting at `"perf-soak"`. Never returns normally —
/// every path ends in `std::process::exit` (mirrors `ui_snapshot::run`'s
/// convention).
pub fn run(args: &[String]) -> ! {
    // Subcommand dispatch happens before the normal app logger setup. Keep
    // production load/warmup diagnostics visible for the headless path too.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let project_path = match args.get(1) {
        Some(p) if !p.starts_with("--") => p.clone(),
        _ => usage_exit("missing <project|glb> argument"),
    };

    // D7 extension dispatch: `.glb`/`.gltf` route to the import-graph sibling
    // loop (never a wrapper project, never through the loader/content
    // thread); everything else stays on this file's P1 project soak.
    if crate::perf_soak_import::is_glb_path(&project_path) {
        // D7 flag matrix: `--seconds`/`--start`/`--update-baseline` are
        // project-mode-only — reject rather than silently ignore
        // (no-silent-fallbacks).
        for flag in ["--seconds", "--start", "--update-baseline", "--report-only"] {
            if args.iter().any(|a| a == flag) {
                usage_exit(&format!(
                    "{flag} is only valid for a .manifold project input (D7); import-graph mode \
                     measures a fixed frame count via --frames instead"
                ));
            }
        }
        match crate::perf_soak_import::run_import(&project_path, args) {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!("perf-soak: {e}");
                std::process::exit(3);
            }
        }
    }
    // `--size` is import-mode-only (D7) — reject on a `.manifold` input
    // rather than silently ignoring it.
    if arg_value(args, "--size").is_some() {
        usage_exit(
            "--size is only valid for a .glb/.gltf input (D7); .manifold projects size from \
             project.settings.output_width/output_height",
        );
    }

    let seconds = match arg_value(args, "--seconds") {
        Some(s) => match s.parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => v,
            _ => usage_exit("--seconds must be a positive number"),
        },
        None => usage_exit("--seconds N is required"),
    };

    let start_beats = match arg_value(args, "--start") {
        Some(s) => match s.parse::<f64>() {
            Ok(v) if v.is_finite() => Some(v),
            _ => usage_exit("--start must be a finite number of beats"),
        },
        None => None,
    };

    let update_baseline = args.iter().any(|a| a == "--update-baseline");
    let profile_mode = args.iter().any(|a| a == "--profile");
    let report_only = args.iter().any(|a| a == "--report-only");

    if has_conflicting_flags(profile_mode, update_baseline, report_only) {
        usage_exit("--profile, --report-only, and --update-baseline are mutually exclusive");
    }

    let result = if profile_mode {
        run_profile(&project_path, seconds, start_beats)
    } else {
        run_soak(
            &project_path,
            seconds,
            start_beats,
            update_baseline,
            report_only,
        )
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
         [--start <beats>] [--update-baseline] [--report-only] [--profile]"
    );
    eprintln!(
        "   or: cargo xtask perf-soak <file.glb|.gltf> [--size WxH] [--frames N] [--profile] \
         (D7 import-graph mode, report-only)"
    );
    std::process::exit(2);
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn has_conflicting_flags(profile_mode: bool, update_baseline: bool, report_only: bool) -> bool {
    (profile_mode && update_baseline) || (report_only && (profile_mode || update_baseline))
}

struct PreparedProject {
    ct: ContentThread,
    cmd_tx: crossbeam_channel::Sender<ContentCommand>,
    cmd_rx: crossbeam_channel::Receiver<ContentCommand>,
    state_tx: crossbeam_channel::Sender<ContentState>,
    drain: std::thread::JoinHandle<()>,
    project_path: PathBuf,
    width: u32,
    height: u32,
    frame_rate: f64,
    bpm: manifold_core::Bpm,
    startup: serde_json::Value,
}

/// Load a project into the same empty headless context used by the production
/// content thread, then run the shared production install/warmup path. Both
/// normal and diagnostic runs use this helper so startup telemetry describes
/// the same lifecycle in both modes.
fn prepare_project(project_path_str: &str, mode: &str) -> Result<PreparedProject, String> {
    let startup_started = Instant::now();
    let project_path = Path::new(project_path_str).to_path_buf();

    let parse_started = Instant::now();
    let project = manifold_io::loader::load_project_with(
        &project_path,
        crate::project_io::install_embedded_presets,
    )
    .map_err(|e| format!("failed to load project '{}': {e}", project_path.display()))?;
    let parse_ms = parse_started.elapsed().as_secs_f64() * 1000.0;

    let width = project.settings.output_width.max(1) as u32;
    let height = project.settings.output_height.max(1) as u32;
    let frame_rate = project.settings.frame_rate as f64;
    let bpm = project.settings.bpm;

    // Set up the progress channel before entering the shared warmup. The
    // drain keeps the unbounded state queue bounded during long fixture loads.
    let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();
    let drain = std::thread::Builder::new()
        .name("perf-soak-load-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .map_err(|e| format!("spawn load drain thread: {e}"))?;
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();

    let setup_started = Instant::now();
    // Deliberately construct an empty context. Loading the fixture through the
    // shared lifecycle below is what makes this path representative of the
    // app's production startup sequence.
    let mut ct = headless_content_thread(manifold_core::project::Project::default(), width, height);
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1000.0;

    let load_started = Instant::now();
    let load_report = ct.load_project_and_warmup(Box::new(project), &cmd_rx, &cmd_tx, &state_tx);
    let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
    let total_ms = startup_started.elapsed().as_secs_f64() * 1000.0;

    let startup = serde_json::json!({
        "mode": mode,
        "parse_ms": parse_ms,
        "device_context_setup_ms": setup_ms,
        "shared_load_warmup_ms": load_ms,
        "total_startup_ms": total_ms,
        "load_report": load_report,
        "headless": {
            "ticks": "content-thread ticks",
            "display_present_deadlines": false,
            "audio_hardware": false,
            "ui_surfaces": false,
        },
    });

    eprintln!(
        "perf-soak ({mode}): startup parse={parse_ms:.1}ms setup={setup_ms:.1}ms \
         load+warmup={load_ms:.1}ms total={total_ms:.1}ms"
    );

    Ok(PreparedProject {
        ct,
        cmd_tx,
        cmd_rx,
        state_tx,
        drain,
        project_path,
        width,
        height,
        frame_rate,
        bpm,
        startup,
    })
}

#[derive(Default)]
struct MemorySamples {
    count: usize,
    first: Option<u64>,
    last: Option<u64>,
    min: Option<u64>,
    max: Option<u64>,
}

impl MemorySamples {
    fn sample(&mut self, ct: &ContentThread) {
        if let Some(snapshot) = ct
            .content_pipeline
            .native_device()
            .and_then(manifold_gpu::GpuDevice::modifier_memory_snapshot)
        {
            let bytes = snapshot.current_allocated_bytes;
            self.count += 1;
            self.first.get_or_insert(bytes);
            self.last = Some(bytes);
            self.min = Some(self.min.map_or(bytes, |min| min.min(bytes)));
            self.max = Some(self.max.map_or(bytes, |max| max.max(bytes)));
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "samples": self.count,
            "first_bytes": self.first,
            "min_bytes": self.min,
            "max_bytes": self.max,
            "last_bytes": self.last,
        })
    }
}

fn cold_touch_summary() -> serde_json::Value {
    use manifold_core::cold_touch::{ColdTouchKind, cold_touch_count};
    let mut categories = BTreeMap::new();
    for (kind, label) in [
        (ColdTouchKind::PipelineCompile, "pipeline_compile"),
        (ColdTouchKind::GlbParse, "glb_parse"),
        (ColdTouchKind::HdriDecode, "hdri_decode"),
        (ColdTouchKind::ModelLoad, "model_load"),
        (ColdTouchKind::ChainConstruction, "chain_construction"),
    ] {
        categories.insert(label, cold_touch_count(kind));
    }
    serde_json::json!({
        "categories": categories,
        "total": manifold_core::cold_touch::total_cold_touches(),
    })
}

/// Returns `Ok(true)` if the gate passed, `Ok(false)` if it failed a
/// threshold (I1/I2) — the process still exits cleanly in both cases, only
/// the exit code differs. `Err` is a run failure (load, tick, or IO error).
fn run_soak(
    project_path_str: &str,
    seconds: f64,
    start_beats: Option<f64>,
    update_baseline: bool,
    report_only: bool,
) -> Result<bool, String> {
    let PreparedProject {
        mut ct,
        cmd_tx,
        cmd_rx,
        state_tx,
        drain,
        project_path,
        width,
        height,
        frame_rate,
        bpm,
        startup,
    } = prepare_project(project_path_str, "normal")?;

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

    eprintln!(
        "perf-soak: soaking '{}' for {seconds:.1}s at {frame_rate:.1} fps \
         ({width}x{height}, bpm={:.1}{})",
        project_path.display(),
        bpm.0,
        start_beats
            .map(|b| format!(", start={b:.1} beats"))
            .unwrap_or_default(),
    );

    // Real-time pacing, D5: the SAME `FrameTimer::wait_for_deadline` +
    // `tick_frame` pair the production `ContentThread::run()` loop calls —
    // no separate sleep/pacing logic invented here.
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut memory_samples = MemorySamples::default();
    while Instant::now() < deadline {
        if ct.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx) {
            break;
        }
        memory_samples.sample(&ct);
    }

    let session_dir = ct
        .profiler
        .as_mut()
        .expect("profiler set above")
        .stop_and_dump()
        .map_err(|e| format!("profiler dump failed: {e}"))?;

    drop(state_tx);
    drain
        .join()
        .map_err(|_| "drain thread panicked".to_string())?;

    let (stats, worst_frame_breakdown, frame_summary) = load_stats(&session_dir)?;
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

    let baseline_path = baseline_path_for(&project_path);
    let machine = current_machine();

    // Stats JSON: written every run (not flag-gated — only the BASELINE
    // write is flag-gated per I3/D4). Sits next to the profiling session
    // for a human/agent to read the acceptance-demo evidence from.
    let stats_json = serde_json::json!({
        "mode": "project",
        "measurement_version": MEASUREMENT_VERSION,
        "run_mode": "normal",
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
        "startup": startup,
        "telemetry": {
            "missed_ticks_total": frame_summary.missed_ticks_total,
            "max_missed_ticks": frame_summary.max_missed_ticks,
            "max_gpu_fence_wait_ms": frame_summary.max_gpu_fence_wait_ms,
            "timing_scope": "content-thread tick through state publication; excludes profiler capture overhead, pre-tick GPU fence wait, autorelease drain and display presentation",
            "active_clip_frames": frame_summary.active_clip_frames,
            "peak_active_clips": frame_summary.peak_active_clips,
            "frames_over_project_budget": frame_summary.frames_over_project_budget,
            "frames_over_regression_guard": frame_summary.frames_over_regression_guard,
            "regression_guard_ms": 20.0,
            "cold_touches": cold_touch_summary(),
            "metal_allocated_bytes": memory_samples.json(),
        },
        "profiling_session_dir": session_dir.display().to_string(),
    });
    let stats_path = session_dir.join("perf_soak_stats.json");
    std::fs::write(
        &stats_path,
        serde_json::to_string_pretty(&stats_json).unwrap(),
    )
    .map_err(|e| format!("write {}: {e}", stats_path.display()))?;
    eprintln!("perf-soak: stats written to {}", stats_path.display());

    // I1 — hard fail: any frame over 20ms (max_ms > 20 <=> some frame > 20ms).
    const HARD_FAIL_MS: f64 = 20.0;
    let hard_fail = stats.max_ms > HARD_FAIL_MS;
    if report_only {
        eprintln!("perf-soak: report-only — baseline comparison and writes skipped");
        return Ok(true);
    }
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
            "measurement_version": MEASUREMENT_VERSION,
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
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(
            &baseline_path,
            serde_json::to_string_pretty(&baseline).unwrap(),
        )
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
    let baseline_version = baseline["measurement_version"].as_u64().ok_or_else(|| {
        format!(
            "baseline at {} predates measurement version {MEASUREMENT_VERSION}; regenerate with --update-baseline",
            baseline_path.display()
        )
    })?;
    if baseline_version != MEASUREMENT_VERSION as u64 {
        return Err(format!(
            "baseline at {} has measurement version {baseline_version}, expected {MEASUREMENT_VERSION}; regenerate with --update-baseline",
            baseline_path.display()
        ));
    }
    let baseline_p95 = baseline["p95_ms"]
        .as_f64()
        .ok_or("baseline missing p95_ms")?;
    let baseline_machine = baseline["machine"].as_str().unwrap_or("unknown");
    if baseline_machine != machine {
        eprintln!(
            "perf-soak: WARNING — baseline was recorded on '{baseline_machine}', this run is on \
             '{machine}'. D4: baselines are only meaningful on the machine that recorded them; \
             this comparison is stated, not authoritative."
        );
    }

    let regression_ratio = if baseline_p95 > 0.0 {
        stats.p95_ms / baseline_p95
    } else {
        1.0
    };
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

/// Bumped when startup or pacing semantics change enough to invalidate
/// existing comparison baselines (shared production warmup + surface pacing).
const MEASUREMENT_VERSION: u32 = 3;

/// One node's accumulated attribution within a single profiled frame, keyed
/// by the scoped tag (`"{scope}:s{idx}"`) that both the CPU `StepProfile` and
/// the GPU `GpuProfiledSpan` carry — the D6 join key.
struct ProfiledNode {
    type_id: String,
    gpu_ms: f64,
    cpu_us: f64,
}

/// One profiled frame's attribution, including sampled untagged work and
/// command-buffer time outside the resolved spans. The latter is a residual,
/// not a measurement of any particular GPU operation.
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
    invalid_spans: usize,
    failed_command_buffers: usize,
    unresolved_ms: f64,
    untagged_ms: f64,
    gpu_ms_by_kind: std::collections::BTreeMap<&'static str, f64>,
    rt_updates: manifold_gpu::raytrace::RtAccelUpdate,
    rt_dispatches: u32,
    rt_history_resets: u32,
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
fn run_profile(
    project_path_str: &str,
    seconds: f64,
    start_beats: Option<f64>,
) -> Result<bool, String> {
    let PreparedProject {
        mut ct,
        cmd_tx,
        cmd_rx,
        state_tx,
        drain,
        project_path,
        width,
        height,
        frame_rate,
        bpm,
        startup,
    } = prepare_project(project_path_str, "diagnostic")?;

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
            .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>(
        ) {
            gen_renderer.set_profiling(true);
        }
    }

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

    eprintln!(
        "perf-soak --profile: profiling '{}' for {seconds:.1}s at {frame_rate:.1} fps \
         ({width}x{height}, bpm={:.1}{}) — forced composite_serial (D6)",
        project_path.display(),
        bpm.0,
        start_beats
            .map(|b| format!(", start={b:.1} beats"))
            .unwrap_or_default(),
    );

    let mut frames: Vec<ProfiledFrame> = Vec::new();
    let mut memory_samples = MemorySamples::default();
    let mut frame_idx: u64 = 0;
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        if ct.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx) {
            break;
        }
        memory_samples.sample(&ct);

        let gpu_profiles = ct.content_pipeline.take_gpu_profiles();
        let mut cpu_profiles = ct.content_pipeline.take_step_profiles();
        // GeneratorRenderer lives on PlaybackEngine::renderers, not on
        // ContentPipeline — drained directly here (see
        // ContentPipeline::take_step_profiles's doc).
        for renderer in ct.engine.renderers_mut() {
            if let Some(gen_renderer) = renderer
                .as_any_mut()
                .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>(
            ) {
                cpu_profiles.extend(gen_renderer.take_step_profiles());
            }
        }

        let cpu_by_tag: std::collections::HashMap<
            &str,
            &manifold_renderer::node_graph::StepProfile,
        > = cpu_profiles.iter().map(|p| (p.tag.as_str(), p)).collect();

        let (rt_updates, rt_dispatches, rt_history_resets) = ct.content_pipeline.frame_rt_observation();
        let mut frame = ProfiledFrame {
            index: frame_idx,
            total_gpu_ms: 0.0,
            overflow: 0,
            spans_used: 0,
            invalid_spans: 0,
            failed_command_buffers: 0,
            unresolved_ms: 0.0,
            untagged_ms: 0.0,
            gpu_ms_by_kind: std::collections::BTreeMap::new(),
            rt_updates,
            rt_dispatches,
            rt_history_resets,
            nodes: std::collections::HashMap::new(),
        };
        for (_cb_label, profile) in &gpu_profiles {
            frame.total_gpu_ms += profile.total_ms;
            frame.overflow += profile.overflow;
            frame.spans_used += profile.spans.len();
            frame.invalid_spans += profile.invalid;
            frame.failed_command_buffers += profile.failed_command_buffers;
            // Keep the signed residual: overlapping spans or timestamp
            // calibration can over-attribute time. Do not disguise that as zero.
            frame.unresolved_ms += profile.total_ms - profile.attributed_ms();
            for span in &profile.spans {
                *frame.gpu_ms_by_kind.entry(span.kind.as_str()).or_default() += span.millis;
                // A span whose tag matches no live executor step this frame
                // (empty scope, or a compositor-owned pass — blend/tonemap/
                // LED slicer — with no `Executor` behind it at all) is
                // reported explicitly, never silently dropped (D6).
                match cpu_by_tag.get(span.tag.as_str()) {
                    Some(cpu) => {
                        let entry =
                            frame
                                .nodes
                                .entry(span.tag.clone())
                                .or_insert_with(|| ProfiledNode {
                                    type_id: cpu.type_id.clone(),
                                    gpu_ms: 0.0,
                                    cpu_us: cpu.cpu_nanos as f64 / 1000.0,
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
    drain
        .join()
        .map_err(|_| "drain thread panicked".to_string())?;

    if frames.is_empty() {
        return Err("no frames recorded — soak duration too short?".to_string());
    }

    let session_dir = ct
        .profiler
        .as_mut()
        .expect("diagnostic profiler set above")
        .stop_and_dump()
        .map_err(|e| format!("profiler dump failed: {e}"))?;
    let (diagnostic_stats, _diagnostic_worst, diagnostic_summary) = load_stats(&session_dir)?;

    // D6 capacity check: report, never silently truncate. `max_spans` is the
    // sampler's capacity in spans; a frame using >= it means dispatches were
    // dropped (already visible per-frame as `overflow`, surfaced here as one
    // whole-run verdict too).
    let sampler_capacity_spans = ct
        .content_pipeline
        .profiling_sampler_capacity()
        .unwrap_or(0);
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
    ranked.sort_by(|a, b| {
        b.total_gpu_ms
            .partial_cmp(&a.total_gpu_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let worst: Vec<serde_json::Value> = ranked
        .iter()
        .take(PROFILE_WORST_FRAMES_K)
        .map(|f| {
            let mut node_rows: Vec<(&String, &ProfiledNode)> = f.nodes.iter().collect();
            node_rows.sort_by(|a, b| {
                b.1.gpu_ms
                    .partial_cmp(&a.1.gpu_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let share_denom = if f.total_gpu_ms > 0.0 {
                f.total_gpu_ms
            } else {
                1.0
            };
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
                "invalid_spans": f.invalid_spans,
                "failed_command_buffers": f.failed_command_buffers,
                "unresolved_gpu_ms": f.unresolved_ms,
                "unresolved_share_of_frame": f.unresolved_ms / share_denom,
                "gpu_ms_by_kind": f.gpu_ms_by_kind,
                "rt_updates": {
                    "blas_builds": f.rt_updates.blas_builds,
                    "blas_refits": f.rt_updates.blas_refits,
                    "tlas_builds": f.rt_updates.tlas_builds,
                    "tlas_refits": f.rt_updates.tlas_refits,
                    "emissive_refreshes": f.rt_updates.emissive_refreshes,
                    "dispatches": f.rt_dispatches,
                    "history_resets": f.rt_history_resets,
                },
                "nodes": nodes_json,
            })
        })
        .collect();

    let profile_json = serde_json::json!({
        "mode": "project",
        "measurement_version": MEASUREMENT_VERSION,
        "run_mode": "diagnostic",
        "profile": true,
        "project": project_path.display().to_string(),
        "seconds": seconds,
        "start_beats": start_beats,
        "forced_composite_serial": true,
        "attribution_note": "Node rows contain resolved sampled spans. unresolved_gpu_ms is the signed command-buffer total minus all resolved spans, including untagged spans; it can include uninstrumented work and timing gaps and does not identify their cause. Negative values indicate over-attribution. Span durations use frame-calibrated timestamps.",
        "frames_measured": frames.len(),
        "frame_stats": {
            "min_ms": diagnostic_stats.min_ms,
            "p50_ms": diagnostic_stats.p50_ms,
            "p95_ms": diagnostic_stats.p95_ms,
            "max_ms": diagnostic_stats.max_ms,
        },
        "sampler_capacity_spans": sampler_capacity_spans,
        "max_frame_spans_used": max_frame_spans_used,
        "capacity_overflow": any_overflow,
        "startup": startup,
        "telemetry": {
            "missed_ticks_total": diagnostic_summary.missed_ticks_total,
            "max_missed_ticks": diagnostic_summary.max_missed_ticks,
            "max_gpu_fence_wait_ms": diagnostic_summary.max_gpu_fence_wait_ms,
            "active_clip_frames": diagnostic_summary.active_clip_frames,
            "peak_active_clips": diagnostic_summary.peak_active_clips,
            "frames_over_project_budget": diagnostic_summary.frames_over_project_budget,
            "frames_over_regression_guard": diagnostic_summary.frames_over_regression_guard,
            "regression_guard_ms": 20.0,
            "cold_touches": cold_touch_summary(),
            "metal_allocated_bytes": memory_samples.json(),
        },
        "profiling_session_dir": session_dir.display().to_string(),
        "worst_frames": worst,
    });

    let out_dir = Path::new("target/perf-profile");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let stem = project_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let out_path = out_dir.join(format!("{stem}-profile.json"));
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&profile_json).unwrap(),
    )
    .map_err(|e| format!("write {}: {e}", out_path.display()))?;
    eprintln!(
        "perf-soak --profile: attribution JSON written to {}",
        out_path.display()
    );
    if let Some(worst) = ranked.first() {
        eprintln!(
            "perf-soak --profile: worst frame #{} = {:.3}ms GPU ({} nodes + untagged {:.3}ms; unresolved {:.3}ms, {} invalid spans, {} failed command buffers)",
            worst.index,
            worst.total_gpu_ms,
            worst.nodes.len(),
            worst.untagged_ms,
            worst.unresolved_ms,
            worst.invalid_spans,
            worst.failed_command_buffers
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

struct FrameSummary {
    missed_ticks_total: u64,
    max_missed_ticks: u64,
    max_gpu_fence_wait_ms: f64,
    active_clip_frames: usize,
    peak_active_clips: usize,
    frames_over_project_budget: usize,
    frames_over_regression_guard: usize,
}

fn summarize_frames(frames: &[FrameRecord]) -> FrameSummary {
    FrameSummary {
        missed_ticks_total: frames.iter().map(|f| f.missed_frames).sum(),
        max_gpu_fence_wait_ms: frames
            .iter()
            .map(|f| f.content_thread.gpu_poll_ms)
            .fold(0.0, f64::max),
        max_missed_ticks: frames.iter().map(|f| f.missed_frames).max().unwrap_or(0),
        active_clip_frames: frames.iter().filter(|f| !f.active_clips.is_empty()).count(),
        peak_active_clips: frames
            .iter()
            .map(|f| f.active_clips.len())
            .max()
            .unwrap_or(0),
        frames_over_project_budget: frames.iter().filter(|f| f.budget_exceeded).count(),
        frames_over_regression_guard: frames.iter().filter(|f| f.wall_time_ms > 20.0).count(),
    }
}

/// Read `summary.json` (for `max_ms`/`p95_ms`, already computed) and
/// `frames.jsonl` (for `min_ms`/`p50_ms`, missing from `SessionSummary`, and
/// the worst frame's own per-section breakdown — `worst_frame.index` only
/// names which frame; the section ms live in that frame's own `FrameRecord`).
fn load_stats(session_dir: &Path) -> Result<(Stats, Option<FrameRecord>, FrameSummary), String> {
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
    let frame_summary = summarize_frames(&frames);

    let worst_frame = summary
        .worst_frame
        .as_ref()
        .and_then(|w| frames.iter().find(|f| f.index == w.index).cloned());

    Ok((
        Stats {
            frame_count: frames.len(),
            min_ms,
            p50_ms,
            p95_ms: summary.p95_frame_ms,
            max_ms: summary.max_frame_ms,
        },
        worst_frame,
        frame_summary,
    ))
}

/// `docs/perf-baselines/<project-stem>.json` (D4) — machine-tagged, one file
/// per project fixture, checked in deliberately via `--update-baseline`.
fn baseline_path_for(project_path: &Path) -> PathBuf {
    let stem = project_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let sanitized: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
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

#[cfg(test)]
mod tests {
    use super::{FrameRecord, has_conflicting_flags, summarize_frames};

    #[test]
    fn summary_keeps_gpu_wait_separate_from_tick_work() {
        let frame = FrameRecord {
            index: 0,
            beat: 56.0,
            bar: 15,
            wall_time_ms: 5.0,
            budget_exceeded: false,
            content_thread: manifold_profiler::ContentTimings {
                gpu_poll_ms: 150.0,
                ..Default::default()
            },
            gpu_passes: vec![],
            active_clips: vec![],
            active_effects: vec![],
            active_layer_count: 0,
            gpu_pass_count: 0,
            gpu_total_ms: 0.0,
            layer_states: vec![],
            missed_frames: 3,
            profiler_overhead_ms: 0.0,
            memory: Default::default(),
        };
        let mut slow_cpu = frame.clone();
        slow_cpu.wall_time_ms = 50.0;
        slow_cpu.budget_exceeded = true;
        slow_cpu.missed_frames = 1;
        slow_cpu.content_thread.gpu_poll_ms = 0.0;
        let summary = summarize_frames(&[frame, slow_cpu]);
        assert_eq!(summary.missed_ticks_total, 4);
        assert_eq!(summary.max_gpu_fence_wait_ms, 150.0);
        assert_eq!(summary.frames_over_project_budget, 1);
        assert_eq!(summary.frames_over_regression_guard, 1);
    }

    #[test]
    fn report_only_conflicts_with_baseline_or_profile() {
        assert!(!has_conflicting_flags(false, false, false));
        assert!(!has_conflicting_flags(false, false, true));
        assert!(has_conflicting_flags(true, true, false));
        assert!(has_conflicting_flags(true, false, true));
        assert!(has_conflicting_flags(false, true, true));
    }
}
