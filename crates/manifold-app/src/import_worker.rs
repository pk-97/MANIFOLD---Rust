//! Background model-import worker + its progress channel.
//!
//! `Application::import_model_file` (`app_lifecycle.rs`) would run Blender
//! conversion and the glTF parse on the UI thread — a multi-second stall on
//! a large model. [`run_import_worker`] runs those blocking calls on one
//! background `std::thread`; it sends [`ImportProgress`] events over a
//! `crossbeam_channel` the UI thread drains once per frame
//! (`Application::drain_import_progress`, `app_render.rs`) at the same site
//! `tick_import_failures` already drains BUG-133's probe-failure channel.
//!
//! What stays on the UI thread, deliberately: everything AFTER the parse
//! succeeds — adding the model to a scene layer (`app_lifecycle.rs`,
//! `finish_import_model`). That tail mutates `local_project`; per
//! `CLAUDE.md`, all project mutation stays UI/content-thread only, never a
//! bare background thread. This module never sends a
//! `ContentCommand` — see the negative gate in the design's section 3.

use std::path::PathBuf;

use manifold_core::scene_doc::{ImportedModel, ModelFacts};

use crate::blender_import;
use crate::user_prefs::UserPrefs;

/// One blocking step of the background import pipeline, surfaced to the UI
/// as a toast (D4). Mirrors the three blocking calls the old synchronous
/// `import_model_file` made in sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportStage {
    Converting,
    Parsing,
}

impl ImportStage {
    /// Toast-text verb — `"Importing <name> — <verb>…"` (D4).
    pub(crate) fn verb(self) -> &'static str {
        match self {
            ImportStage::Converting => "converting",
            ImportStage::Parsing => "parsing",
        }
    }
}

/// Where a parsed model goes. Layers and items travel by id, never by
/// position: the stack can change while the file parses.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ImportTarget {
    /// A drop at `beat`: the scene layer `under` the cursor, else the active
    /// one, takes the model; with neither, a new scene layer sits above
    /// `under` (at the top when the drop missed the tracks).
    Drop {
        beat: f32,
        under: Option<manifold_core::LayerId>,
    },
    /// Add > Mesh from file: into scene layer `layer`, under `parent` when it
    /// is a Group.
    Into {
        layer: manifold_core::LayerId,
        parent: Option<manifold_core::scene_doc::ItemId>,
    },
    /// Model File swap: `item` of scene layer `layer` reads the new file.
    Swap {
        layer: manifold_core::LayerId,
        item: manifold_core::scene_doc::ItemId,
    },
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
        /// The `.glb` the model was read from: the original, or the one Blender
        /// converted it to. It is the import Group.s Source.
        file: PathBuf,
        model: Box<ImportedModel>,
        /// What the compiler reads from the file, from the same parse.
        facts: ModelFacts,
        /// What the file's materials hold that the surface cannot draw.
        warnings: Vec<String>,
        /// Set only when the source needed Blender conversion — folded into
        /// the UI-thread success log line exactly as the old inline code did.
        conversion_report_line: Option<String>,
        target: ImportTarget,
    },
    Failed {
        path: PathBuf,
        message: String,
    },
    /// A model file's facts, read for a document that names the file
    /// without them (`crate::scene_models`), and its material warnings.
    Facts {
        path: String,
        read: Result<(ModelFacts, Vec<String>), String>,
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
    pub(crate) target: ImportTarget,
    pub(crate) blender_path_pref: String,
}

/// Runs on a background `std::thread` (D3) — the entire blocking body of the
/// old synchronous `import_model_file`, minus the UI-thread command-dispatch
/// tail. Sends a `Stage` event before each blocking step and exactly one
/// `Done`/`Failed` at the end; never sends a `ContentCommand`.
pub(crate) fn run_import_worker(
    req: ImportRequest,
    repo_root: PathBuf,
    progress_tx: crossbeam_channel::Sender<ImportProgress>,
) {
    let ImportRequest {
        path,
        target,
        blender_path_pref,
    } = req;

    // FBX/.obj/.dae (MANIFOLD is glTF-only internally, per
    // Convert through the user's
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

    // Parse. Errors (no meshes, unreadable file) abort the drop with a
    // Failed event rather than leaving a half-built layer behind.
    let _ = progress_tx.send(ImportProgress::Stage {
        path: path.clone(),
        stage: ImportStage::Parsing,
    });
    let read = match manifold_nodes_scene::node_graph::gltf_import::read_model(&import_path) {
        Ok(read) => read,
        Err(e) => {
            let _ = progress_tx.send(ImportProgress::Failed {
                path,
                message: format!("glTF import failed: {e}"),
            });
            return;
        }
    };

    let _ = progress_tx.send(ImportProgress::Done {
        path,
        file: import_path,
        model: Box::new(read.model),
        facts: read.facts,
        warnings: read.warnings,
        conversion_report_line,
        target,
    });
}
