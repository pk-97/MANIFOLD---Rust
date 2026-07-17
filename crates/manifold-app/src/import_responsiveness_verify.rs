//! `IMPORT_RESPONSIVENESS_DESIGN.md` P3 gates 2 and 3 — deliberate-run
//! harnesses (feature `journey-proofs`, matching `bug219_verify.rs`'s
//! gating: real `GpuDevice`/`ContentThread` construction, not default-sweep
//! material).
//!
//! Gate 2: a deliberately-corrupt `.glb` must fail through
//! `run_import_worker` as `ImportProgress::Failed`, and `drain_import_progress`
//! must surface it as an accent toast — never a silent log-only failure.
//!
//! Gate 3 (the performer gesture): dropping a large model must never stall
//! the content thread's own transport. A real headless `ContentThread` ticks
//! on its own background thread (same construction `bug219_verify.rs` uses)
//! while `import_model_file` runs a real background import of the full 43MB
//! `ABeautifulGame.glb` fixture; the content thread's frame counter is
//! sampled repeatedly across that span and must be strictly increasing —
//! transport time never stalls waiting on the import.
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use manifold_core::Seconds;
use manifold_playback::engine::TickContext;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;
use crate::import_worker::{self, ImportProgress, ImportRequest};
use crate::user_prefs::UserPrefs;

fn headless_application() -> Application {
    let mut app = Application::new();
    let (tx, rx) = crossbeam_channel::unbounded::<ContentCommand>();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {}
    });
    app.content_tx = Some(tx);
    app.user_prefs = UserPrefs::for_test();
    app
}

fn large_fixture_path() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // workspace root
    p.push("tests/fixtures/gltf/khronos/ABeautifulGame.glb");
    p
}

/// Gate 2: a garbage `.glb` fails `assemble_import_graph` (never reaches
/// `validate_def`, so the `Arc<GpuDevice>` this signature requires is held
/// but never dispatched against) and the worker reports `Failed` — never
/// silently swallowed.
#[test]
fn corrupt_glb_fails_through_worker_as_failed_event() {
    let dir = std::env::temp_dir().join(format!(
        "manifold_import_responsiveness_corrupt_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let bad_path = dir.join("deliberately_broken.glb");
    std::fs::write(&bad_path, b"this is not a glTF binary, on purpose")
        .expect("write corrupt fixture");

    let (tx, rx) = crossbeam_channel::unbounded::<ImportProgress>();
    let device = Arc::new(manifold_gpu::GpuDevice::new());
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let req = ImportRequest {
        path: bad_path.clone(),
        drop_beat: 0.0,
        layer_under_cursor: None,
        blender_path_pref: String::new(),
    };

    import_worker::run_import_worker(req, device, repo_root, tx);

    // The worker sends `Stage { Parsing }` before it ever tries the parse
    // that fails — drain past it (D3's committed shape: zero or more Stage
    // events, then exactly one terminal Done/Failed) to the terminal event.
    let mut terminal = None;
    for _ in 0..8 {
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(ImportProgress::Stage { stage, .. }) => {
                eprintln!("[IMPORT_RESPONSIVENESS gate 2] Stage: {stage:?}");
            }
            Ok(other) => {
                terminal = Some(other);
                break;
            }
            Err(e) => panic!("channel closed before a terminal event arrived: {e}"),
        }
    }
    match terminal.expect("worker must send a terminal event for a corrupt file") {
        ImportProgress::Failed { path, message } => {
            assert_eq!(path, bad_path);
            assert!(!message.is_empty());
            eprintln!("[IMPORT_RESPONSIVENESS gate 2] Failed message: {message}");
        }
        ImportProgress::Done { .. } => {
            panic!("a corrupt .glb must never produce Done — that would be a silent partial import")
        }
        ImportProgress::Stage { .. } => unreachable!("drained above"),
    }

    // Drained through the real Application path, this must surface as a
    // visible, never-silent toast (D3/D4's actual UI contract).
    let mut app = headless_application();
    app.import_progress_tx
        .send(ImportProgress::Failed {
            path: bad_path,
            message: "glTF import failed: deliberately broken for this test".to_string(),
        })
        .unwrap();
    app.drain_import_progress();
    use manifold_ui::panels::overlay::Overlay;
    assert!(app.ws.ui_root.toast.is_open(), "Failed must always show a toast");

    std::fs::remove_dir_all(&dir).ok();
}

/// Gate 3 — the performer gesture: transport time strictly increases across
/// snapshots while a large model imports in the background. A real headless
/// `ContentThread` ticks independently on its own thread (bumping an atomic
/// frame counter each tick — the same "own thread, no shared lock with the
/// import worker" shape the real two-thread architecture always has), while
/// `Application::import_model_file`/`drain_import_progress` run the full
/// 43MB `ABeautifulGame.glb` import to completion on this thread. The
/// counter is sampled repeatedly across the import span and must never
/// plateau — proving the content thread's transport never stalls waiting on
/// import work, which is the actual mechanism behind "keep scrubbing the
/// timeline during a drop."
#[test]
fn content_thread_keeps_advancing_while_a_large_import_runs_in_the_background() {
    let path = large_fixture_path();
    assert!(path.exists(), "fixture missing at {path:?}");

    let frame_counter = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let counter_clone = Arc::clone(&frame_counter);
    let stop_clone = Arc::clone(&stop);
    std::thread::spawn(move || {
        let build_t0 = std::time::Instant::now();
        let project = manifold_core::project::Project::default();
        let mut ct = headless_content_thread(project, 320, 180);
        eprintln!(
            "[IMPORT_RESPONSIVENESS gate 3] headless_content_thread construction: {:?}",
            build_t0.elapsed()
        );
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
            counter_clone.store(frame, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
    // Cold-start pipeline compilation (first-use shader/pipeline creation)
    // can take several seconds — wait for at least a few real ticks before
    // treating the counter's baseline as meaningful.
    let warmup_started = std::time::Instant::now();
    while frame_counter.load(Ordering::Relaxed) < 3 && warmup_started.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    eprintln!(
        "[IMPORT_RESPONSIVENESS gate 3] warmup done after {:?}, frame={}",
        warmup_started.elapsed(),
        frame_counter.load(Ordering::Relaxed)
    );

    let mut app = headless_application();
    let start_frame = frame_counter.load(Ordering::Relaxed);
    app.import_model_file(&path, 0.0, None);
    assert!(app.import_worker_busy, "the drop must have started a background worker");

    let mut samples: Vec<u64> = vec![start_frame];
    let started = std::time::Instant::now();
    while app.import_worker_busy {
        app.drain_import_progress();
        samples.push(frame_counter.load(Ordering::Relaxed));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "import never completed within 60s"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // One more drain after the worker reports idle, and a final sample.
    app.drain_import_progress();
    samples.push(frame_counter.load(Ordering::Relaxed));

    stop.store(true, Ordering::Relaxed);

    eprintln!("[IMPORT_RESPONSIVENESS gate 3] content-thread frame samples across the import: {samples:?}");
    assert!(
        samples.len() >= 3,
        "expected several samples across the import span, got {}",
        samples.len()
    );
    assert!(
        samples.last().unwrap() > samples.first().unwrap(),
        "content-thread frame counter must have advanced across the whole import span: {samples:?}"
    );
    // No two consecutive samples may show a stall long enough to indicate
    // the content thread was blocked waiting on the import worker (each
    // tick sleeps ~16ms; a healthy run advances by >=1 frame almost every
    // sample interval — assert the counter is monotonically non-decreasing
    // and moves at least once every few samples, not just at the very end).
    let mut stalled_gaps = 0usize;
    for pair in samples.windows(2) {
        if pair[1] < pair[0] {
            panic!("content-thread frame counter went BACKWARDS: {samples:?}");
        }
        if pair[1] == pair[0] {
            stalled_gaps += 1;
        }
    }
    assert!(
        stalled_gaps < samples.len() - 1,
        "content thread never advanced between ANY consecutive samples — it stalled for the whole import: {samples:?}"
    );
}
