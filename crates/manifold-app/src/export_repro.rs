//! Feature-gated reproduction of the production project export path. It opens
//! the project and starts the export with the app's own command sequences and
//! drives the production `ContentThread::run` loop, headless.
//!
//! Exit codes: 0 success, 2 usage, 3 export failed, 4 stalled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;

use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

/// States seen after the last warmup update before the export is requested.
const SETTLED_STATES: u32 = 120;
/// Time in the export phase without progress before the run is declared stalled.
const STALL_LIMIT: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq)]
struct ExportReproArgs {
    project: PathBuf,
    output: PathBuf,
    start: Option<f64>,
    end: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f32>,
}

fn parse_args(args: &[String]) -> Result<ExportReproArgs, String> {
    let project = args
        .get(1)
        .filter(|s| !s.starts_with("--"))
        .map(PathBuf::from)
        .ok_or_else(|| "missing <project>".to_string())?;
    let value = |flag: &str| -> Result<String, String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .filter(|v| !v.starts_with("--"))
            .ok_or_else(|| format!("missing value for {flag}"))
    };
    let given = |flag: &str| args.iter().any(|a| a == flag);
    let output = PathBuf::from(value("--output")?);
    let parse = |flag: &str| -> Result<Option<f32>, String> {
        if !given(flag) {
            return Ok(None);
        }
        value(flag)?
            .parse()
            .map(Some)
            .map_err(|_| format!("{flag} must be a number"))
    };
    let parse_f64 = |flag: &str| -> Result<Option<f64>, String> {
        if !given(flag) {
            return Ok(None);
        }
        value(flag)?
            .parse()
            .map(Some)
            .map_err(|_| format!("{flag} must be a number"))
    };
    let parse_u32 = |flag: &str| -> Result<Option<u32>, String> {
        if !given(flag) {
            return Ok(None);
        }
        value(flag)?
            .parse()
            .map(Some)
            .map_err(|_| format!("{flag} must be an integer"))
    };
    let result = ExportReproArgs {
        project,
        output,
        start: parse_f64("--start")?,
        end: parse_f64("--end")?,
        width: parse_u32("--width")?,
        height: parse_u32("--height")?,
        fps: parse("--fps")?,
    };
    if result.start.is_some_and(|v| !v.is_finite()) || result.end.is_some_and(|v| !v.is_finite()) {
        return Err("--start and --end must be finite beat values".into());
    }
    if result.fps.is_some_and(|v| !v.is_finite()) {
        return Err("--fps must be finite".into());
    }
    if let (Some(start), Some(end)) = (result.start, result.end)
        && start >= end
    {
        return Err("--start must be less than --end".into());
    }
    if result.width == Some(0) || result.height == Some(0) || result.fps.is_some_and(|v| v <= 0.0) {
        return Err("width, height, and fps must be positive".into());
    }
    Ok(result)
}

pub fn run(args: &[String]) -> ! {
    // This subcommand exits before the GUI logger is initialized. Keep the
    // same diagnostics as the other headless commands, especially on failure.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    let parsed = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "export-repro: {e}\nusage: export-repro <project> --output <path> [--start <beat>] [--end <beat>] [--width N] [--height N] [--fps N]"
            );
            std::process::exit(2);
        }
    };
    match run_through_app_loop(&parsed) {
        Ok(()) => {
            eprintln!("export-repro: success: {}", parsed.output.display());
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("export-repro: failed: {e}");
            std::process::exit(3);
        }
    }
}

/// Export progress shared between the drain thread and the watchdog.
struct Progress {
    start: Instant,
    /// Milliseconds since `start` when the export phase began or last advanced.
    last_progress_ms: AtomicU64,
    export_phase: AtomicBool,
    saw_exporting: AtomicBool,
    last_pct_bits: AtomicU32,
    done: AtomicBool,
}

impl Progress {
    fn mark(&self) {
        self.last_progress_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

fn run_through_app_loop(args: &ExportReproArgs) -> Result<(), String> {
    let project = manifold_io::loader::load_project_with(
        Path::new(&args.project),
        crate::project_io::install_embedded_presets,
    )
    .map_err(|e| format!("failed to load project '{}': {e}", args.project.display()))?;
    let mut config = crate::app_lifecycle::export_config_for(
        &project,
        args.output.to_string_lossy().into_owned(),
    );
    if let Some(v) = args.start {
        config.start_beat = v;
    }
    if let Some(v) = args.end {
        config.end_beat = v;
    }
    if let Some(v) = args.width {
        config.width = v;
    }
    if let Some(v) = args.height {
        config.height = v;
    }
    if let Some(v) = args.fps {
        config.fps = v;
    }
    let ct = headless_content_thread(
        manifold_core::project::Project::default(),
        config.width,
        config.height,
    );
    let (cmd_tx, cmd_rx) = unbounded::<ContentCommand>();
    let (state_tx, state_rx) = crate::content_state::state_channel();
    crate::app_lifecycle::send_open_project(project, |cmd| {
        cmd_tx.send(cmd).expect("content command channel open")
    });

    let progress = Arc::new(Progress {
        start: Instant::now(),
        last_progress_ms: AtomicU64::new(0),
        export_phase: AtomicBool::new(false),
        saw_exporting: AtomicBool::new(false),
        last_pct_bits: AtomicU32::new(0f32.to_bits()),
        done: AtomicBool::new(false),
    });

    let drain_tx = cmd_tx.clone();
    let drain_progress = Arc::clone(&progress);
    let drain = std::thread::Builder::new()
        .name("export-repro-drain".into())
        .spawn(move || {
            let mut config = Some(config);
            let mut settled = 0u32;
            let mut finished = None;
            while let Ok(state) = state_rx.recv() {
                if !drain_progress.export_phase.load(Ordering::Relaxed) {
                    if state.warmup.is_some() {
                        settled = 0;
                    } else {
                        settled += 1;
                    }
                    if settled >= SETTLED_STATES
                        && let Some(config) = config.take()
                    {
                        drain_tx
                            .send(ContentCommand::StartExport(Box::new(config)))
                            .expect("content command channel open");
                        eprintln!("export-repro: export requested");
                        drain_progress.mark();
                        drain_progress.export_phase.store(true, Ordering::Relaxed);
                    }
                    continue;
                }
                if let Some(event) = state.export_finished.into_iter().next() {
                    drain_progress.mark();
                    finished = Some(event);
                    let _ = drain_tx.send(ContentCommand::Shutdown);
                    break;
                }
                if state.is_exporting {
                    drain_progress.saw_exporting.store(true, Ordering::Relaxed);
                    let last = f32::from_bits(drain_progress.last_pct_bits.load(Ordering::Relaxed));
                    if state.export_progress != last {
                        drain_progress
                            .last_pct_bits
                            .store(state.export_progress.to_bits(), Ordering::Relaxed);
                        drain_progress.mark();
                    }
                }
            }
            drain_progress.done.store(true, Ordering::Relaxed);
            finished
        })
        .map_err(|e| format!("failed to start export status drain: {e}"))?;

    let watch = Arc::clone(&progress);
    std::thread::Builder::new()
        .name("export-repro-watchdog".into())
        .spawn(move || {
            while !watch.done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if !watch.export_phase.load(Ordering::Relaxed) {
                    continue;
                }
                let idle_ms = (watch.start.elapsed().as_millis() as u64)
                    .saturating_sub(watch.last_progress_ms.load(Ordering::Relaxed));
                if idle_ms >= STALL_LIMIT.as_millis() as u64 && !watch.done.load(Ordering::Relaxed)
                {
                    if watch.saw_exporting.load(Ordering::Relaxed) {
                        let pct = f32::from_bits(watch.last_pct_bits.load(Ordering::Relaxed));
                        eprintln!(
                            "export-repro: stalled: no progress since {}%",
                            (pct * 100.0).round()
                        );
                    } else {
                        eprintln!("export-repro: stalled: export never started");
                    }
                    std::process::exit(4);
                }
            }
        })
        .map_err(|e| format!("failed to start watchdog: {e}"))?;

    ct.run(cmd_tx, cmd_rx, state_tx);
    match drain
        .join()
        .map_err(|_| "export status drain panicked".to_string())?
    {
        Some(event) if event.success => Ok(()),
        Some(event) => Err(event.message),
        None => Err("export produced no completion status".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_reproduction_config() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--output",
            "out.mp4",
            "--start",
            "56",
            "--end",
            "128",
            "--width",
            "1080",
            "--height",
            "1920",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert_eq!(
            parse_args(&args).unwrap(),
            ExportReproArgs {
                project: "scene.manifold".into(),
                output: "out.mp4".into(),
                start: Some(56.0),
                end: Some(128.0),
                width: Some(1080),
                height: Some(1920),
                fps: Some(60.0)
            }
        );
    }

    #[test]
    fn output_alone_leaves_every_override_unset() {
        let args = ["export-repro", "scene.manifold", "--output", "out.mp4"].map(String::from);
        assert_eq!(
            parse_args(&args).unwrap(),
            ExportReproArgs {
                project: "scene.manifold".into(),
                output: "out.mp4".into(),
                start: None,
                end: None,
                width: None,
                height: None,
                fps: None
            }
        );
    }

    #[test]
    fn rejects_reversed_range() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--output",
            "out.mp4",
            "--start",
            "128",
            "--end",
            "56",
            "--width",
            "1",
            "--height",
            "1",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn requires_output() {
        let args = [
            "export-repro",
            "scene.manifold",
            "--start",
            "56",
            "--end",
            "128",
            "--width",
            "1",
            "--height",
            "1",
            "--fps",
            "60",
        ]
        .map(String::from);
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn rejects_non_finite_numeric_flags() {
        let base = vec![
            "export-repro",
            "scene.manifold",
            "--output",
            "out.mp4",
            "--start",
            "1",
            "--end",
            "2",
            "--width",
            "1",
            "--height",
            "1",
            "--fps",
            "60",
        ];
        for (flag, values) in [
            ("--start", ["NaN", "inf", "-inf"]),
            ("--end", ["NaN", "inf", "-inf"]),
            ("--fps", ["NaN", "inf", "-inf"]),
        ] {
            for value in values {
                let mut args = base.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                let index = args.iter().position(|s| s == flag).unwrap() + 1;
                args[index] = value.to_string();
                assert!(parse_args(&args).is_err(), "{flag}={value} should fail");
            }
        }
    }
}
