#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod alerts;
mod app;
mod app_lifecycle;
mod app_render;
mod audio_mod_runtime;
mod autosave;
mod audio_waveform_cache;
mod blender_import;
mod breadcrumb;
mod clip_atlas;
mod clip_filmstrip;
mod clip_thumb_cache;
mod content_command;
mod content_commands;
mod content_export;
mod content_pipeline;
mod content_state;
mod content_thread;
mod dialog_path_memory;
#[cfg(target_os = "macos")]
mod display_link;
mod drag_hover;
mod drag_interpose;
mod editing_host;
mod editor_frame;
mod edr_surface;
mod frame_timer;
// The graph canvas + its mapping popover moved into `manifold-ui` (Phase 8 of
// `docs/UI_ARCHITECTURE_OVERHAUL.md`). Re-export under the historic `crate::`
// paths so the editor-window glue keeps resolving `crate::graph_canvas::*` and
// `crate::mapping_popover::*` unchanged.
pub(crate) use manifold_ui::graph_canvas;
pub(crate) use manifold_ui::graph_canvas::mapping_popover;
mod graph_dump;
// Shared headless `ContentThread` construction (PERF_BUDGET_GATE_DESIGN.md
// P1) — used by the `journey-proofs` test harness (only compiled in test
// mode) AND the `perf-soak` xtask binary path (a real, non-test caller), so
// it lives outside `#[cfg(test)]` itself. Gated so a plain non-test build
// with only `journey-proofs` on doesn't compile a module with zero callers
// (dead_code) — `perf-soak` always wants it; `journey-proofs` only wants it
// under `cfg(test)`, matching where its own callers (`journey_proof.rs`,
// `bug035_verify.rs`, `bug037_verify.rs`) actually live.
#[cfg(all(target_os = "macos", any(feature = "perf-soak", all(feature = "journey-proofs", test))))]
mod headless_harness;
mod input_handler;
mod input_host;
#[cfg(target_os = "macos")]
mod macos_pasteboard;
mod menu;
mod offline_audio_mod;
// P3 release-journey harness (docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md):
// headless end-to-end export proofs. Feature-gated, macOS-only (same
// constraint as `run_export` itself — native Metal, no wgpu).
#[cfg(all(feature = "journey-proofs", target_os = "macos"))]
mod journey_proof;
// BUG-035 regression guard: headless before/after MANIFOLD_RENDER_TRACE proof
// that the clip-atlas persist debounce cycle no longer spikes the content
// thread. Shares `journey_proof`'s headless ContentThread infra (same feature
// gate — no separate harness to maintain).
#[cfg(all(feature = "journey-proofs", target_os = "macos"))]
mod bug035_verify;
// BUG-037 regression guard: headless before/after MANIFOLD_RENDER_TRACE proof
// that node.render_scene / node.gltf_texture_source's lazy pipeline compiles
// no longer stall a glTF scene layer's first rendered frame. Shares
// `journey_proof`'s headless ContentThread infra (same feature gate — no
// separate harness to maintain).
#[cfg(all(feature = "journey-proofs", target_os = "macos"))]
mod bug037_verify;
// BUG-219 P1 evidence harness (docs/IMPORT_RESPONSIVENESS_DESIGN.md P1):
// drives the REAL `Application::import_model_file` (unmodified) against the
// full 43MB ABeautifulGame.glb fixture, sequentially 3x, with a real
// content-shaped GPU runtime alive concurrently — the interactive-app shape
// BUG-219's headless `render-import` repro attempt never exercised. Shares
// `journey_proof`'s headless-`ContentThread` infra (same feature gate). Its
// one test is `#[ignore]`d — deliberate-run only, never the default sweep.
#[cfg(all(feature = "journey-proofs", target_os = "macos"))]
mod bug219_verify;
mod perform_mode;
// `cargo xtask perf-soak <project> --seconds N [--start <beats>]
// [--update-baseline]` — PERF_BUDGET_GATE_DESIGN.md P1: headless, real-time
// paced content-thread soak of a real project + baseline gate. macOS-only
// (native Metal `ContentThread`, same constraint as `journey-proofs`).
#[cfg(all(feature = "perf-soak", target_os = "macos"))]
mod perf_soak;
// Sibling frame loop for bare-glb/gltf input — PERF_BUDGET_GATE_DESIGN.md D7 /
// P2b. Same feature gate as `perf_soak` (dispatched from inside its `run()`).
#[cfg(all(feature = "perf-soak", target_os = "macos"))]
mod perf_soak_import;
mod project_io;
#[cfg(target_os = "macos")]
mod shared_texture;
#[cfg(target_os = "macos")]
mod texture_pane;
mod text_input;
mod ui_bridge;
mod tree_passes;
mod ui_frame;
mod ui_frame_profile;
mod ui_root;
#[cfg(feature = "ui-snapshot")]
mod ui_snapshot;
mod ui_translate;
mod user_library;
mod user_prefs;
mod window_input;
mod viewport_input;
// P5c evidence — test-only (`#![cfg(test)]` inside), see its module doc.
mod viewport_p5c_demo;
mod window_registry;
mod workspace;

fn main() {
    // UI motion layer OFF (experimental — evaluating whether the chrome
    // micro-animations earn their keep). Collapses every AnimF32/FlipList tween
    // to an instant snap; the motion code stays in place behind the flag, so
    // flipping this back to `true` restores it. See `manifold_ui::anim`.
    manifold_ui::anim::set_motion_enabled(false);

    // Headless UI snapshot subcommand (feature `ui-snapshot`): render the real
    // UI tree to a PNG + tree dump with no window, then exit before winit.
    #[cfg(feature = "ui-snapshot")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("ui-snap") {
            crate::ui_snapshot::run(&args[1..]);
            return;
        }
    }

    // Headless perf-soak subcommand (feature `perf-soak`): headless,
    // real-time-paced content-thread soak of a real project against the
    // frame-budget gate (docs/PERF_BUDGET_GATE_DESIGN.md P1).
    #[cfg(all(feature = "perf-soak", target_os = "macos"))]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("perf-soak") {
            crate::perf_soak::run(&args[1..]);
        }
    }

    // --- `--resume <breadcrumb-path>` (GIG_RESILIENCE_DESIGN §5.2) ---
    // The crash-recovery relaunch path: `manifold --resume <path>` skips
    // everything that isn't pixels. Parsed here (no other CLI arg parsing
    // exists in this binary — see `crash_log_tests` module for the closest
    // existing pattern) and handed to `Application` before the event loop
    // starts; the actual project load + rejoin happens once the content
    // thread + GPU are up, inside `Application::resumed()`.
    let resume_breadcrumb_path = parse_resume_arg(std::env::args());

    // --- Panic hook (10.1, 10.12) ---
    // Install before anything else so even early panics get logged to disk.
    // Critical with `panic=abort` + stripped symbols in release builds.
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let timestamp = {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            d.as_secs()
        };
        // Score-address the crash report (GIG_RESILIENCE_DESIGN §5.1): the
        // content-state drain publishes the latest known beat to a single
        // atomic every UI frame (`breadcrumb::publish_beat_for_crash_log`);
        // read it here since a panic hook has no `&Application` to call
        // through.
        let beat_line = match crate::breadcrumb::last_known_beat_for_crash_log() {
            Some(beat) => format!("current_beat={beat:.3}\n"),
            None => "current_beat=unknown\n".to_string(),
        };
        let msg = format!(
            "MANIFOLD PANIC at unix_ts={timestamp}\n{beat_line}{info}\n\nBacktrace:\n{backtrace}",
        );
        eprintln!("{msg}");

        // Write a timestamped crash log and rotate old ones (G10: one
        // overwritten crash.log meant the second crash destroyed evidence
        // of the first). Keep the most recent CRASH_LOGS_KEPT files.
        if let Some(dir) = crash_log_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = write_crash_log(&dir, &msg, timestamp);
            prune_crash_logs(&dir, CRASH_LOGS_KEPT);
        }
    }));

    // --- SIGPIPE handler (10.9) ---
    // Prevent broken-pipe signals from killing the process (e.g. piped output).
    #[cfg(target_os = "macos")]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // --- Multiple instance protection (10.11) ---
    // Hold a file lock for the lifetime of the process.
    #[cfg(target_os = "macos")]
    let _instance_lock = acquire_instance_lock();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("MANIFOLD starting...");

    // --- Unclean-exit detection (GIG_RESILIENCE_DESIGN §6, G10) ---
    // A sentinel file lives for the duration of a session and is removed on
    // clean exit. Finding it at startup means the last session ended in a
    // panic-abort, kill, or power loss — surface one quiet notice so the
    // crash log and last autosave actually get looked at. Placed after the
    // instance lock so a bounced second instance can't touch it.
    let previous_session_uncleanly_exited = detect_unclean_exit_and_arm_sentinel();

    // --- IOPMAssertion — prevent display sleep (10.2) ---
    #[cfg(target_os = "macos")]
    let _iopm_id = create_iopm_assertion();

    // --- App Nap suppression (10.3) ---
    #[cfg(target_os = "macos")]
    let _activity_token = suppress_app_nap();

    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut application = app::Application::new();
    application.show_crash_notice = previous_session_uncleanly_exited;
    application.resume_breadcrumb_path = resume_breadcrumb_path;
    event_loop.run_app(&mut application).unwrap();

    // Reached only on a clean event-loop exit (panics abort above): the next
    // launch should not report a crash.
    clear_session_sentinel();
}

// ---------------------------------------------------------------------------
// `--resume` CLI parsing (GIG_RESILIENCE_DESIGN §5.2)
// ---------------------------------------------------------------------------

/// Parse `--resume <breadcrumb-path>` out of the process arguments. Returns
/// `None` for a normal launch. Takes an iterator (rather than reading
/// `std::env::args()` internally) so it's testable without a real process.
fn parse_resume_arg(args: impl Iterator<Item = String>) -> Option<std::path::PathBuf> {
    let args: Vec<String> = args.collect();
    let idx = args.iter().position(|a| a == "--resume")?;
    args.get(idx + 1).map(std::path::PathBuf::from)
}

// ---------------------------------------------------------------------------
// Crash-log rotation + session sentinel (GIG_RESILIENCE_DESIGN §6 / G10)
// ---------------------------------------------------------------------------

/// How many timestamped crash logs to keep.
const CRASH_LOGS_KEPT: usize = 20;

/// `~/Library/Logs/com.latentspace.manifold` — same location the single
/// crash.log used before rotation.
fn crash_log_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("Library/Logs/com.latentspace.manifold"))
}

/// Write one timestamped crash log: `crash-<unix_ts>.log`. Fixed-width
/// timestamp keeps lexicographic order == chronological order.
fn write_crash_log(
    dir: &std::path::Path,
    msg: &str,
    unix_ts: u64,
) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(format!("crash-{unix_ts:010}.log"));
    std::fs::write(&path, msg)?;
    Ok(path)
}

/// Delete the oldest `crash-*.log` files beyond `keep`. Best-effort — this
/// runs inside the panic hook and must never itself fail loudly.
fn prune_crash_logs(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".log"))
        })
        .collect();
    if logs.len() <= keep {
        return;
    }
    // Fixed-width timestamps: name order is age order (oldest first).
    logs.sort();
    let excess = logs.len() - keep;
    for old in &logs[..excess] {
        let _ = std::fs::remove_file(old);
    }
}

/// `~/Library/Application Support/com.latentspace.manifold/session.active` —
/// exists while a session runs; removed by `clear_session_sentinel` on clean
/// exit.
fn session_sentinel_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::PathBuf::from(home)
            .join("Library/Application Support/com.latentspace.manifold/session.active")
    })
}

/// Returns true when the previous session left its sentinel behind (unclean
/// exit), then (re)creates the sentinel for this session.
fn detect_unclean_exit_and_arm_sentinel() -> bool {
    let Some(path) = session_sentinel_path() else {
        return false;
    };
    let unclean = path.exists();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, format!("pid={}\n", std::process::id())) {
        log::warn!("[Session] Couldn't write session sentinel: {e}");
    }
    unclean
}

/// Remove the sentinel — the definition of a clean exit.
fn clear_session_sentinel() {
    if let Some(path) = session_sentinel_path() {
        let _ = std::fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// Platform-specific helpers (macOS)
// ---------------------------------------------------------------------------

/// Acquire a file lock to prevent multiple instances.
/// Returns the locked `File` handle — the lock is released when dropped.
#[cfg(target_os = "macos")]
fn acquire_instance_lock() -> std::fs::File {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    let home = std::env::var("HOME").expect("HOME not set");
    let lock_dir = std::path::PathBuf::from(&home)
        .join("Library/Application Support/com.latentspace.manifold");
    std::fs::create_dir_all(&lock_dir).expect("failed to create lock directory");

    let lock_path = lock_dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("failed to open lock file");

    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let _ = writeln!(
            std::io::stderr(),
            "MANIFOLD is already running. Only one instance is allowed."
        );
        std::process::exit(1);
    }
    file
}

/// Create an IOPMAssertion to prevent the display from sleeping.
/// Returns the assertion ID (kept alive for the process lifetime).
#[cfg(target_os = "macos")]
fn create_iopm_assertion() -> u32 {
    use core_foundation::string::CFString;

    // kIOPMAssertionLevelOn
    const IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    use core_foundation::base::TCFType;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const std::ffi::c_void,
            level: u32,
            name: *const std::ffi::c_void,
            assertion_id: *mut u32,
        ) -> i32;
    }

    let assertion_type = CFString::new("PreventUserIdleDisplaySleep");
    let reason = CFString::new("MANIFOLD live performance");
    let mut assertion_id: u32 = 0;

    let status = unsafe {
        IOPMAssertionCreateWithName(
            assertion_type.as_CFTypeRef().cast(),
            IOPM_ASSERTION_LEVEL_ON,
            reason.as_CFTypeRef().cast(),
            &mut assertion_id,
        )
    };

    if status != 0 {
        log::warn!("IOPMAssertionCreateWithName failed (status={})", status);
    } else {
        log::info!("IOPMAssertion created (id={assertion_id}) — display sleep prevented");
    }

    assertion_id
}

/// Suppress App Nap so macOS doesn't throttle MANIFOLD when backgrounded.
/// Returns the activity token object (must stay alive).
#[cfg(target_os = "macos")]
fn suppress_app_nap() -> objc2::rc::Retained<objc2::runtime::AnyObject> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    // NSActivityUserInitiated (0x00000001) | NSActivityLatencyCritical (0xFF00000000).
    // Combined keeps the process at full speed and prevents App Nap.
    let options: u64 = 0xFF00000001;

    unsafe {
        let process_info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let reason = NSString::from_str("MANIFOLD live performance");
        let token: Retained<AnyObject> = msg_send![
            process_info,
            beginActivityWithOptions: options,
            reason: &*reason,
        ];
        token
    }
}

#[cfg(test)]
mod crash_log_tests {
    use super::{prune_crash_logs, write_crash_log};

    fn temp_dir(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "manifold-crashlog-test-{}-{}",
            test,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn rotation_keeps_newest_logs() {
        let dir = temp_dir("rotation");
        for ts in 0..25u64 {
            write_crash_log(&dir, &format!("crash {ts}"), 1_000_000 + ts).expect("write");
        }
        prune_crash_logs(&dir, 20);

        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();

        assert_eq!(names.len(), 20, "cap holds");
        // Oldest five (ts 1_000_000..1_000_004) pruned, newest kept.
        assert_eq!(names.first().unwrap(), "crash-0001000005.log");
        assert_eq!(names.last().unwrap(), "crash-0001000024.log");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_ignores_foreign_files_and_small_sets() {
        let dir = temp_dir("foreign");
        std::fs::write(dir.join("notes.txt"), "keep me").unwrap();
        write_crash_log(&dir, "only crash", 42).unwrap();
        prune_crash_logs(&dir, 20);

        assert!(dir.join("notes.txt").exists());
        assert!(dir.join("crash-0000000042.log").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod resume_arg_tests {
    use super::parse_resume_arg;

    fn args(v: &[&str]) -> impl Iterator<Item = String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn no_resume_flag_is_none() {
        assert_eq!(parse_resume_arg(args(&["manifold"])), None);
    }

    #[test]
    fn resume_flag_with_path_parses() {
        assert_eq!(
            parse_resume_arg(args(&["manifold", "--resume", "/tmp/show.manifold.breadcrumb"])),
            Some(std::path::PathBuf::from("/tmp/show.manifold.breadcrumb"))
        );
    }

    #[test]
    fn resume_flag_without_a_following_path_is_none() {
        assert_eq!(parse_resume_arg(args(&["manifold", "--resume"])), None);
    }

    #[test]
    fn resume_flag_not_confused_with_other_args() {
        assert_eq!(
            parse_resume_arg(args(&[
                "manifold",
                "--some-other-flag",
                "value",
                "--resume",
                "/path/a.manifold.breadcrumb"
            ])),
            Some(std::path::PathBuf::from("/path/a.manifold.breadcrumb"))
        );
    }
}
