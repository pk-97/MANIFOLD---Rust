//! Background model-import worker + its progress channel.
//! `docs/IMPORT_RESPONSIVENESS_DESIGN.md` D3+D4.
//!
//! `Application::import_model_file` (`app_lifecycle.rs`) used to run Blender
//! conversion → `assemble_import_graph` → `validate_def` synchronously on
//! the UI thread — a multi-second stall on a large model. [`run_import_worker`]
//! is the SAME sequence of blocking calls, unchanged, moved onto one
//! background `std::thread`; it sends [`ImportProgress`] events over a
//! `crossbeam_channel` the UI thread drains once per frame
//! (`Application::drain_import_progress`, `app_render.rs`) at the same site
//! `tick_import_failures` already drains BUG-133's probe-failure channel.
//!
//! What stays on the UI thread, deliberately: everything AFTER validation
//! succeeds — minting the embedded-preset id, installing the project-preset
//! overlay, and dispatching `ImportModelLayerCommand`/`AddClipCommand`. That
//! tail mutates `local_project` and reads the process-global preset
//! registry; per `CLAUDE.md`, all project mutation stays UI/content-thread
//! only, never a bare background thread. This module never sends a
//! `ContentCommand` — see the negative gate in the design's §3.

use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_renderer::node_graph::gltf_import::ImportReport;

use crate::blender_import;
use crate::user_prefs::UserPrefs;

/// One blocking step of the background import pipeline, surfaced to the UI
/// as a toast (D4). Mirrors the three blocking calls the old synchronous
/// `import_model_file` made in sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportStage {
    Converting,
    Parsing,
    Validating,
}

impl ImportStage {
    /// Toast-text verb — `"Importing <name> — <verb>…"` (D4).
    pub(crate) fn verb(self) -> &'static str {
        match self {
            ImportStage::Converting => "converting",
            ImportStage::Parsing => "parsing",
            ImportStage::Validating => "validating",
        }
    }
}

/// One event on the background-import channel (D3's committed shape).
/// `Stage` fires zero or more times per import (zero when the source is
/// already glTF and skips the Converting stage); exactly one of
/// `Done`/`Failed` fires last, always — a drop is never silently swallowed.
pub(crate) enum ImportProgress {
    Stage {
        path: PathBuf,
        stage: ImportStage,
    },
    Done {
        path: PathBuf,
        graph: Box<EffectGraphDef>,
        report: ImportReport,
        /// Set only when the source needed Blender conversion — folded into
        /// the UI-thread success log line exactly as the old inline code did.
        conversion_report_line: Option<String>,
        drop_beat: f32,
        layer_under_cursor: Option<usize>,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
}

/// Everything [`run_import_worker`] needs, captured by value so it can cross
/// the thread boundary. `blender_path_pref` is the one `UserPrefs` value
/// `discover_blender` actually reads (`BLENDER_PATH_PREF_KEY`) — extracted
/// on the UI thread rather than sending `UserPrefs` itself across, since
/// `UserPrefs` carries no `Clone`/`Send` impl of its own and this is the
/// only field the conversion path touches.
pub(crate) struct ImportRequest {
    pub(crate) path: PathBuf,
    pub(crate) drop_beat: f32,
    pub(crate) layer_under_cursor: Option<usize>,
    pub(crate) blender_path_pref: String,
}

/// Runs on a background `std::thread` (D3) — the entire blocking body of the
/// old synchronous `import_model_file`, minus the UI-thread command-dispatch
/// tail. Sends a `Stage` event before each blocking step and exactly one
/// `Done`/`Failed` at the end; never sends a `ContentCommand`.
pub(crate) fn run_import_worker(
    req: ImportRequest,
    device: Arc<manifold_gpu::GpuDevice>,
    repo_root: PathBuf,
    progress_tx: crossbeam_channel::Sender<ImportProgress>,
) {
    let ImportRequest {
        path,
        drop_beat,
        layer_under_cursor,
        blender_path_pref,
    } = req;

    // FBX/.obj/.dae (MANIFOLD is glTF-only internally, per
    // IMPORT_ANYTHING_WAVE_DESIGN.md Lane W3): convert through the user's
    // installed Blender first, then continue with the produced `.glb`
    // through the exact same path a native glTF drop takes.
    let mut conversion_report_line: Option<String> = None;
    let import_path: PathBuf = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    {
        Some(ext) if blender_import::is_blender_convertible_extension(&ext) => {
            let _ = progress_tx.send(ImportProgress::Stage {
                path: path.clone(),
                stage: ImportStage::Converting,
            });
            // Stand-in `UserPrefs` carrying only the one key
            // `discover_blender` reads — see the doc comment on
            // `ImportRequest::blender_path_pref`.
            let mut prefs = UserPrefs::ephemeral();
            if !blender_path_pref.is_empty() {
                prefs.set_string(blender_import::BLENDER_PATH_PREF_KEY, &blender_path_pref);
            }
            match blender_import::convert_via_blender(&prefs, &repo_root, &path) {
                Ok(outcome) => {
                    conversion_report_line = Some(match &outcome.blender_version {
                        Some(v) => format!(
                            "converted from {} via Blender {v}",
                            blender_import::source_format_label(&ext)
                        ),
                        None => format!(
                            "converted from {} via Blender",
                            blender_import::source_format_label(&ext)
                        ),
                    });
                    outcome.glb_path
                }
                Err(e) => {
                    let _ = progress_tx.send(ImportProgress::Failed {
                        path,
                        message: format!("Blender conversion failed: {e}"),
                    });
                    return;
                }
            }
        }
        _ => path.clone(),
    };

    // Parse + assemble. Errors (no geometry with materials, unreadable
    // file) abort the drop with a Failed event rather than leaving a
    // half-built layer behind.
    let _ = progress_tx.send(ImportProgress::Stage {
        path: path.clone(),
        stage: ImportStage::Parsing,
    });
    let (graph, report) =
        match manifold_renderer::node_graph::gltf_import::assemble_import_graph(&import_path) {
            Ok(pair) => pair,
            Err(e) => {
                let _ = progress_tx.send(ImportProgress::Failed {
                    path,
                    message: format!("glTF import failed: {e}"),
                });
                return;
            }
        };

    // The assembler is code and has bugs (GRAPH_TOOLING_DESIGN D6): run its
    // output through the same validate_def pipeline the runtime loader
    // takes, and abort on failure rather than let a malformed def surface
    // later as wrong pixels far from the cause. Never a silent partial
    // import. IMPORT_RESPONSIVENESS_DESIGN.md D2: the device is the app's
    // shared handle, passed in — never a fresh `GpuDevice::new()` here.
    let _ = progress_tx.send(ImportProgress::Stage {
        path: path.clone(),
        stage: ImportStage::Validating,
    });
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let validation = manifold_renderer::node_graph::validate_def(
        &graph,
        &registry,
        manifold_renderer::node_graph::ValidateKind::Generator,
        &device,
    );
    if !validation.is_valid() {
        let messages: Vec<String> =
            validation.errors.iter().map(|issue| issue.message.clone()).collect();
        let _ = progress_tx.send(ImportProgress::Failed {
            path,
            message: format!("assembled graph failed validation: {}", messages.join("; ")),
        });
        return;
    }

    let _ = progress_tx.send(ImportProgress::Done {
        path,
        graph: Box::new(graph),
        report,
        conversion_report_line,
        drop_beat,
        layer_under_cursor,
    });
}
