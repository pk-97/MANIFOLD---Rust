//! BUG-219 (abeautifulgame-interactive-import-crashes-doubled-full-gpu-build)
//! P1 evidence harness — `docs/IMPORT_RESPONSIVENESS_DESIGN.md` P1.
//!
//! BUG-219's diagnosis (headless, via `render-import`) proved the production
//! `assemble_import_graph` path itself renders `ABeautifulGame.glb`
//! correctly and does not crash. The open question was whether the
//! interactive drag-drop path — `Application::import_model_file`
//! (`app_lifecycle.rs:584-787`) — crashes for a reason the headless path
//! never exercises: it builds a second, throwaway `manifold_gpu::GpuDevice`
//! purely for `validate_def`'s trial `PresetRuntime` build, on top of the
//! app's own real content-thread device, entirely synchronously on the
//! calling thread.
//!
//! This harness drives `import_model_file` directly — not a reimplementation
//! (`ui_snapshot/fixtures.rs`'s `gltf_scene()` deliberately mirrors the
//! import logic instead of calling the real method; this harness is the
//! first caller of the real method outside `app.rs`'s live drag-drop site).
//! `Application::new()` touches no window/GPU state (`gpu: None`,
//! `content_tx: None` — GPU setup only happens in `resumed()`, which
//! `import_model_file` never calls), so a headless `Application` plus a
//! stubbed `content_tx` channel is a faithful, unmodified call of the real
//! UI-thread code path.
//!
//! P3 (`IMPORT_RESPONSIVENESS_DESIGN.md` D3) moved the blocking work off
//! `import_model_file` onto a background thread — the method now returns
//! almost immediately (spawn+enqueue only). [`drive_import_to_completion`]
//! is this harness's adaptation: it drains `Application::drain_import_progress`
//! in a loop until the worker reports idle, the same "poll until done" shape
//! a real UI event loop gets for free from ticking every frame. The
//! sequential-pressure repro below still calls the real
//! `import_model_file`/`drain_import_progress` pair 3 times in one process —
//! only the "block until it's done" mechanism changed, from "the call itself
//! blocks" to "drain in a loop after the call returns".
//!
//! To emulate the app's actual runtime shape (a live content thread holding
//! its own real GPU device, ticking frames, WHILE the "UI thread" runs
//! `import_model_file` synchronously) a background thread runs a real
//! headless `ContentThread` (`headless_harness::headless_content_thread`,
//! the same construction `journey_proof`/BUG-035/BUG-037's harnesses use) at
//! 1920x1080 for the harness's lifetime. Peter had been dropping many scenes
//! in the session where this crashed — BUG-180 (`large-glb-import-oom-risk`)
//! names cumulative/non-deterministic memory pressure across imports as a
//! live adjacent hypothesis, so the primary repro drives the SAME real
//! `Application::import_model_file` call sequentially 3 times in one process
//! against the full 43MB `ABeautifulGame.glb` fixture, sampling process RSS
//! (`ps -o rss=`) around each call so even a non-crash run yields memory
//! evidence.
//!
//! Run with:
//! ```text
//! RUST_BACKTRACE=full cargo test -p manifold-app --features journey-proofs \
//!   bug219_interactive_import_sequential_pressure -- --ignored --nocapture
//! ```
//! `#[ignore]`d (per IMPORT_RESPONSIVENESS_DESIGN P1's gate): this loads a
//! real 43MB GLB through a real GPU-resident import path 3 times per run —
//! not default-sweep material, and it is explicitly probing a
//! crash/OOM-shaped bug, so it must never run un-asked-for in CI or the
//! default `cargo nextest run -p manifold-app` sweep.
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use manifold_core::Seconds;
use manifold_playback::engine::TickContext;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;
use crate::user_prefs::UserPrefs;

const W: u32 = 1920;
const H: u32 = 1080;

fn fixture_path() -> PathBuf {
    // Repo-root-relative — cargo test's cwd is the crate dir, so climb to
    // the workspace root the same way other fixture-consuming tests do.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // workspace root
    p.push("tests/fixtures/gltf/khronos/ABeautifulGame.glb");
    p
}

/// Sample this process's resident set size in MB via `ps` — cheap, no new
/// dependency, and accurate enough to see a doubling or a monotonic climb
/// across sequential imports.
fn rss_mb() -> Option<u64> {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().parse::<u64>().ok().map(|kb| kb / 1024)
}

fn log_rss(label: &str) {
    match rss_mb() {
        Some(mb) => eprintln!("[BUG-219] RSS {label}: {mb} MB"),
        None => eprintln!("[BUG-219] RSS {label}: <unavailable>"),
    }
}

/// P3 adaptation: `import_model_file` now only spawns+enqueues, so the old
/// "call it and it's done" repro shape needs an explicit drain loop —
/// `drain_import_progress` until the worker (and any queued follow-up) goes
/// idle, or `timeout` elapses (a hang here IS a repro-worthy finding, same
/// posture as the pre-P3 code catching a panic). Polls every 5ms, matching
/// the frame-cadence a real UI event loop would drain at.
fn drive_import_to_completion(app: &mut crate::app::Application, timeout: std::time::Duration) {
    let started = std::time::Instant::now();
    loop {
        app.drain_import_progress();
        if !app.import_worker_busy && app.import_queue.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < timeout,
            "BUG-219: import worker never reported done within {timeout:?} — treat as a repro"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Build a headless `Application` the same way `main.rs` does (`Application::
/// new()`, untouched), then wire the two fields `import_model_file` actually
/// reads that `new()` leaves `None`/disk-backed: an unbounded `content_tx`
/// stub (never drained — `import_model_file` only sends, never waits on a
/// reply, matching `ui_snapshot`'s precedent) and an in-memory `UserPrefs` so
/// the harness touches no real user config on disk.
fn headless_application() -> Application {
    let mut app = Application::new();
    let (tx, rx) = crossbeam_channel::unbounded::<ContentCommand>();
    // Leak the receiver's ownership into a background drain thread so sent
    // commands don't pile up unread (cosmetic only — `import_model_file`
    // never blocks on drain either way since the channel is unbounded).
    std::thread::spawn(move || {
        while rx.recv().is_ok() {}
    });
    app.content_tx = Some(tx);
    app.user_prefs = UserPrefs::for_test();
    app
}

/// Runs a real headless `ContentThread` (own real GPU device, own real
/// renderers) ticking frames on a background thread for the harness's
/// lifetime — the same "second, content-shaped GPU-resident runtime alive
/// during import" the real interactive app always has. Returns a flag the
/// caller flips to stop the loop.
fn spawn_background_content_thread() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    std::thread::spawn(move || {
        let project = manifold_core::project::Project::default();
        let mut ct = headless_content_thread(project, W, H);
        ct.engine.play();
        let dt = 1.0 / 60.0;
        let mut frame: u64 = 0;
        while !stop_clone.load(Ordering::Relaxed) {
            let ctx = TickContext {
                dt_seconds: Seconds(dt),
                realtime_now: Seconds(frame as f64 * dt),
                pre_render_dt: Seconds(dt),
                frame_count: frame,
                export_fixed_dt: Seconds::ZERO,
            };
            let tick_result = ct.engine.tick(ctx);
            ct.content_pipeline.render_content(
                &ct.gpu,
                &mut ct.engine,
                &tick_result,
                dt,
                frame,
                false,
                ct.editing_service.data_version(),
            );
            frame += 1;
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
    stop
}

/// P1's repro: the REAL `Application::import_model_file` call, driven
/// sequentially against the full 43MB `ABeautifulGame.glb` fixture 3 times
/// in one process, with a live background content-shaped GPU runtime
/// running concurrently (matching the real interactive app's shape) —
/// cumulative-pressure hypothesis from BUG-180 cross-referenced in
/// BUG-219's entry. RSS is sampled around every call so a non-crash run
/// still yields memory evidence for the backlog entry.
#[test]
#[ignore = "loads a real 43MB GLB through the real GPU-resident interactive import path up to 3x per run — deliberate-run only, probing a crash/OOM bug (IMPORT_RESPONSIVENESS_DESIGN P1); never in the default sweep or CI"]
fn bug219_interactive_import_sequential_pressure() {
    let path = fixture_path();
    assert!(
        path.exists(),
        "fixture missing at {path:?} — repo-root-relative resolution broke"
    );

    let stop_bg = spawn_background_content_thread();
    // Let the background content thread reach steady state before piling
    // imports on top of it, same as a real set already in progress.
    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut app = headless_application();
    log_rss("before any import");

    // P3: `import_model_file` is spawn+enqueue only now — the blocking work
    // (and anything that could crash) runs on a background thread this
    // process's default panic hook still prints to stderr on its own, even
    // though `catch_unwind` here can no longer intercept it (that only
    // catches panics on THIS thread). A worker-thread crash now shows up as
    // `drive_import_to_completion`'s timeout assert failing below, with the
    // panic message visible above it in `--nocapture` output — the repro
    // artifact just moved from "caught panic" to "timeout + stderr line".
    for i in 1..=3u32 {
        eprintln!("[BUG-219] --- import attempt {i}/3 ---");
        log_rss(&format!("before import {i}"));
        let started = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.import_model_file(&path, 0.0, None);
            drive_import_to_completion(&mut app, std::time::Duration::from_secs(60));
        }));
        let elapsed = started.elapsed();
        eprintln!(
            "[BUG-219] import {i} wall-clock (spawn + drain-to-done on this thread, the \
             real UI thread's per-frame drain compressed into a poll loop): {elapsed:?}"
        );
        log_rss(&format!("after import {i}"));
        match result {
            Ok(()) => eprintln!("[BUG-219] import {i} completed (no UI-thread panic, no timeout)"),
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                eprintln!("[BUG-219] import {i} PANICKED or TIMED OUT: {msg}");
                stop_bg.store(true, Ordering::Relaxed);
                panic!("BUG-219 reproduced on import attempt {i}/3: {msg}");
            }
        }
    }

    stop_bg.store(true, Ordering::Relaxed);
    eprintln!(
        "[BUG-219] all 3 sequential imports completed without a caught Rust panic — \
         see RSS samples above for memory evidence; a hard process abort (SIGKILL/SIGSEGV/SIGABRT) \
         instead of this line printing is itself the repro (re-run under `lldb`/`sample` to confirm)."
    );
}

/// IMPORT_RESPONSIVENESS_DESIGN.md P2 measurement: the sibling test above
/// (`headless_application`, `Application::new()` untouched) always has
/// `gpu: None` because `resumed()` never runs headless — so it can only
/// exercise D2's *lazy-fallback* branch (`validation_gpu_device`'s `None`
/// arm), never the branch that matters for the real running app: sharing
/// the UI-side device that already exists by the time a user can drop a
/// file (`resumed()` runs before the window can receive a drop). D2/P2
/// eliminated `GpuDevice::new()` from `app_lifecycle.rs` entirely — this
/// test seeds `Application.gpu` with a real `GpuContext` (its `new()` only
/// builds a `GpuDevice`, no window/surface needed, so this is reachable
/// headless) to measure that branch directly: with a device already
/// resident, validation should add ZERO new devices across all 3 imports,
/// unlike the pre-P2 code which built one throwaway `GpuDevice` PER import
/// on top of whatever device already existed.
#[test]
#[ignore = "loads a real 43MB GLB through the real GPU-resident interactive import path up to 3x per run — deliberate-run only, probing a crash/OOM bug (IMPORT_RESPONSIVENESS_DESIGN P1/P2); never in the default sweep or CI"]
fn bug219_interactive_import_with_existing_gpu_context_reuses_device() {
    let path = fixture_path();
    assert!(
        path.exists(),
        "fixture missing at {path:?} — repo-root-relative resolution broke"
    );

    let stop_bg = spawn_background_content_thread();
    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut app = headless_application();
    // Seed `self.gpu` the way `resumed()` would have by the time a real
    // window can receive a drop — this is the case D2/P2 optimizes for.
    app.gpu = Some(manifold_renderer::gpu::GpuContext::new());
    log_rss("before any import (gpu context pre-populated)");

    for i in 1..=3u32 {
        eprintln!("[BUG-219 P2] --- import attempt {i}/3 (shared-device case) ---");
        log_rss(&format!("before import {i}"));
        let started = std::time::Instant::now();
        app.import_model_file(&path, 0.0, None);
        drive_import_to_completion(&mut app, std::time::Duration::from_secs(60));
        let elapsed = started.elapsed();
        eprintln!("[BUG-219 P2] import {i} wall-clock (spawn + drain-to-done): {elapsed:?}");
        log_rss(&format!("after import {i}"));
    }

    stop_bg.store(true, Ordering::Relaxed);
    eprintln!(
        "[BUG-219 P2] all 3 imports completed against a pre-existing GPU context — \
         no per-import device allocation should show up as a step change in RSS."
    );
}
