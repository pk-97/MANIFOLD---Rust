//! `IMPORT_RESPONSIVENESS_DESIGN.md` P3 gate 1 — the L3 flow driver
//! (`scripts/ui-flows/`) has no way to express a file drop yet (no flow verb
//! drives drag-drop), so per the phase brief's own stated fallback this is
//! an integration test on the drain loop instead: it proves `Stage` events
//! surface as a toast BEFORE `Done` runs the command-dispatch tail, using
//! `Application::drain_import_progress` directly — the exact per-frame call
//! `app_render.rs` makes — with hand-fed `ImportProgress` events instead of
//! a real background worker (no GPU device needed: `assemble_import_graph`
//! is a plain CPU parse, and this test never reaches `validate_def`, so it
//! runs in the default GPU-free sweep, unlike `bug219_verify.rs`'s
//! `journey-proofs`-gated harness).

#![cfg(test)]

use manifold_ui::panels::overlay::Overlay;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::import_worker::{ImportProgress, ImportStage};

fn small_fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/BoxAnimated.glb")
}

/// Same shape as `bug219_verify.rs`'s `headless_application()`: a real
/// `Application::new()` (no window/GPU touched) with `content_tx` wired to
/// an unbounded stub channel so `finish_import_model`'s `content_tx.send`
/// calls have somewhere to land.
fn headless_application() -> Application {
    let mut app = Application::new();
    let (tx, rx) = crossbeam_channel::unbounded::<ContentCommand>();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {}
    });
    app.content_tx = Some(tx);
    app
}

#[test]
fn stage_event_shows_toast_before_done_dispatches_the_layer() {
    let path = small_fixture_path();
    assert!(path.exists(), "fixture missing at {path:?}");

    let mut app = headless_application();
    let layers_before = app.local_project.timeline.layers.len();

    // A real graph — CPU-only parse, no GPU, no `validate_def` call (the
    // worker's `Validating` stage is deliberately never reached in this
    // test; validate_def's own correctness is proven by its own tests).
    let (graph, report) = manifold_renderer::node_graph::gltf_import::assemble_import_graph(&path)
        .unwrap_or_else(|e| panic!("assemble_import_graph({}) failed: {e}", path.display()));

    // 1. Hand-feed a `Stage` event, exactly as `run_import_worker` would
    //    before its `Parsing` step, and drain it.
    app.import_progress_tx
        .send(ImportProgress::Stage {
            path: path.clone(),
            stage: ImportStage::Parsing,
        })
        .unwrap();
    app.drain_import_progress();

    assert!(
        app.ws.ui_root.toast.is_open(),
        "Stage event must show the toast immediately"
    );
    // No command dispatch yet — Done hasn't arrived.
    assert_eq!(
        app.local_project.timeline.layers.len(),
        layers_before,
        "Stage alone must never dispatch the import — that's Done's job"
    );

    // 2. Now feed Done — the drain must run the command-dispatch tail this
    //    frame, on the UI thread, unchanged from the old synchronous path.
    app.import_progress_tx
        .send(ImportProgress::Done {
            path: path.clone(),
            graph: Box::new(graph),
            report,
            conversion_report_line: None,
            drop_beat: 0.0,
            layer_under_cursor: None,
        })
        .unwrap();
    app.drain_import_progress();

    assert_eq!(
        app.local_project.timeline.layers.len(),
        layers_before + 1,
        "Done must dispatch the new layer exactly once"
    );
    assert!(!app.import_worker_busy, "Done must clear the busy flag");
}

#[test]
fn failed_event_shows_an_accent_toast_never_silent() {
    let mut app = headless_application();
    app.import_worker_busy = true;

    app.import_progress_tx
        .send(ImportProgress::Failed {
            path: small_fixture_path(),
            message: "glTF import failed: deliberately broken for this test".to_string(),
        })
        .unwrap();
    app.drain_import_progress();

    assert!(
        app.ws.ui_root.toast.is_open(),
        "a Failed import must never be silent — the toast is the invariant's enforcement"
    );
    assert!(!app.import_worker_busy, "Failed must clear the busy flag too");
}

#[test]
fn a_second_drop_while_busy_is_queued_not_run_in_parallel() {
    let mut app = headless_application();
    // Simulate an in-flight worker without actually spawning one (spawning
    // touches the lazy-fallback GPU device via `validation_gpu_device` —
    // this test is about the enqueue branch only, which never does).
    app.import_worker_busy = true;
    assert!(app.import_queue.is_empty());

    let path = small_fixture_path();
    app.import_model_file(&path, 4.0, None);

    assert!(app.import_worker_busy, "still busy — the queued request didn't spawn");
    assert_eq!(app.import_queue.len(), 1, "the drop while busy must be queued FIFO");
    assert_eq!(app.import_queue[0].path, path);
    assert_eq!(app.import_queue[0].drop_beat, 4.0);
}
