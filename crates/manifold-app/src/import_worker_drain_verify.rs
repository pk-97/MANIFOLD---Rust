//! Import worker drain gate 1 — the L3 flow driver
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
    std::thread::spawn(move || while rx.recv().is_ok() {});
    app.content_tx = Some(tx);
    app
}

#[test]
fn stage_event_shows_toast_before_done_dispatches_the_layer() {
    let path = small_fixture_path();
    assert!(path.exists(), "fixture missing at {path:?}");

    let mut app = headless_application();
    let layers_before = app.local_project.timeline.layers.len();

    // A real model — CPU-only parse, no GPU.
    let read = manifold_nodes_scene::node_graph::gltf_import::read_model(&path)
        .unwrap_or_else(|e| panic!("read_model({}) failed: {e}", path.display()));

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
            file: path.clone(),
            model: Box::new(read.model),
            facts: read.facts,
            warnings: read.warnings,
            conversion_report_line: None,
            target: crate::import_worker::ImportTarget::Drop {
                beat: 0.0,
                under: None,
            },
        })
        .unwrap();
    app.drain_import_progress();

    assert_eq!(
        app.local_project.timeline.layers.len(),
        layers_before + 1,
        "Done must dispatch the new layer exactly once"
    );
    assert!(!app.import_worker_busy, "Done must clear the busy flag");
    // The worker's facts travel on the new layer's document, so neither
    // thread's compile opens the file.
    let file = path.to_string_lossy();
    let carried = app
        .local_project
        .timeline
        .layers
        .iter()
        .filter_map(|layer| layer.gen_params()?.scene.as_ref())
        .any(|doc| doc.models.contains(&file));
    assert!(
        carried,
        "the new scene layer's document carries the file's facts"
    );
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
    assert!(
        !app.import_worker_busy,
        "Failed must clear the busy flag too"
    );
}

#[test]
fn a_second_drop_while_busy_is_queued_not_run_in_parallel() {
    let mut app = headless_application();
    // Simulate an in-flight worker without actually spawning one (spawning
    // starts a thread —
    // this test is about the enqueue branch only, which never does).
    app.import_worker_busy = true;
    assert!(app.import_queue.is_empty());

    let path = small_fixture_path();
    app.import_model_file(&path, 4.0, None);

    assert!(
        app.import_worker_busy,
        "still busy — the queued request didn't spawn"
    );
    assert_eq!(
        app.import_queue.len(),
        1,
        "the drop while busy must be queued FIFO"
    );
    assert_eq!(app.import_queue[0].path, path);
    assert_eq!(
        app.import_queue[0].target,
        crate::import_worker::ImportTarget::Drop {
            beat: 4.0,
            under: None
        }
    );
}

/// The import's layer travels by id: a layer removed while the file parsed
/// drops the import with a toast, never landing it on whatever layer took
/// its place in the stack.
#[test]
fn an_import_whose_layer_went_is_dropped_with_a_toast() {
    let path = small_fixture_path();
    let read = manifold_nodes_scene::node_graph::gltf_import::read_model(&path)
        .unwrap_or_else(|e| panic!("read_model({}) failed: {e}", path.display()));
    let mut app = headless_application();
    let layers_before = app.local_project.timeline.layers.len();
    let gone = manifold_core::LayerId::new("removed-while-parsing");
    for target in [
        crate::import_worker::ImportTarget::Drop {
            beat: 0.0,
            under: Some(gone.clone()),
        },
        crate::import_worker::ImportTarget::Into {
            layer: gone.clone(),
            parent: None,
        },
    ] {
        app.import_worker_busy = true;
        app.import_progress_tx
            .send(ImportProgress::Done {
                path: path.clone(),
                file: path.clone(),
                model: Box::new(read.model.clone()),
                facts: read.facts.clone(),
                warnings: Vec::new(),
                conversion_report_line: None,
                target,
            })
            .unwrap();
        app.drain_import_progress();
        assert_eq!(
            app.local_project.timeline.layers.len(),
            layers_before,
            "the import landed although its layer is gone"
        );
        assert!(app.ws.ui_root.toast.is_open(), "a dropped import says why");
        assert!(!app.import_worker_busy);
    }
}
