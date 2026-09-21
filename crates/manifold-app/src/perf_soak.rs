//! `cargo xtask perf-soak <project> --seconds N [--start <beats>]
//! [--update-baseline]` — PERF_BUDGET_GATE_DESIGN.md P1.
//!
//! Uses shared production loading and paced content ticks. Content work,
//! actual tick intervals and GPU surface waits are separate measurements.
//! Display presentation and audio hardware are not measured headlessly.
//!
//! Exit codes: 0 = scoped criteria passed, or report-only capture completed;
//! 1 = criteria failed; 2 = usage error; 3 = capture/comparison failure.
//! Every completed report carries a separate evaluation status. Report-only,
//! diagnostic and import modes are not_evaluated, never smoothness passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use manifold_profiler::FrameRecord;
use sha2::{Digest, Sha256};

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

/// The soak contract is intentionally independent of presentation timing.
/// A normal run judges content work and measured tick pacing; report-only and
/// diagnostic runs record evidence without claiming smooth playback.
const MEASUREMENT_VERSION: u32 = 4;
const HARD_FAIL_MS: f64 = 20.0;
const REGRESSION_BAND: f64 = 1.15;
const DEADLINE_TOLERANCE_MS: f64 = manifold_profiler::DEADLINE_TOLERANCE_MS;

#[derive(Debug, Clone, PartialEq)]
struct ProjectFingerprint {
    canonical_path: String,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
struct BaselineIdentity {
    measurement_version: u32,
    machine: String,
    gpu: String,
    project_path: String,
    project_sha256: String,
    width: u32,
    height: u32,
    fps: f64,
    duration_seconds: f64,
    start_beat: f64,
    run_mode: &'static str,
    build_profile: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Evaluation {
    status: &'static str,
    reasons: Vec<String>,
    scope: &'static str,
}

impl Evaluation {
    fn passed(reasons: Vec<String>) -> Self {
        Self {
            status: "passed",
            reasons,
            scope: "headless_content_timing",
        }
    }

    fn failed(reasons: Vec<String>) -> Self {
        Self {
            status: "failed",
            reasons,
            scope: "headless_content_timing",
        }
    }

    pub(super) fn not_evaluated(reason: impl Into<String>, scope: &'static str) -> Self {
        Self {
            status: "not_evaluated",
            reasons: vec![reason.into()],
            scope,
        }
    }

    fn error(reason: impl Into<String>) -> Self {
        Self {
            status: "error",
            reasons: vec![reason.into()],
            scope: "headless_content_timing",
        }
    }

    pub(super) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "reasons": self.reasons,
            "scope": self.scope,
            "presentation": "not_measured",
        })
    }
}

fn fingerprint_project(path: &Path) -> Result<ProjectFingerprint, String> {
    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("canonicalize project '{}': {e}", path.display()))?;
    let bytes = std::fs::read(&canonical_path)
        .map_err(|e| format!("read project '{}': {e}", canonical_path.display()))?;
    let digest = Sha256::digest(bytes);
    Ok(ProjectFingerprint {
        canonical_path: canonical_path.display().to_string(),
        sha256: format!("{digest:x}"),
    })
}

fn baseline_identity_json(identity: &BaselineIdentity) -> serde_json::Value {
    serde_json::json!({
        "measurement_version": identity.measurement_version,
        "machine": identity.machine,
        "gpu": identity.gpu,
        "project_path": identity.project_path,
        "project_sha256": identity.project_sha256,
        "resolution": [identity.width, identity.height],
        "fps": identity.fps,
        "duration_seconds": identity.duration_seconds,
        "start_beat": identity.start_beat,
        "run_mode": identity.run_mode,
        "build_profile": identity.build_profile,
    })
}

fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn baseline_identity_errors(
    baseline: &serde_json::Value,
    expected: &BaselineIdentity,
) -> Vec<String> {
    let mut errors = Vec::new();
    if baseline["measurement_version"].as_u64() != Some(expected.measurement_version as u64) {
        errors.push(format!(
            "measurement version mismatch (expected {}, baseline {:?})",
            expected.measurement_version, baseline["measurement_version"]
        ));
    }
    if baseline["machine"].as_str() != Some(expected.machine.as_str()) {
        errors.push("machine mismatch".to_string());
    }
    if baseline["gpu"].as_str() != Some(expected.gpu.as_str()) {
        errors.push("GPU mismatch".to_string());
    }
    let baseline_path = baseline["project_path"].as_str();
    if baseline_path != Some(expected.project_path.as_str()) {
        errors.push("canonical project path mismatch".to_string());
    }
    if baseline["project_sha256"].as_str() != Some(expected.project_sha256.as_str()) {
        errors.push("project SHA256 mismatch".to_string());
    }
    let resolution = baseline["resolution"].as_array();
    let resolution_matches = resolution
        .and_then(|r| Some((r.first()?.as_u64()?, r.get(1)?.as_u64()?)))
        .map(|(w, h)| w == expected.width as u64 && h == expected.height as u64)
        .unwrap_or(false);
    if !resolution_matches {
        errors.push("resolution mismatch".to_string());
    }
    if json_f64(baseline, "fps") != Some(expected.fps) {
        errors.push("frame-rate mismatch".to_string());
    }
    if json_f64(baseline, "duration_seconds") != Some(expected.duration_seconds) {
        errors.push("duration mismatch".to_string());
    }
    if json_f64(baseline, "start_beat") != Some(expected.start_beat) {
        errors.push("start beat mismatch".to_string());
    }
    if baseline["run_mode"].as_str() != Some(expected.run_mode) {
        errors.push("run mode mismatch".to_string());
    }
    if baseline["build_profile"].as_str() != Some(expected.build_profile) {
        errors.push("build profile mismatch".to_string());
    }
    errors
}

fn baseline_interval_p95(baseline: &serde_json::Value) -> Result<f64, String> {
    baseline["p95_interval_ms"]
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| "baseline p95 tick interval is missing, nonfinite, or nonpositive".into())
}

fn identity_availability_errors(identity: &BaselineIdentity) -> Vec<String> {
    let mut errors = Vec::new();
    if identity.machine == "unknown-machine" || identity.machine.is_empty() {
        errors.push("machine identity is unavailable".to_string());
    }
    if identity.gpu == "unknown" || identity.gpu.is_empty() {
        errors.push("GPU identity is unavailable".to_string());
    }
    errors
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn startup_errors(startup: &serde_json::Value) -> Vec<String> {
    let mut errors = Vec::new();
    let report = &startup["load_report"];
    if report["installError"].as_str().is_some() {
        errors.push("project installation reported an error".to_string());
    }
    let warmup = &report["warmup"];
    if warmup["completed"].as_bool() != Some(true) {
        errors.push("project warmup did not complete".to_string());
    }
    for (field, label) in [
        ("installFailed", "project warmup installation failed"),
        ("interrupted", "project warmup was interrupted"),
        ("budgetExhausted", "project warmup exhausted its budget"),
    ] {
        if warmup[field].as_bool() == Some(true) {
            errors.push(label.to_string());
        }
    }
    if warmup["pendingWorkers"].as_u64().unwrap_or(0) > 0 {
        errors.push("project warmup has pending workers".to_string());
    }
    errors
}

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
        "timing_scope": "shared project parse through GPU warmup; excludes fingerprint preflight and process startup",
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
    // Freeze the project fingerprint before loading or running it. Baselines
    // must describe the measured input, even if the file changes afterwards.
    let fingerprint_started = Instant::now();
    let fingerprint = fingerprint_project(Path::new(project_path_str))?;
    let fingerprint_preflight_ms = fingerprint_started.elapsed().as_secs_f64() * 1000.0;
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

    if let Some(beats) = start_beats {
        ct.handle_command(ContentCommand::SeekToBeat(manifold_core::Beats(beats)));
    }
    let measured_start_beat = ct.engine.current_beat_f64();

    let gpu_name = ct
        .content_pipeline
        .native_device()
        .map(|d| d.device_name())
        .unwrap_or_else(|| "unknown".to_string());
    let machine = current_machine();

    ct.profiler = Some(manifold_profiler::ProfileSession::new(
        project_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string()),
        project_path.display().to_string(),
        (width, height),
        frame_rate as f32,
        gpu_name.clone(),
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
    // Profiling setup is outside the observed playback window. In the app,
    // paused content ticks already maintain this clock while the UI prepares.
    ct.timer.resume_after_load();
    ct.handle_command(ContentCommand::Play);
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
    let startup_errors = startup_errors(&startup);
    eprintln!(
        "perf-soak: {} content-work samples — min={:.2}ms p50={:.2}ms p95={:.2}ms max={:.2}ms",
        stats.frame_count, stats.min_ms, stats.p50_ms, stats.p95_ms, stats.max_ms
    );
    if let Some(ref w) = worst_frame_breakdown {
        eprintln!(
            "perf-soak: worst content-work sample #{} @ beat {:.2} bar {} = {:.2}ms \
             (midi={:.2} sync={:.2} engine={:.2} render={:.2} separate_gpu_surface_wait={:.2} cleanup={:.2})",
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
    let gpu = gpu_name;
    let identity = BaselineIdentity {
        measurement_version: MEASUREMENT_VERSION,
        machine: machine.clone(),
        gpu: gpu.clone(),
        project_path: fingerprint.canonical_path.clone(),
        project_sha256: fingerprint.sha256.clone(),
        width,
        height,
        fps: frame_rate,
        duration_seconds: seconds,
        start_beat: measured_start_beat,
        run_mode: "normal",
        build_profile: build_profile(),
    };

    // Stats JSON: written every run (not flag-gated — only the BASELINE
    // write is flag-gated per I3/D4). Sits next to the profiling session
    // for a human/agent to read the acceptance-demo evidence from.
    let mut stats_json = serde_json::json!({
        "mode": "project",
        "measurement_version": MEASUREMENT_VERSION,
        "run_mode": "normal",
        "presentation": "not_measured",
        "build_profile": build_profile(),
        "fingerprint_preflight_ms": fingerprint_preflight_ms,
        "project": fingerprint.canonical_path.clone(),
        "machine": machine,
        "gpu": gpu,
        "project_path": fingerprint.canonical_path,
        "project_sha256": fingerprint.sha256,
        "resolution": [width, height],
        "fps": frame_rate,
        "duration_seconds": seconds,
        "start_beat": measured_start_beat,
        "seconds": seconds,
        "start_beats": start_beats,
        "content_work": {
            "sample_count": stats.frame_count,
            "min_ms": stats.min_ms,
            "p50_ms": stats.p50_ms,
            "p95_ms": stats.p95_ms,
            "max_ms": stats.max_ms,
        },
        "tick_interval": {
            "sample_count": stats.interval_sample_count,
            "deadline_lateness_ms": stats.deadline_lateness_ms,
            "late_intervals": stats.pacing_valid.then_some(stats.late_intervals),
            "deadline_tolerance_ms": DEADLINE_TOLERANCE_MS,
            "min_ms": stats.pacing_valid.then_some(stats.interval_min_ms),
            "p50_ms": stats.pacing_valid.then_some(stats.interval_p50_ms),
            "p95_ms": stats.pacing_valid.then_some(stats.interval_p95_ms),
            "max_ms": stats.pacing_valid.then_some(stats.interval_max_ms),
            "coverage_valid": stats.pacing_valid,
            "coverage_errors": stats.pacing_errors,
        },
        "worst_content_work": worst_frame_breakdown,
        "execution": {"completed": true},
        "evaluation": Evaluation::not_evaluated("Evaluation has not completed", "headless_content_timing").json(),
        "comparison_limits": ["Project SHA256 covers project-file bytes, not external asset bytes", "Machine load and cache state are not controlled"],
        "startup": startup,
        "startup_validation": {
            "completed": startup_errors.is_empty(),
            "errors": startup_errors.clone(),
        },
        "telemetry": {
            "whole_tick_intervals_skipped_total": frame_summary.whole_tick_intervals_skipped_total,
            "max_whole_tick_intervals_skipped": frame_summary.max_whole_tick_intervals_skipped,
            "max_gpu_fence_wait_ms": frame_summary.max_gpu_fence_wait_ms,
            "timing_scope": "content-thread tick through state publication; excludes profiler capture overhead, pre-tick GPU fence wait, autorelease drain and display presentation",
            "active_clip_frames": frame_summary.active_clip_frames,
            "peak_active_clips": frame_summary.peak_active_clips,
            "content_work_over_project_budget": frame_summary.content_work_over_project_budget,
            "content_work_over_regression_guard": frame_summary.content_work_over_regression_guard,
            "regression_guard_ms": 20.0,
            "max_gpu_pass_count": frame_summary.gpu_pass_count,
            "max_sampled_gpu_pass_time_ms": frame_summary.gpu_total_ms,
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

    let mut evaluation = if report_only {
        let identity_errors = identity_availability_errors(&identity);
        let reason = if startup_errors.is_empty() && identity_errors.is_empty() {
            "report-only run; no gate or baseline comparison".to_string()
        } else {
            format!(
                "report-only run; unavailable identity or preparation: {}",
                startup_errors
                    .iter()
                    .chain(identity_errors.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        Evaluation::not_evaluated(reason, "report_only")
    } else {
        let mut evaluation = evaluate_normal_run(&stats);
        let identity_errors = identity_availability_errors(&identity);
        if !startup_errors.is_empty() || !identity_errors.is_empty() {
            evaluation = Evaluation::failed(
                startup_errors
                    .iter()
                    .map(|error| format!("startup: {error}"))
                    .chain(
                        identity_errors
                            .iter()
                            .map(|error| format!("identity: {error}")),
                    )
                    .chain(evaluation.reasons)
                    .collect(),
            );
        }
        evaluation
    };
    if report_only {
        stats_json["execution"] = serde_json::json!({"completed": true});
        stats_json["evaluation"] = evaluation.json();
        write_evaluation_report(&stats_path, &stats_json)?;
        eprintln!("perf-soak: report-only — baseline comparison and writes skipped");
        return Ok(true);
    }

    let gate_passed = evaluation.status == "passed";
    if !gate_passed {
        eprintln!("perf-soak: FAIL — {}", evaluation.reasons.join("; "));
    }

    if update_baseline {
        if !gate_passed {
            stats_json["execution"] = serde_json::json!({"completed": true});
            stats_json["evaluation"] = evaluation.json();
            write_evaluation_report(&stats_path, &stats_json)?;
            eprintln!("perf-soak: baseline update skipped because evaluation failed");
            return Ok(false);
        }
        // D4/I3: baseline write is flag-gated — this is the ONLY place the
        // baseline file is written.
        let mut baseline = baseline_identity_json(&identity);
        baseline["p95_interval_ms"] = serde_json::json!(stats.interval_p95_ms);
        baseline["content_work_p95_ms"] = serde_json::json!(stats.p95_ms);
        baseline["recorded_at"] = serde_json::json!(iso_now());
        write_passing_baseline(&baseline_path, &baseline, &evaluation)?;
        evaluation = Evaluation::passed(vec![
            "baseline updated after passing timing evaluation".to_string(),
        ]);
        stats_json["execution"] = serde_json::json!({"completed": true});
        stats_json["evaluation"] = evaluation.json();
        write_evaluation_report(&stats_path, &stats_json)?;
        eprintln!("perf-soak: baseline written to {}", baseline_path.display());
        return Ok(true);
    }

    if !gate_passed {
        stats_json["execution"] = serde_json::json!({"completed": true});
        stats_json["evaluation"] = evaluation.json();
        write_evaluation_report(&stats_path, &stats_json)?;
        return Ok(false);
    }

    // D3 — regression fail: p95 > baseline p95 * 1.15. No baseline yet is a
    // run failure, not a silent pass (no-silent-fallbacks) — the executor
    // must create one deliberately with --update-baseline first.
    let baseline_raw = match std::fs::read_to_string(&baseline_path) {
        Ok(raw) => raw,
        Err(e) => {
            let error = Evaluation::error(format!(
                "no baseline at {} ({e}); run once with --update-baseline first",
                baseline_path.display()
            ));
            persist_evaluation_report(&stats_path, &stats_json, &error)?;
            return Err(error.reasons.join("; "));
        }
    };
    let baseline: serde_json::Value = match serde_json::from_str(&baseline_raw) {
        Ok(value) => value,
        Err(e) => {
            let error = Evaluation::error(format!("parse {}: {e}", baseline_path.display()));
            persist_evaluation_report(&stats_path, &stats_json, &error)?;
            return Err(error.reasons.join("; "));
        }
    };
    let identity_errors = baseline_identity_errors(&baseline, &identity);
    if !identity_errors.is_empty() {
        evaluation = Evaluation::error(format!(
            "baseline identity mismatch: {}",
            identity_errors.join("; ")
        ));
        stats_json["execution"] = serde_json::json!({"completed": true});
        stats_json["evaluation"] = evaluation.json();
        write_evaluation_report(&stats_path, &stats_json)?;
        return Err(evaluation.reasons.join("; "));
    }
    let baseline_p95 = match baseline_interval_p95(&baseline) {
        Ok(value) => value,
        Err(reason) => {
            let error = Evaluation::error(reason);
            persist_evaluation_report(&stats_path, &stats_json, &error)?;
            return Err(error.reasons.join("; "));
        }
    };

    let regression_ratio = stats.interval_p95_ms / baseline_p95;
    let regressed = regression_ratio > REGRESSION_BAND;
    if regressed {
        eprintln!(
            "perf-soak: FAIL (I2) — tick-interval p95 {:.2}ms is {:.1}% above baseline {:.2}ms (band: {:.0}%)",
            stats.interval_p95_ms,
            (regression_ratio - 1.0) * 100.0,
            baseline_p95,
            (REGRESSION_BAND - 1.0) * 100.0
        );
    } else {
        eprintln!(
            "perf-soak: tick-interval p95 {:.2}ms vs baseline {:.2}ms ({:+.1}%) — within the {:.0}% band",
            stats.interval_p95_ms,
            baseline_p95,
            (regression_ratio - 1.0) * 100.0,
            (REGRESSION_BAND - 1.0) * 100.0
        );
    }

    let passed = !regressed;
    evaluation = if passed {
        Evaluation::passed(Vec::new())
    } else {
        Evaluation::failed(vec![format!(
            "tick interval p95 {:.2}ms exceeds baseline {:.2}ms by more than {:.0}%",
            stats.interval_p95_ms,
            baseline_p95,
            (REGRESSION_BAND - 1.0) * 100.0
        )])
    };
    stats_json["execution"] = serde_json::json!({"completed": true});
    stats_json["evaluation"] = evaluation.json();
    write_evaluation_report(&stats_path, &stats_json)?;
    if passed {
        eprintln!(
            "perf-soak: PASS — headless content timing criteria only; presentation not measured"
        );
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
    let fingerprint_started = Instant::now();
    let fingerprint = fingerprint_project(Path::new(project_path_str))?;
    let fingerprint_preflight_ms = fingerprint_started.elapsed().as_secs_f64() * 1000.0;
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

    if let Some(beats) = start_beats {
        ct.handle_command(ContentCommand::SeekToBeat(manifold_core::Beats(beats)));
    }
    let measured_start_beat = ct.engine.current_beat_f64();

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
    let machine = current_machine();
    ct.profiler = Some(manifold_profiler::ProfileSession::new(
        project_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string()),
        project_path.display().to_string(),
        (width, height),
        frame_rate as f32,
        gpu_name.clone(),
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
    // Profiling setup is outside the observed playback window. In the app,
    // paused content ticks already maintain this clock while the UI prepares.
    ct.timer.resume_after_load();
    ct.handle_command(ContentCommand::Play);
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

        let (rt_updates, rt_dispatches, rt_history_resets) =
            ct.content_pipeline.frame_rt_observation();
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
    let startup_errors = startup_errors(&startup);

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
        "presentation": "not_measured",
        "build_profile": build_profile(),
        "fingerprint_preflight_ms": fingerprint_preflight_ms,
        "profile": true,
        "project": fingerprint.canonical_path.clone(),
        "machine": machine,
        "gpu": gpu_name,
        "project_path": fingerprint.canonical_path,
        "project_sha256": fingerprint.sha256,
        "seconds": seconds,
        "start_beats": start_beats,
        "start_beat": measured_start_beat,
        "forced_composite_serial": true,
        "attribution_note": "Node rows contain resolved sampled spans. unresolved_gpu_ms is the signed command-buffer total minus all resolved spans, including untagged spans; it can include uninstrumented work and timing gaps and does not identify their cause. Negative values indicate over-attribution. Span durations use frame-calibrated timestamps.",
        "frames_measured": frames.len(),
        "content_work": {
            "sample_count": diagnostic_stats.frame_count,
            "min_ms": diagnostic_stats.min_ms,
            "p50_ms": diagnostic_stats.p50_ms,
            "p95_ms": diagnostic_stats.p95_ms,
            "max_ms": diagnostic_stats.max_ms,
        },
        "tick_interval": {
            "sample_count": diagnostic_stats.interval_sample_count,
            "deadline_lateness_ms": diagnostic_stats.deadline_lateness_ms,
            "late_intervals": diagnostic_stats.pacing_valid.then_some(diagnostic_stats.late_intervals),
            "deadline_tolerance_ms": DEADLINE_TOLERANCE_MS,
            "min_ms": diagnostic_stats.pacing_valid.then_some(diagnostic_stats.interval_min_ms),
            "p50_ms": diagnostic_stats.pacing_valid.then_some(diagnostic_stats.interval_p50_ms),
            "p95_ms": diagnostic_stats.pacing_valid.then_some(diagnostic_stats.interval_p95_ms),
            "max_ms": diagnostic_stats.pacing_valid.then_some(diagnostic_stats.interval_max_ms),
            "coverage_valid": diagnostic_stats.pacing_valid,
            "coverage_errors": diagnostic_stats.pacing_errors,
        },
        "sampler_capacity_spans": sampler_capacity_spans,
        "max_frame_spans_used": max_frame_spans_used,
        "capacity_overflow": any_overflow,
        "startup": startup,
        "startup_validation": {
            "completed": startup_errors.is_empty(),
            "errors": startup_errors.clone(),
        },
        "telemetry": {
            "whole_tick_intervals_skipped_total": diagnostic_summary.whole_tick_intervals_skipped_total,
            "max_whole_tick_intervals_skipped": diagnostic_summary.max_whole_tick_intervals_skipped,
            "max_gpu_fence_wait_ms": diagnostic_summary.max_gpu_fence_wait_ms,
            "active_clip_frames": diagnostic_summary.active_clip_frames,
            "peak_active_clips": diagnostic_summary.peak_active_clips,
            "content_work_over_project_budget": diagnostic_summary.content_work_over_project_budget,
            "content_work_over_regression_guard": diagnostic_summary.content_work_over_regression_guard,
            "regression_guard_ms": 20.0,
            "cold_touches": cold_touch_summary(),
            "metal_allocated_bytes": memory_samples.json(),
        },
        "profiling_session_dir": session_dir.display().to_string(),
        "execution": {"completed": true},
        "evaluation": Evaluation::not_evaluated(
            if startup_errors.is_empty() {
                "diagnostic attribution is report-only and presentation timing was not measured".to_string()
            } else {
                format!(
                    "diagnostic attribution is report-only; preparation incomplete: {}",
                    startup_errors.join("; ")
                )
            },
            "diagnostic",
        )
        .json(),
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
    interval_sample_count: usize,
    deadline_lateness_ms: Option<manifold_profiler::PercentileStat>,
    interval_min_ms: f64,
    interval_p50_ms: f64,
    interval_p95_ms: f64,
    interval_max_ms: f64,
    pacing_valid: bool,
    pacing_errors: Vec<String>,
    late_intervals: u64,
    content_work_valid: bool,
    content_work_errors: Vec<String>,
}

struct FrameSummary {
    whole_tick_intervals_skipped_total: u64,
    max_whole_tick_intervals_skipped: u64,
    max_gpu_fence_wait_ms: f64,
    active_clip_frames: usize,
    peak_active_clips: usize,
    content_work_over_project_budget: usize,
    content_work_over_regression_guard: usize,
    gpu_pass_count: Option<u32>,
    gpu_total_ms: Option<f64>,
}

fn summarize_frames(frames: &[FrameRecord]) -> FrameSummary {
    let gpu_pass_count = frames
        .iter()
        .all(|f| f.gpu_pass_count.is_some())
        .then(|| frames.iter().filter_map(|f| f.gpu_pass_count).max())
        .flatten();
    let gpu_total_ms = frames
        .iter()
        .all(|f| f.gpu_total_ms.is_some_and(|v| v.is_finite() && v >= 0.0))
        .then(|| {
            frames
                .iter()
                .filter_map(|f| f.gpu_total_ms)
                .reduce(f64::max)
        })
        .flatten();
    FrameSummary {
        whole_tick_intervals_skipped_total: frames.iter().map(|f| f.missed_frames).sum(),
        max_gpu_fence_wait_ms: frames
            .iter()
            .map(|f| f.content_thread.gpu_poll_ms)
            .fold(0.0, f64::max),
        max_whole_tick_intervals_skipped: frames.iter().map(|f| f.missed_frames).max().unwrap_or(0),
        active_clip_frames: frames.iter().filter(|f| !f.active_clips.is_empty()).count(),
        peak_active_clips: frames
            .iter()
            .map(|f| f.active_clips.len())
            .max()
            .unwrap_or(0),
        content_work_over_project_budget: frames.iter().filter(|f| f.budget_exceeded).count(),
        content_work_over_regression_guard: frames
            .iter()
            .filter(|f| f.wall_time_ms > HARD_FAIL_MS)
            .count(),
        gpu_pass_count,
        gpu_total_ms,
    }
}

fn percentile_at(values: &mut [f64], pct: f64) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return 0.0;
    }
    let idx = ((values.len() as f64 * pct) as usize).min(values.len() - 1);
    values[idx]
}

fn pacing_statistics(frames: &[FrameRecord]) -> (Vec<f64>, bool, Vec<String>, u64) {
    let mut intervals = Vec::new();
    let mut errors = Vec::new();
    let mut late_intervals = 0;
    if frames.len() < 2 {
        errors.push("at least two content ticks are required".to_string());
    }
    for (index, frame) in frames.iter().enumerate() {
        match frame.pacing.as_ref() {
            None if index == 0 => {}
            None => errors.push(format!("frame {index} is missing pacing telemetry")),
            Some(pacing) => {
                if !pacing.is_valid() {
                    errors.push(format!("frame {index} has invalid pacing interval"));
                } else {
                    intervals.push(pacing.interval_ms);
                }
                if pacing.is_valid()
                    && (pacing.deadline_lateness_ms > DEADLINE_TOLERANCE_MS
                        || pacing.interval_ms > pacing.target_interval_ms + DEADLINE_TOLERANCE_MS)
                {
                    late_intervals += 1;
                }
            }
        }
    }
    if intervals.is_empty() {
        errors.push("no measured tick interval samples".to_string());
    }
    (intervals, errors.is_empty(), errors, late_intervals)
}

fn evaluate_normal_run(stats: &Stats) -> Evaluation {
    let mut reasons = stats.pacing_errors.clone();
    reasons.extend(stats.content_work_errors.clone());
    if !stats.content_work_valid {
        reasons.push("content work timing coverage is invalid".to_string());
    }
    if !stats.pacing_valid {
        reasons.push("tick interval coverage is incomplete or invalid".to_string());
    }
    if stats.late_intervals > 0 {
        reasons.push(format!(
            "{} tick intervals exceeded the {:.1}ms deadline tolerance",
            stats.late_intervals, DEADLINE_TOLERANCE_MS
        ));
    }
    if !stats.max_ms.is_finite() || stats.max_ms > HARD_FAIL_MS {
        reasons.push(format!(
            "content work max {:.2}ms exceeds the {:.1}ms CPU guard",
            stats.max_ms, HARD_FAIL_MS
        ));
    }
    if reasons.is_empty() {
        Evaluation::passed(Vec::new())
    } else {
        Evaluation::failed(reasons)
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
    if summary.schema_version != manifold_profiler::PROFILER_SCHEMA_VERSION {
        return Err("profiler summary uses an incompatible measurement schema".into());
    }

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
    let content_work_errors: Vec<String> = frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| !frame.wall_time_ms.is_finite() || frame.wall_time_ms < 0.0)
        .map(|(index, _)| format!("frame {index} has invalid content work timing"))
        .collect();
    wall_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min_ms = wall_times[0];
    let p50_ms = wall_times[wall_times.len() / 2];
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(session_dir.join("session.json"))
            .map_err(|e| format!("read session.json: {e}"))?,
    )
    .map_err(|e| format!("parse session.json: {e}"))?;
    let fps = metadata["target_fps"]
        .as_f64()
        .filter(|fps| fps.is_finite() && *fps > 0.0)
        .ok_or_else(|| "session frame rate is missing or invalid".to_string())?;
    let (mut intervals, _, mut pacing_errors, late_intervals) = pacing_statistics(&frames);
    if frames
        .iter()
        .filter_map(|frame| frame.pacing.as_ref())
        .any(|p| (p.target_interval_ms - 1000.0 / fps).abs() > 0.001)
    {
        pacing_errors.push("tick interval target disagrees with session frame rate".into());
    }
    let pacing_valid = pacing_errors.is_empty();
    let interval_sample_count = intervals.len();
    let interval_min_ms = intervals.iter().copied().fold(f64::INFINITY, f64::min);
    let interval_max_ms = intervals.iter().copied().fold(0.0, f64::max);
    let interval_p50_ms = percentile_at(&mut intervals.clone(), 0.50);
    let interval_p95_ms = percentile_at(&mut intervals, 0.95);
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
            p95_ms: wall_times
                [((wall_times.len() as f64 * 0.95) as usize).min(wall_times.len() - 1)],
            max_ms: wall_times[wall_times.len() - 1],
            interval_sample_count,
            deadline_lateness_ms: summary
                .pacing
                .as_ref()
                .map(|p| p.deadline_lateness_ms.clone()),
            interval_min_ms: if interval_min_ms.is_finite() {
                interval_min_ms
            } else {
                0.0
            },
            interval_p50_ms,
            interval_p95_ms,
            interval_max_ms,
            pacing_valid,
            pacing_errors,
            late_intervals,
            content_work_valid: content_work_errors.is_empty(),
            content_work_errors,
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
/// "unknown-machine" for report-only capture; evaluated baselines reject it.
fn current_machine() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
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

fn write_evaluation_report(
    stats_path: &Path,
    stats_json: &serde_json::Value,
) -> Result<(), String> {
    std::fs::write(
        stats_path,
        serde_json::to_string_pretty(stats_json).unwrap(),
    )
    .map_err(|e| format!("write {}: {e}", stats_path.display()))
}

fn write_passing_baseline(
    path: &Path,
    baseline: &serde_json::Value,
    evaluation: &Evaluation,
) -> Result<(), String> {
    if evaluation.status != "passed" {
        return Err("baseline write requires a passing evaluation".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(path, serde_json::to_string_pretty(baseline).unwrap())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn persist_evaluation_report(
    stats_path: &Path,
    stats_json: &serde_json::Value,
    evaluation: &Evaluation,
) -> Result<(), String> {
    let mut report = stats_json.clone();
    report["execution"] = serde_json::json!({"completed": true});
    report["evaluation"] = evaluation.json();
    write_evaluation_report(stats_path, &report)
}

#[cfg(test)]
mod tests {
    use super::{
        BaselineIdentity, Evaluation, FrameRecord, baseline_identity_errors,
        baseline_identity_json, baseline_interval_p95, evaluate_normal_run, has_conflicting_flags,
        pacing_statistics, summarize_frames,
    };

    fn frame(content_ms: f64, pacing: Option<manifold_profiler::FramePacing>) -> FrameRecord {
        FrameRecord {
            index: 0,
            beat: 56.0,
            bar: 15,
            wall_time_ms: content_ms,
            budget_exceeded: content_ms > 20.0,
            content_thread: manifold_profiler::ContentTimings::default(),
            pacing,
            gpu_passes: vec![],
            active_clips: vec![],
            active_effects: vec![],
            active_layer_count: 0,
            gpu_pass_count: None,
            gpu_total_ms: None,
            layer_states: vec![],
            missed_frames: 0,
            profiler_overhead_ms: 0.0,
            memory: Default::default(),
        }
    }

    fn pacing(interval_ms: f64, lateness_ms: f64) -> manifold_profiler::FramePacing {
        manifold_profiler::FramePacing {
            interval_ms,
            target_interval_ms: 1000.0 / 24.0,
            deadline_lateness_ms: lateness_ms,
        }
    }

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
            pacing: None,
            gpu_pass_count: None,
            gpu_total_ms: None,
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
        assert_eq!(summary.whole_tick_intervals_skipped_total, 4);
        assert_eq!(summary.max_gpu_fence_wait_ms, 150.0);
        assert_eq!(summary.content_work_over_project_budget, 1);
        assert_eq!(summary.content_work_over_regression_guard, 1);
    }

    #[test]
    fn report_only_conflicts_with_baseline_or_profile() {
        assert!(!has_conflicting_flags(false, false, false));
        assert!(!has_conflicting_flags(false, false, true));
        assert!(has_conflicting_flags(true, true, false));
        assert!(has_conflicting_flags(true, false, true));
        assert!(has_conflicting_flags(false, true, true));
    }

    fn saved_capture(interval_ms: Option<f64>, name: &str) -> std::path::PathBuf {
        let mut session = manifold_profiler::ProfileSession::new(
            name.into(),
            "test-project".into(),
            (1, 1),
            24.0,
            "test GPU".into(),
        );
        session.record_frame(frame(2.0, None));
        let mut second = frame(
            2.0,
            interval_ms.map(|interval| pacing(interval, (interval - 1000.0 / 24.0).max(0.0))),
        );
        second.index = 1;
        session.record_frame(second);
        session.stop_and_dump().unwrap()
    }

    #[test]
    fn saved_low_cpu_capture_with_70ms_interval_fails_deadline_criterion() {
        let dir = saved_capture(Some(70.0), "telemetry-late-gate");
        let (stats, _, _) = super::load_stats(&dir).unwrap();
        assert_eq!(stats.max_ms, 2.0);
        assert_eq!(stats.interval_p95_ms, 70.0);
        assert_eq!(stats.late_intervals, 1);
        assert_eq!(
            stats.deadline_lateness_ms.as_ref().unwrap().max_ms,
            70.0 - 1000.0 / 24.0
        );
        let evaluation = evaluate_normal_run(&stats);
        assert_eq!(evaluation.status, "failed");
        let output = dir.join("evaluation-test.json");
        super::persist_evaluation_report(&output, &serde_json::json!({}), &evaluation).unwrap();
        let reloaded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(reloaded["evaluation"]["status"], "failed");
        assert_eq!(reloaded["evaluation"]["presentation"], "not_measured");
        assert_eq!(reloaded["execution"]["completed"], true);
        let baseline_path = dir.join("baseline-test.json");
        std::fs::write(&baseline_path, b"existing baseline").unwrap();
        assert!(
            super::write_passing_baseline(&baseline_path, &serde_json::json!({}), &evaluation)
                .is_err()
        );
        assert_eq!(std::fs::read(&baseline_path).unwrap(), b"existing baseline");
    }

    #[test]
    fn saved_capture_without_interval_coverage_cannot_pass() {
        let dir = saved_capture(None, "telemetry-missing-gate");
        let (stats, _, _) = super::load_stats(&dir).unwrap();
        assert!(!stats.pacing_valid);
        assert_eq!(evaluate_normal_run(&stats).status, "failed");
    }

    #[test]
    fn report_only_evaluation_serializes_as_not_evaluated() {
        let evaluation = Evaluation::not_evaluated("report-only", "report_only");
        let dir = saved_capture(Some(1000.0 / 24.0), "telemetry-report-only");
        let output = dir.join("evaluation-test.json");
        super::persist_evaluation_report(&output, &serde_json::json!({}), &evaluation).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(report["execution"]["completed"], true);
        assert_eq!(report["evaluation"]["status"], "not_evaluated");
        assert_eq!(report["evaluation"]["presentation"], "not_measured");
    }

    #[test]
    fn wrong_frame_rate_cannot_hide_a_late_interval() {
        let dir = saved_capture(Some(70.0), "telemetry-wrong-target");
        let path = dir.join("session.json");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata["target_fps"] = serde_json::json!(10.0);
        std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let (stats, _, _) = super::load_stats(&dir).unwrap();
        assert!(!stats.pacing_valid);
    }

    #[test]
    fn unfinished_startup_is_rejected() {
        let mut startup = serde_json::json!({"load_report": {"warmup": {"completed": true}}});
        assert!(super::startup_errors(&startup).is_empty());
        for field in ["installFailed", "interrupted", "budgetExhausted"] {
            startup["load_report"]["warmup"][field] = serde_json::json!(true);
            assert!(!super::startup_errors(&startup).is_empty());
            startup["load_report"]["warmup"][field] = serde_json::json!(false);
        }
        startup["load_report"]["warmup"]["pendingWorkers"] = serde_json::json!(1);
        assert!(!super::startup_errors(&startup).is_empty());
        assert!(!super::startup_errors(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn missing_later_pacing_is_rejected() {
        let first = frame(2.0, None);
        let mut second = frame(2.0, None);
        second.index = 1;
        let (_intervals, valid, errors, _late) = pacing_statistics(&[first, second]);
        assert!(!valid);
        assert!(errors.iter().any(|e| e.contains("frame 1")));
    }

    #[test]
    fn baseline_identity_mismatches_are_explicit() {
        let identity = BaselineIdentity {
            measurement_version: 4,
            machine: "machine".into(),
            gpu: "gpu".into(),
            project_path: "/tmp/project.manifold".into(),
            project_sha256: "abc".into(),
            width: 1920,
            height: 1080,
            fps: 24.0,
            duration_seconds: 10.0,
            start_beat: 0.0,
            run_mode: "normal",
            build_profile: "debug",
        };
        assert!(baseline_identity_errors(&baseline_identity_json(&identity), &identity).is_empty());
        let mut unknown = identity.clone();
        unknown.machine = "unknown-machine".into();
        unknown.gpu = "unknown".into();
        assert_eq!(super::identity_availability_errors(&unknown).len(), 2);
        for key in [
            "measurement_version",
            "machine",
            "gpu",
            "project_path",
            "project_sha256",
            "resolution",
            "fps",
            "duration_seconds",
            "start_beat",
            "run_mode",
            "build_profile",
        ] {
            let mut baseline = baseline_identity_json(&identity);
            baseline[key] = match key {
                "measurement_version" => serde_json::json!(3),
                "resolution" => serde_json::json!([1280, 720]),
                "fps" => serde_json::json!(60.0),
                "duration_seconds" => serde_json::json!(11.0),
                "start_beat" => serde_json::json!(2.0),
                _ => serde_json::json!("different"),
            };
            assert!(
                baseline_identity_errors(&baseline, &identity)
                    .iter()
                    .any(|reason| !reason.is_empty()),
                "mismatch key {key}"
            );
        }
    }

    #[test]
    fn invalid_baseline_p95_values_are_rejected() {
        for value in [
            serde_json::json!(null),
            serde_json::json!(0.0),
            serde_json::json!(-1.0),
        ] {
            let baseline = serde_json::json!({"p95_interval_ms": value});
            assert!(baseline_interval_p95(&baseline).is_err());
        }
        let nan = serde_json::json!({"p95_interval_ms": f64::NAN});
        assert!(baseline_interval_p95(&nan).is_err());
    }
}
