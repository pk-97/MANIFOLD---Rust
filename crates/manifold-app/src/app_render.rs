//! Rendering methods for Application — extracted from app.rs.
//!
//! Contains `tick_and_render()`, `present_all_windows()`, and the text input
//! overlay rendering helper. All methods are `impl Application` blocks that
//! operate on the struct defined in app.rs.

use manifold_ui::{ClipAction, LayerAction, MarkerAction, ParamsAction, ProjectAction, RootAction, TransportAction};
use manifold_renderer::ui_renderer::UIRenderer;

use manifold_ui::node::FontWeight;
use manifold_ui::panels::{PanelAction, ScrubPhase, ScrubValue, ValueRef};
use manifold_ui::timeline_editing_host::TimelineEditingHost;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use manifold_editing::command::Command;
use manifold_editing::commands::effects::BindingMappingEdit;

pub(crate) use crate::frame::present::format_scope_readout;
pub(crate) use crate::frame::present::fmt_table_cell_seed;

pub(crate) use crate::editor_bridge::build_mapping_command;
pub(crate) use crate::editor_bridge::seed_def_for_project;
pub(crate) use crate::editor_bridge::resolve_canvas_binding;
pub(crate) use crate::editor_bridge::serialized_value_as_f32;











/// A node-face scrub session currently rerouted through a card binding's
/// write-back path (`PARAM_TWO_WAY_BINDING_DESIGN.md` D1). Opened at the
/// first `SetGraphNodeParam` on a bound `(node_id, param_name)`, updated live
/// on every subsequent move, closed on the matching
/// `EndGraphNodeParamScrub` — one undo-worthy `ChangeGraphParamCommand`
/// covering the whole drag (`old_value` → the last `current_value`), not one
/// per pointer-move.
#[derive(Debug, Clone)]
pub(crate) struct BoundNodeParamDrag {
    target: manifold_core::GraphTarget,
    node_id: u32,
    param_name: String,
    outer_param_id: String,
    /// Outer card value at gesture start — the undo baseline.
    old_value: f32,
    /// Outer card value as of the last move — the undo redo target.
    current_value: f32,
}

impl BoundNodeParamDrag {
    /// Re-apply the in-flight card value after snapshot acceptance — the
    /// same write the live scrub path uses (`with_preset_graph_mut` +
    /// `set_base_param` on `outer_param_id`), mirroring
    /// `ResolvedScrub::Param`'s restore arm (BUG-262 precedent). Called via
    /// `ResolvedScrub::BoundNodeParam`'s restore arm (P-I).
    pub(crate) fn apply(&self, project: &mut manifold_core::project::Project) {
        project.with_preset_graph_mut(&self.target, |inst| {
            inst.set_base_param(&self.outer_param_id, self.current_value);
        });
    }
}

/// BUG-282: an ordinary (unbound) node-face param/vec scrub session — the
/// mirror of [`BoundNodeParamDrag`] for the un-rerouted path. Opened at the
/// first `SetGraphNodeParam`/`SetOuterParam`-family move on a given
/// `(target, node_id, param_name, scope_path)`, live-written on every
/// subsequent move via `MutateProjectLive` (no undo entry), closed on the
/// matching `EndGraphNodeParamScrub` — ONE undo-worthy
/// `SetGraphNodeParamCommand` for the whole drag (`pre_drag_value` →
/// `current_value`), not one `Execute` per pointer-move tick.
#[derive(Debug, Clone)]
pub(crate) struct UnboundNodeParamDrag {
    target: manifold_core::GraphTarget,
    node_id: u32,
    param_name: String,
    scope_path: Vec<u32>,
    catalog_default: manifold_core::effect_graph_def::EffectGraphDef,
    /// Value before the drag started. `None` means the key was absent —
    /// the same `Option<SerializedParamValue>` shape `with_previous` takes.
    pre_drag_value: Option<manifold_core::effect_graph_def::SerializedParamValue>,
    /// Value as of the last move — the undo redo target.
    current_value: manifold_core::effect_graph_def::SerializedParamValue,
}

impl Application {




















    pub(crate) fn tick_and_render(&mut self) {
        let dt = self.frame_timer.consume_tick();
        let realtime = self.frame_timer.realtime_since_start();
        self.time_since_start = realtime as f32;

        // Performance mode: skip the entire normal UI tick path. The content
        // thread keeps running (independent), the output window keeps presenting
        // (own display link), and the main window draws only the perform HUD.
        if self.perform.active {
            self.tick_perform_mode();
            return;
        }

        // Content rendering now runs on dedicated thread — no cadence check needed here.
        // `frame_t0` / `seg` drive the UI frame profiler (no-op unless
        // MANIFOLD_UI_FRAME_PROFILE=1). `seg` is reset at each section boundary.
        let frame_t0 = std::time::Instant::now();
        let mut seg = frame_t0;

        // 1. Drain state from content thread
        if let Some(ref rx) = self.state_rx {
            // Drain all pending states, keep the latest
            while let Ok(state) = rx.try_recv() {
                let drag_active =
                    self.overlay.drag_mode() != manifold_ui::interaction_overlay::DragMode::None;
                // Suppress snapshots until content thread catches up after a local project load.
                // Safety net: timeout after 120 frames (~2s) to prevent indefinite suppression.
                const MAX_SUPPRESS_FRAMES: u64 = 120;
                let suppress_timed_out = self.suppress_snapshot_until > 0
                    && self
                        .frame_count
                        .saturating_sub(self.suppress_snapshot_set_at)
                        >= MAX_SUPPRESS_FRAMES;
                if suppress_timed_out {
                    log::warn!("[UI] Snapshot suppression timed out — accepting snapshot");
                    self.suppress_snapshot_until = 0;
                }
                let suppressed = state.data_version < self.suppress_snapshot_until;

                // Accept project snapshot if data_version changed (unless drag in progress)
                if let Some(snapshot) = state.project_snapshot {
                    // Inspector drags (slider/trim/target/ADSR) are safe to accept
                    // snapshots through — handle_drag() writes the dragged value back
                    // to local_project in the same tick (via dispatch()), so the
                    // snapshot value is immediately overwritten. Accepting snapshots
                    // during inspector drag lets modulation-driven slider animations
                    // continue for non-dragged params.
                    //
                    // Overlay drags (clip move/trim in viewport) write clip positions
                    // directly via the host — those would be overwritten by the
                    // snapshot, so we still suppress for overlay drags.
                    if !drag_active && !suppressed {
                        let version_changed = state.data_version != self.content_state.data_version;
                        // Only deep-clone from Arc when it's a different allocation
                        // (new data_version). Modulation-only frames send the same
                        // Arc pointer — skip the clone (values are 1 frame stale,
                        // imperceptible).
                        let is_new_arc = self
                            .last_snapshot_arc
                            .as_ref()
                            .is_none_or(|prev| !std::sync::Arc::ptr_eq(prev, &snapshot));
                        if is_new_arc {
                            self.local_project = (*snapshot).clone();
                            self.last_snapshot_arc = Some(snapshot);
                        } else {
                            // Same Arc — skip deep clone. Drop the Arc ref.
                            drop(snapshot);
                        }
                        // Restore the one actively-dragged gesture so a snapshot
                        // doesn't overwrite the value the user is manipulating —
                        // every family, panel-wired and frame-resident (the
                        // bound-node-param card scrub, BUG-281, whose live writes
                        // land in `local_project` every tick, is now a
                        // `ResolvedScrub::BoundNodeParam` in `active`).
                        self.scrub.restore_dragged(&mut self.local_project);
                        // Clear suppression once we've accepted a post-load snapshot
                        self.suppress_snapshot_until = 0;

                        // Only trigger structural sync when data_version changed
                        // (editing commands, undo/redo). Modulation-only snapshots
                        // just update param_values — push_state() syncs sliders
                        // every frame without needing a structural rebuild.
                        if version_changed {
                            // Prune selection references to deleted clips/layers
                            let valid_clips: std::collections::HashSet<manifold_core::ClipId> =
                                self.local_project
                                    .timeline
                                    .layers
                                    .iter()
                                    .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                                    .collect();
                            let valid_layers: std::collections::HashSet<manifold_core::LayerId> =
                                self.local_project
                                    .timeline
                                    .layers
                                    .iter()
                                    .map(|l| l.layer_id.clone())
                                    .collect();
                            self.selection
                                .prune_stale_references(&valid_clips, &valid_layers);

                            // Validate active_layer_id
                            if let Some(ref id) = self.active_layer_id
                                && !valid_layers.contains(id)
                            {
                                self.active_layer_id = self
                                    .local_project
                                    .timeline
                                    .layers
                                    .last()
                                    .map(|l| l.layer_id.clone());
                            }

                            self.needs_structural_sync = true;
                            self.needs_rebuild = true;
                        }
                    }
                }
                // Apply lightweight modulation snapshot (param_values only)
                // to local_project — no full Project clone needed.
                if !drag_active
                    && !suppressed
                    && let Some(ref mod_snap) = state.modulation_snapshot
                {
                    mod_snap.apply(&mut self.local_project);
                    // Restore the one actively-dragged gesture so modulation
                    // doesn't overwrite the value the user is manipulating —
                    // every family, panel-wired and frame-resident (BUG-281's
                    // bound-node-param card scrub included, now in `active`).
                    self.scrub.restore_dragged(&mut self.local_project);
                }
                // Accumulate VQT columns from EVERY drained snapshot — the
                // assignment below keeps only the latest, so reading columns off
                // `content_state` would drop those from earlier snapshots when
                // several arrive in one UI frame, and re-push them on frames that
                // drain none. The render path consumes (clears) this buffer.
                self.pending_spectrogram_columns
                    .extend_from_slice(&state.spectrogram_columns);
                // Overlay records ride in lockstep (one ScopeColumn per column).
                self.pending_spectrogram_scalars
                    .extend_from_slice(&state.spectrogram_col_scalars);
                // Bound it: never keep more than one screen-width of columns (a
                // full sweep overwrites the rest anyway). 4096 = the max texture
                // width clamp below, so an open scope never drops a column it
                // could display; this only caps memory when the scope is closed
                // but an audio mod keeps capture — and column production —
                // running, since only the render path drains this buffer.
                let nb = state.spectrogram_num_bins;
                if nb > 0 {
                    const MAX_PENDING_COLS: usize = 4096;
                    let excess =
                        self.pending_spectrogram_columns.len().saturating_sub(MAX_PENDING_COLS * nb);
                    if excess > 0 {
                        self.pending_spectrogram_columns.drain(0..excess);
                        // Drop the matching overlay records (one per column).
                        // (Pre-ScopeColumn this used a hand-tracked stride and
                        // had drifted to a wrong literal, silently desyncing
                        // the overlay under overflow — the record type makes
                        // that unrepresentable.)
                        let cols = (excess / nb).min(self.pending_spectrogram_scalars.len());
                        self.pending_spectrogram_scalars.drain(0..cols);
                    }
                }
                self.content_state = ContentState {
                    project_snapshot: None,      // consumed above
                    modulation_snapshot: None,   // consumed above
                    spectrogram_columns: Vec::new(), // accumulated above
                    spectrogram_col_scalars: Vec::new(), // accumulated above
                    ..state
                };
            }
        }

        // 1a. Debounced background autosave (GIG_RESILIENCE_DESIGN §6). Runs
        // after the drain so it sees the latest data_version + dirty flag;
        // never reached in perform mode (early return above) — that IS the
        // D5 "autosave timer parks" behavior.
        self.tick_autosave();

        // 1a0b. Video-import probe-failure surfacing (BUG-133) — same
        // drain-site cadence as autosave; see `tick_import_failures`.
        self.tick_import_failures();
        // IMPORT_RESPONSIVENESS_DESIGN.md D3: drain the background
        // model-import worker's progress channel at the same per-frame site.
        self.drain_import_progress();

        // 1a1. Breadcrumb sidecar (GIG_RESILIENCE_DESIGN §5.1). Unlike
        // autosave this is NOT parked in perform mode — see the matching
        // call in `tick_perform_mode` (perform_mode/render.rs) for that path.
        self.tick_breadcrumb();

        // 1a2. One-shot crash notice (G10): the previous session exited
        // uncleanly. Shown after the first frames have painted so the dialog
        // sits over a real window, never on a perform surface.
        if self.show_crash_notice && self.frame_count >= 2 {
            self.show_crash_notice = false;
            let log_dir = std::env::var_os("HOME")
                .map(|h| format!("{}/Library/Logs/com.latentspace.manifold", h.to_string_lossy()))
                .unwrap_or_default();
            crate::alerts::info(
                "MANIFOLD crashed last session",
                &format!(
                    "Crash log + last autosave available.\n\nCrash logs: {log_dir}\nSnapshots: File → Revert to Snapshot"
                ),
            );
        }

        // 1b2. Drive per-clip audio-layer waveform decode/cache: gather the live
        // audio clips and let the cache background-decode any new ones, drain
        // finished peaks, and evict departed clips. The peaks are attached to
        // each ViewportClip on the next sync. See docs/AUDIO_LAYER_DESIGN.md.
        let audio_clips: Vec<(manifold_core::id::ClipId, String)> = self
            .local_project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio())
            .flat_map(|l| {
                l.clips
                    .iter()
                    .filter(|c| c.is_audio())
                    .map(|c| (c.id.clone(), c.audio_file_path.clone()))
            })
            .collect();
        // A decode that lands this frame must be attached now: the viewport clip
        // snapshot is only rebuilt on drag / structural change, so without this the
        // waveform would stay blank (and look like it "cleared" while scrolling)
        // until the next unrelated edit. Re-sync so the new renderer attaches; the
        // per-layer fingerprint (waveform.is_some()) then repaints the lane once.
        if self.ws.ui_root.audio_waveforms.poll_and_request(&audio_clips) {
            crate::ui_bridge::sync_clip_positions(
                &mut self.ws.ui_root,
                &self.local_project,
                self.selection.automation_mode_visible,
                &self.selection.chosen_automation_params,
            );
        }

        // 1c. Push the latest graph snapshot into the editor canvas
        // (read-only viewer of the running NodeGraphTestFX). Translate the
        // renderer snapshot into the UI view-model once (cached by Arc identity).
        // Per-node preview screens take the project aspect ratio, so a portrait
        // or wide show reads correctly on every node face. Set before
        // `set_snapshot` so the first layout of a level uses the right heights.
        let preview_aspect = self
            .content_pipeline_output
            .as_ref()
            .map(|p| p.get_dimensions())
            .filter(|(_, h)| *h > 0)
            .map(|(w, h)| w as f32 / h as f32);
        let ui_snap = self.editor_ui_snapshot();
        if let (Some(canvas), Some(ui_snap)) = (self.graph_canvas.as_mut(), ui_snap) {
            if let Some(aspect) = preview_aspect {
                canvas.set_preview_aspect(aspect);
            }
            // P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D17): the one per-frame
            // tick for the canvas's marquee-fade/connect-pop/error-shake
            // tweens — no seam existed for this before (graph_canvas had no
            // `tick`/`update` method at all); this is the natural insertion
            // point, right beside the `set_snapshot`/`apply_live_values`
            // calls that already run every frame the editor window is open,
            // using the `dt` this function already computed above.
            canvas.tick((dt * 1000.0) as f32);
            canvas.set_snapshot(&ui_snap);
            // Overlay this frame's live (modulated) node values on top of the
            // just-pushed structural snapshot, so a driver / Ableton / envelope /
            // card slider is seen moving each knob on the node face. Empty (and a
            // no-op) whenever no editor is watching the content side.
            canvas.apply_live_values(&self.content_state.live_node_params);
            // Tell the canvas whether the watched effect is diverged
            // from its bundled preset so the "Reset to Default" pill
            // appears in the header only when there's something to
            // revert. Polled each frame off `local_project`. Works
            // for both effect and generator targets.
            // "MODIFIED" must mean the graph diverges from its bundled preset
            // in a way that changes what it renders — NOT that a node was
            // nudged. Moving nodes materialises the per-instance override
            // (editor_pos has nowhere else to persist), so `graph.is_some()`
            // goes true after any drag. Compare against the cached catalog
            // default *ignoring layout* so the badge only lights on a real edit.
            let has_mod = self.watched_graph_target.as_ref().is_some_and(|target| {
                let instance_graph = match target {
                    manifold_core::GraphTarget::Effect(eid) => self
                        .local_project
                        .find_effect_by_id(eid)
                        .and_then(|fx| fx.graph.as_ref()),
                    manifold_core::GraphTarget::Generator(lid) => self
                        .local_project
                        .timeline
                        .find_layer_by_id(lid)
                        .and_then(|(_, l)| l.generator_graph()),
                };
                match (instance_graph, self.watched_catalog_default.as_ref()) {
                    // Diverged from the bundled preset beyond mere layout.
                    (Some(g), Some(base)) => g.diverges_ignoring_layout(base),
                    // Override present but no catalog base to compare against —
                    // can't prove it's layout-only, so treat as modified.
                    (Some(_), None) => true,
                    // Still on the catalog default: nothing to reset.
                    (None, _) => false,
                }
            });
            canvas.set_has_graph_mod(has_mod);
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
        }

        // 1d. Percussion import runs on content thread — read status from content_state.
        let was_importing = false; // previous frame state not tracked here
        let is_importing = self.content_state.percussion_importing;

        // 1e. Sync percussion pipeline status to header panel
        // Port of Unity WorkspaceController.RefreshPercussionImportStatusLabel
        {
            let msg = self.content_state.percussion_status_message.clone();
            let progress = self.content_state.percussion_progress;
            let show = self.content_state.percussion_show_progress && !msg.is_empty();
            self.ws.ui_root.header.set_import_status(
                &msg,
                if progress < 0.0 {
                    0.0
                } else {
                    progress.clamp(0.0, 1.0)
                },
                show,
            );
            // Force UI rebuild while pipeline is running (progress bar updates)
            // and on completion (new clips/layers need to appear).
            if is_importing {
                self.needs_rebuild = true;
            }
            if was_importing && !is_importing {
                // Pipeline just finished — structural sync to pick up new clips/layers.
                self.needs_structural_sync = true;
                self.needs_rebuild = true;
            }
        }

        // 1d2. Export progress (BUG-083) — the content thread's export loop
        // (content_export.rs's run_export/send_export_progress) blocks the
        // content thread and pushes a degraded ContentState every 10 frames;
        // read it the same way percussion import status is read above, so a
        // multi-minute export no longer looks like a hang.
        {
            let is_exporting = self.content_state.is_exporting;
            self.ws.ui_root.header.set_export_status(
                &self.content_state.export_status,
                self.content_state.export_progress,
                is_exporting,
            );
            // Keep redrawing the progress strip while exporting, same as
            // the percussion import bar above.
            if is_exporting {
                self.needs_rebuild = true;
            }
        }

        // 1e2. Sync live recording state to layer header record button.
        self.ws.ui_root.layer_headers.set_recording_active(
            &mut self.ws.ui_root.tree,
            self.content_state.is_live_recording,
        );
        // BUG-084/BUG-086: surface drop counters (video pool exhaustion +
        // native audio-encoder backpressure) on the same Record button.
        self.ws.ui_root.layer_headers.set_recording_drops(
            &mut self.ws.ui_root.tree,
            self.content_state.recording_dropped_frames,
            self.content_state.recording_dropped_audio_frames,
        );

        self.ui_profile.add("drain_state", seg.elapsed());
        seg = std::time::Instant::now();

        // 2. Process UI events and dispatch actions
        // Keep the Add-picker's embedded-preset list current (fingerprint-gated;
        // rebuilds only when a fork/import/remove changed the set).
        self.ws
            .ui_root
            .sync_embedded_presets(&self.local_project);
        let mut actions = self.ws.ui_root.process_events();

        // Overlay-hosted text sessions (main window): cancel any session
        // whose overlay just closed during the routing above — the app pump
        // half of `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3, D2. A no-op
        // most frames (empty drain).
        for id in self.ws.ui_root.take_closed_overlays() {
            self.text_input
                .cancel_if_owned_by(crate::text_input::TextSessionOwner::MainOverlay(id));
        }

        // Native menu bar clicks → the same PanelAction dispatch as on-screen
        // chrome. Drain into an owned Vec first so the immutable borrow of
        // `self.app_menu` ends before we touch `&mut self` below. File/View
        // items map onto existing PanelActions; Undo/Redo and Import/Settings
        // are handled directly (no PanelAction equivalent).
        let menu_actions = self
            .app_menu
            .as_ref()
            .map(crate::menu::AppMenu::drain)
            .unwrap_or_default();
        for ma in menu_actions {
            use crate::menu::MenuAction as M;
            use manifold_ui::panels::PanelAction as P;
            match ma {
                M::New => actions.push(P::Project(ProjectAction::NewProject)),
                M::Open => actions.push(P::Project(ProjectAction::OpenProject)),
                M::OpenRecentPath(path) => {
                    self.open_project_from_path(path);
                    self.needs_structural_sync = true;
                }
                M::ClearRecentProjects => {
                    self.project_io.clear_recent_projects(&mut self.user_prefs);
                    self.refresh_recent_menu();
                }
                M::Save => actions.push(P::Project(ProjectAction::SaveProject)),
                M::SaveAs => actions.push(P::Project(ProjectAction::SaveProjectAs)),
                M::RestoreSnapshot(hash) => {
                    if crate::alerts::confirm(
                        "Restore snapshot",
                        "Replace the current project state with this snapshot?\n\n\
                         The file on disk is untouched until the next save, and \
                         the replaced state is journaled to history on that save.",
                    ) {
                        self.restore_history_snapshot(&hash);
                    }
                }
                M::OpenSnapshotCopy(hash) => {
                    self.open_history_snapshot_copy(&hash);
                }
                M::ExportVideo => actions.push(P::Project(ProjectAction::ExportVideo)),
                M::ExportFrame => actions.push(P::Project(ProjectAction::ExportFrame)),
                M::Perform => actions.push(P::Project(ProjectAction::EnterPerformMode)),
                M::Monitor => actions.push(P::Project(ProjectAction::ToggleMonitor)),
                M::Audio => actions.push(P::Root(RootAction::OpenAudioSetup)),
                M::Scene => actions.push(P::Root(RootAction::OpenSceneSetup)),
                M::ImportVideo => self.import_video_clip(),
                M::Undo => {
                    if let Some(tx) = self.content_tx.as_ref() {
                        crate::ui_bridge::undo(tx);
                    }
                    // D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2):
                    // the real "Undo: <command name>" label now fires from
                    // `ui_bridge/state_sync.rs`'s `push_state`, once the
                    // content thread's `ContentState.undo_redo_event` round-
                    // trips back with the actual command description (see
                    // `content_commands.rs`'s `Undo`/`Redo` handlers and
                    // `ContentThread::pending_undo_redo_event`). No toast is
                    // fired here directly any more — that would show a
                    // generic label first and then get immediately replaced.
                }
                M::Redo => {
                    if let Some(tx) = self.content_tx.as_ref() {
                        crate::ui_bridge::redo(tx);
                    }
                }
                M::Settings => self.pending_open_settings = true,
            }
        }

        // Settings… (⌘, or the MANIFOLD menu) → open the floating settings popup.
        if std::mem::take(&mut self.pending_open_settings) {
            self.ws.ui_root.settings_popup.open();
            // Programmatic open: nudge the overlay driver to rebuild + draw it.
            self.ws.ui_root.overlay_dirty = true;
        }

        // An in-place inspector scroll (wheel in window_input, or a scrollbar
        // drag handled inside process_events) offset the content nodes without a
        // rebuild — re-render just the inspector's atlas slot. A full rebuild
        // later this frame (needs_rebuild → invalidate_all) supersedes it
        // harmlessly. One drain point for both scroll inputs. The actual
        // `invalidate_inspector()` call now lives in
        // `ui_frame::apply_ui_frame_invalidations` (P1, D3) — captured here as
        // a signal so it fires in the same relative order as before (ahead of
        // the rebuild/structural decision later this frame).
        let scrolled_in_place = self.ws.ui_root.inspector.take_scrolled_in_place();
        // Graph-editor edits (canvas + sidebar) accumulate here and dispatch
        // through their own command vocabulary (`GraphEditCommand`), separately
        // from the `PanelAction` loop — Phase 4.3.
        let mut graph_edits: Vec<manifold_ui::GraphEditCommand> = Vec::new();

        // Editor LEFT-LANE CARD actions are collected separately so they can be
        // dispatched against the editor's watched graph identity: they carry the
        // same PanelAction variants the inspector emits, but must resolve against
        // the edited effect/generator, not the main window's active layer.
        // Appended to `actions` after a recorded boundary so the dispatch loop
        // can tell which segment they live in.
        let mut editor_card_actions: Vec<manifold_ui::panels::PanelAction> = Vec::new();

        // 2a. Drain the graph-editor window's UITree events. The editor
        // doesn't go through `UIRoot::process_events` (its panel set is
        // a single `GraphEditorPanel`, not the full main-window mix), so
        // we route raw click events through the panel's own
        // `handle_click` to translate them into `PanelAction::EffectParamExpose`.
        // Resulting actions are appended to the main queue and dispatched
        // through the same `ui_bridge::dispatch` arms as everything else.
        if let Some(ed) = self.graph_editor.as_mut() {
            let events = ed.ui_root.input.drain_events();
            // When the node picker is open it's a modal — it claims every
            // click in the editor window (the backdrop spans the whole
            // surface). Route clicks to the popup and skip the palette +
            // sidebar handlers entirely so a click on a cell doesn't also
            // toggle a node behind it.
            if ed.ui_root.browser_popup.is_open() {
                use manifold_ui::input::UIEvent;
                use manifold_ui::panels::browser_popup::BrowserPopupAction;
                for event in events {
                    if let UIEvent::Click { node_id, .. } = event {
                        // Search bar → focus the search field (already
                        // auto-focused on open, but a click re-focuses).
                        if ed.ui_root.browser_popup.is_search_bar(node_id) {
                            let r = ed.ui_root.browser_popup.search_bar_rect(&ed.ui_root.tree);
                            self.text_input.begin_owned(
                                crate::text_input::TextSessionOwner::EditorOverlay(
                                    crate::ui_root::OverlayId::BrowserPopup,
                                ),
                                crate::text_input::TextInputField::SearchFilter,
                                ed.ui_root.browser_popup.current_filter(),
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                            ed.offscreen_dirty = true;
                        } else {
                            // This routes straight to `handle_click`, bypassing
                            // the overlay driver's `route_overlay_event` — so a
                            // close here (cell pick / backdrop) never reaches
                            // `route_overlay_event`'s closed-overlay tracking.
                            // Snapshot before/after and record it ourselves; the
                            // per-frame pump below drains it and cancels this
                            // popup's owned text session (no manual `cancel()`
                            // needed here — that was the distributed-reset
                            // pattern `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`
                            // §3 replaces).
                            let was_open = ed.ui_root.browser_popup.is_open();
                            if let Some(action) = ed.ui_root.browser_popup.handle_click(node_id) {
                                ed.ui_root.note_overlay_closed_if(
                                    crate::ui_root::OverlayId::BrowserPopup,
                                    was_open,
                                );
                                // Dismissed, or Effect/Generator/Paste (which never
                                // arise in Node mode from the editor popup), need
                                // nothing further here — the text-session cancel
                                // (if any) is handled by the closed-overlay pump.
                                if let BrowserPopupAction::NodeSelected { type_id, graph_pos } =
                                    action
                                {
                                    // Hand off to the layer-2 spawn handler.
                                    // `graph_pos` is the palette-origin canvas
                                    // position captured at open — pass it
                                    // straight through, never recompute.
                                    graph_edits.push(manifold_ui::GraphEditCommand::AddGraphNodeAt {
                                        type_id,
                                        graph_pos,
                                    });
                                }
                                ed.offscreen_dirty = true;
                            } else if ed.ui_root.browser_popup.contains_node(node_id) {
                                ed.ui_root.note_overlay_closed_if(
                                    crate::ui_root::OverlayId::BrowserPopup,
                                    was_open,
                                );
                                // Internal click (category chip, background) —
                                // consume so it doesn't leak to the canvas.
                                ed.offscreen_dirty = true;
                            }
                        }
                    }
                }
            } else {
                use manifold_ui::input::UIEvent;
                // Right lane = the full inspector column (this window's own
                // `ws.ui_root.inspector`). The one non-inspector interaction left
                // here is the left preview pane's "Smart preview" auto-gain toggle
                // (a `GraphEditCommand`, not a `PanelAction`) — register its flip
                // on the button id captured during the last render and resolve it
                // off the raw events before the inspector router runs.
                if !events.is_empty() {
                    self.editor_sidebar_intents.clear();
                    if let Some(id) = self.editor_smart_preview_toggle_id {
                        self.editor_sidebar_intents.on(
                            id,
                            manifold_ui::intent::Gesture::Click,
                            manifold_ui::GraphEditCommand::SetNodePreviewNormalize(
                                !self.node_preview_normalize,
                            ),
                        );
                    }
                }
                for event in &events {
                    if let UIEvent::Click { node_id, .. } = event
                        && let Some(cmd) = self.editor_sidebar_intents.resolve(
                            &ed.ui_root.tree,
                            Some(*node_id),
                            manifold_ui::intent::Gesture::Click,
                        )
                    {
                        graph_edits.push(cmd);
                    }
                }
                // Route the rest through the shared inspector event path — the
                // same intents + handle_event + drag/card-reorder + dropdown
                // routing the main window's `process_events` uses — so tabs,
                // cards, chrome, macros, sliders and drags all work identically.
                // These actions dispatch against the EDITOR's UIRoot in the
                // trailing `editor_card_actions` segment below.
                editor_card_actions.extend(ed.ui_root.route_inspector_events(&events));
            }
            // Overlay-hosted text sessions (editor window): same pump as the
            // main window above, draining both the bespoke browser-popup
            // click path (marked via `note_overlay_closed_if` in the branch
            // above) and any close `route_inspector_events` observed through
            // the normal overlay driver (e.g. an inspector dropdown).
            for id in ed.ui_root.take_closed_overlays() {
                self.text_input
                    .cancel_if_owned_by(crate::text_input::TextSessionOwner::EditorOverlay(id));
            }
        }
        // 2b. Drain editor-canvas actions (wire-drag completions,
        // node-drag releases, delete-key requests). Bypasses the
        // UITree event path because the canvas owns its own pointer
        // state — see `GraphCanvas::drain_actions`.
        if let Some(canvas) = self.graph_canvas.as_mut() {
            graph_edits.extend(canvas.drain_edits());
            actions.extend(canvas.drain_popover_actions());
        }

        // The editor mapping popover (canvas on-node rows) emits the same
        // `EffectMapping*` actions the canvas popover does (range / scale / offset
        // / invert / curve), keyed by binding id and dispatched against the
        // editor's `watched_graph_target` in the inline arms below.
        actions.extend(self.editor_mapping_popover.drain_actions());

        // 2a. Route viewport tracks-area events through InteractionOverlay.
        // These events were stashed by process_events() because the overlay
        // needs &mut TimelineEditingHost which UIRoot can't provide.
        {
            let viewport_events = self.ws.ui_root.drain_viewport_events();
            if !viewport_events.is_empty() {
                // Sync modifier state to overlay (Unity reads Keyboard.current inline)
                self.overlay.set_modifiers(self.modifiers);
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate_layers,
                    &mut self.pre_drag_commands,
                );
                for event in &viewport_events {
                    use manifold_ui::input::UIEvent;
                    match event {
                        UIEvent::Click { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos,
                                modifiers.shift,
                                modifiers.ctrl || modifiers.command,
                                1,
                                false,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DoubleClick { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos,
                                modifiers.shift,
                                modifiers.ctrl || modifiers.command,
                                2,
                                false,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::RightClick { pos, .. } => {
                            self.overlay.on_pointer_click(
                                *pos,
                                false,
                                false,
                                1,
                                true,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DragBegin { origin, .. } => {
                            self.overlay.on_begin_drag(
                                *origin,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::Drag { pos, .. } => {
                            self.overlay.on_drag(
                                *pos,
                                &mut host,
                                &mut self.selection,
                                &mut self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { .. } => {
                            self.overlay.on_end_drag(&mut host);
                        }
                        _ => {}
                    }
                }

                // Drain actions generated by the host during overlay processing
                // (right-click context menus: ClipRightClicked, TrackRightClicked).
                actions.append(&mut host.pending_actions);
            }
        }

        // Overlay-generated right-click actions (TrackRightClicked, ClipRightClicked)
        // arrive AFTER process_events() has already run its try_open_dropdown pass.
        // Route them through the dropdown system now so context menus actually open.
        self.ws.ui_root.intercept_overlay_actions(&mut actions);

        // Update effect clipboard count for browser popup
        self.ws.ui_root.effect_clipboard_count = self.effect_clipboard.count();

        // Trigger Ableton re-discovery when the picker opens so it shows fresh data.
        if self.ws.ui_root.ableton_rediscovery_needed {
            self.ws.ui_root.ableton_rediscovery_needed = false;
            self.send_content_cmd(ContentCommand::AbletonRediscover);
        }

        // Consume deferred structural sync flag (set by keyboard shortcuts)
        let mut needs_structural_sync = self.needs_structural_sync;
        self.needs_structural_sync = false;
        let mut needs_resolution_resize = false;
        let prev_active_layer = self.active_layer_id.clone();
        let prev_sel_version = self.selection.selection_version;

        // Append the editor inspector's actions as a trailing segment, recording
        // where it starts. Actions at or past `editor_card_seg_start` were emitted
        // by the graph-editor window's inspector column and dispatch against the
        // EDITOR's own `UIRoot` (its inspector instance) in a second pass below;
        // everything before is main-window / sidebar and dispatches here.
        let editor_card_seg_start = actions.len();
        actions.extend(editor_card_actions);
        // The canvas's current view depth (a path of group ids; empty = root),
        // captured once so the per-node graph edits below target the level the
        // user is actually looking at when they're inside a group.
        let canvas_scope: Vec<u32> = self
            .graph_canvas
            .as_ref()
            .map(|c| c.scope_path().to_vec())
            .unwrap_or_default();

        for (action_idx, action) in actions.iter().enumerate().take(editor_card_seg_start) {
            // Intercept actions that need Application-level access
            match action {
                PanelAction::Root(RootAction::CopyOscAddress(addr)) => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(addr.clone());
                    }
                    continue;
                }
                PanelAction::Project(ProjectAction::ToggleLiveRecording) => {
                    if self.content_state.is_live_recording {
                        self.send_content_cmd(ContentCommand::StopLiveRecording);
                    } else {
                        let mut config =
                            manifold_recording::LiveRecordingConfig::default_to_desktop();
                        config.audio_device = self.ws.ui_root.selected_audio_input_device.clone();
                        self.send_content_cmd(ContentCommand::StartLiveRecording(Box::new(config)));
                    }
                    continue;
                }
                PanelAction::Project(ProjectAction::SetAudioInputDevice(name)) => {
                    let display = if name.is_empty() {
                        self.ws.ui_root.selected_audio_input_device = None;
                        "No audio input".to_string()
                    } else {
                        self.ws.ui_root.selected_audio_input_device = Some(name.clone());
                        name.clone()
                    };
                    self.ws
                        .ui_root
                        .layer_headers
                        .set_audio_device_name(&mut self.ws.ui_root.tree, &display);
                    continue;
                }
                PanelAction::Project(ProjectAction::ToggleMonitor) => {
                    self.pending_toggle_output = true;
                    continue;
                }
                PanelAction::Root(RootAction::OpenAudioSetup) => {
                    // Toggle the docked Audio Setup column (D1). The panel's
                    // `open` flag and the layout's `audio_setup_width` are the
                    // two halves of "docked": `open` gates build/update/draw,
                    // the width is the geometry `content_area()` subtracts. Keep
                    // them in lockstep — set the width from the NEW open state so
                    // this is a true toggle regardless of entry state. A
                    // structural sync then rebuilds the whole tree at the new
                    // geometry (preview + timeline shrink) and populates the
                    // panel's device/send list via sync_inspector_data. The
                    // toggle itself lives on UIRoot so the headless script
                    // harness reaches the same one via ui_bridge::dispatch.
                    self.ws.ui_root.toggle_audio_dock();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::Root(RootAction::OpenSceneSetup) => {
                    // Mirror of `OpenAudioSetup` above (SCENE_SETUP_PANEL_DESIGN
                    // D2) — same lockstep `open`/`scene_setup_width` toggle,
                    // same structural rebuild, same dual reachability (live app
                    // here, headless harness via `ui_bridge::dispatch`).
                    self.ws.ui_root.toggle_scene_dock();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::Root(RootAction::SceneSetupOpenGraphEditor(layer_id)) => {
                    // D7 "Open Graph Editor" empty state — same mechanism as
                    // `OpenGeneratorGraphEditor` below, addressed explicitly by
                    // the panel's own layer instead of `active_layer_id`.
                    self.watch_generator_graph(layer_id.clone());
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(layer_id, group_node_id, name)) => {
                    // P2 object-name click — same shape as
                    // `AudioSendLabelClicked` below: begin the shared inline
                    // text-input session anchored over the row's own name
                    // label. Commit routes to `RenameGroupCommand` addressed
                    // directly at the layer (no graph editor needs to be
                    // open — the panel is a fourth surface, not a canvas view).
                    if let Some(r) = self
                        .ws
                        .ui_root
                        .scene_setup_panel
                        .object_name_rect(&self.ws.ui_root.tree, *group_node_id)
                    {
                        self.text_input.scene_object_layer_id = Some(layer_id.clone());
                        self.text_input.begin(
                            crate::text_input::TextInputField::SceneObjectRename(*group_node_id),
                            name,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            11.0,
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::SceneSetupRenameLightClicked(layer_id, light_node_id, name)) => {
                    // P5 light-row/properties-header name click — same shape
                    // as `SceneSetupRenameObjectClicked` above, addressed by
                    // the light's own doc id (no group indirection).
                    if let Some(r) = self
                        .ws
                        .ui_root
                        .scene_setup_panel
                        .light_name_rect(&self.ws.ui_root.tree, *light_node_id)
                    {
                        self.text_input.scene_object_layer_id = Some(layer_id.clone());
                        self.text_input.begin(
                            crate::text_input::TextInputField::SceneLightRename(*light_node_id),
                            name,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            11.0,
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::OpenGeneratorGraphEditor) => {
                    // Ask the content thread to snapshot the active layer's
                    // generator graph and set the unified watched_graph_target
                    // so every downstream edit dispatches against the generator
                    // graph rather than an effect. Shared with selection-follows
                    // via `watch_generator_graph`; the cog additionally opens
                    // the window.
                    if let Some(lid) = self.active_layer_id.clone() {
                        self.watch_generator_graph(lid);
                    }
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::Root(RootAction::OpenGraphEditor(ei)) => {
                    // Resolve `ei` (effect index in the active inspector tab) to
                    // the effect's stable `EffectId`, then start snapshotting
                    // that specific instance's graph. Keyed by instance id — not
                    // type id — so two cards of the same effect type produce
                    // independent snapshots. `watched_graph_target` is the sole
                    // identity for every editor-card edit and the exposure panel,
                    // so clip-scoped effects are addressed with no positional
                    // fallback. Shared with selection-follows via
                    // `watch_effect_graph`; the cog additionally opens the window.
                    match self.resolve_effect_card_id(*ei) {
                        Some(eid) => self.watch_effect_graph(eid),
                        None => {
                            self.watched_graph_target = None;
                            self.watched_catalog_default = None;
                        }
                    }
                    self.pending_open_graph_editor = true;
                    continue;
                }
                // ── Graph-editor mutations moved to the `graph_edits` loop
                // below (Phase 4.3) — they're `GraphEditCommand` now, not
                // `PanelAction`. ──
                PanelAction::Root(RootAction::EffectMappingRangeSnapshot { binding_id }) => {
                    // Pre-drag (min, max) as the undo baseline AND the
                    // snapshot-stomp guard, folded into the one
                    // `ScrubState.active` slot (P-I). Frame-resident: dispatched
                    // here, not on the `PanelAction::Scrub` wire, because the
                    // commit reads the range back via `watched_reshape` (needs the
                    // editor context). Store-aware / kind-aware.
                    let snap = self.watched_reshape(binding_id).map(|(mn, mx, _, _)| (mn, mx));
                    if let (Some(t), Some((mn, mx))) = (self.mapping_target(), snap) {
                        self.scrub.check_single_active_on_begin("mapping-range");
                        self.scrub.active =
                            Some(crate::ui_bridge::scrub::ResolvedScrub::MappingRange {
                                target: t,
                                param_id: binding_id.to_string(),
                                baseline: (mn, mx),
                                live: (mn, mx),
                            });
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingRangeChanged {
                    binding_id,
                    min,
                    max,
                }) => {
                    // Track the in-flight range on the guard so a snapshot
                    // stomp restores the latest dragged value, not the pre-drag
                    // one (BUG-262).
                    if let Some(crate::ui_bridge::scrub::ResolvedScrub::MappingRange {
                        live, ..
                    }) = &mut self.scrub.active
                    {
                        *live = (*min, *max);
                    }
                    if let Some(t) = self.mapping_target() {
                        self.preview_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                min: Some(*min),
                                max: Some(*max),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingRangeCommit { binding_id }) => {
                    // Take the baseline out of `active` only if it holds THIS
                    // gesture's range guard (leave any other live gesture alone).
                    let baseline = if let Some(
                        crate::ui_bridge::scrub::ResolvedScrub::MappingRange { baseline, .. },
                    ) = &self.scrub.active
                    {
                        let b = *baseline;
                        self.scrub.active = None;
                        Some(b)
                    } else {
                        None
                    };
                    if let (Some((old_min, old_max)), Some(t)) = (baseline, self.mapping_target())
                        && let Some((new_min, new_max, _, _)) = self.watched_reshape(binding_id)
                        && ((old_min - new_min).abs() > f32::EPSILON
                            || (old_max - new_max).abs() > f32::EPSILON)
                    {
                        self.commit_mapping_with_reverse(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                min: Some(new_min),
                                max: Some(new_max),
                                ..Default::default()
                            },
                            BindingMappingEdit {
                                min: Some(old_min),
                                max: Some(old_max),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingLabel { binding_id, label }) => {
                    if let Some(t) = self.mapping_target() {
                        self.commit_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                label: Some(label.clone()),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingSection { binding_id, section }) => {
                    if let Some(t) = self.mapping_target() {
                        self.commit_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                section: Some(section.clone()),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingInvert { binding_id, invert }) => {
                    if let Some(t) = self.mapping_target() {
                        self.commit_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                invert: Some(*invert),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingCurve { binding_id, curve }) => {
                    if let Some(t) = self.mapping_target() {
                        self.commit_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                curve: Some(crate::ui_translate::macro_curve_to_core(*curve)),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingAffineSnapshot { binding_id }) => {
                    // Pre-drag (scale, offset) as the undo baseline + stomp guard,
                    // folded into `ScrubState.active` (P-I; frame-resident, same
                    // as the range gesture above).
                    let snap = self.watched_reshape(binding_id).map(|(_, _, sc, of)| (sc, of));
                    if let (Some(t), Some((sc, of))) = (self.mapping_target(), snap) {
                        self.scrub.check_single_active_on_begin("mapping-affine");
                        self.scrub.active =
                            Some(crate::ui_bridge::scrub::ResolvedScrub::MappingAffine {
                                target: t,
                                param_id: binding_id.to_string(),
                                baseline: (sc, of),
                                live: (sc, of),
                            });
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingAffineChanged {
                    binding_id,
                    scale,
                    offset,
                }) => {
                    if let Some(crate::ui_bridge::scrub::ResolvedScrub::MappingAffine {
                        live, ..
                    }) = &mut self.scrub.active
                    {
                        *live = (*scale, *offset);
                    }
                    if let Some(t) = self.mapping_target() {
                        self.preview_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                scale: Some(*scale),
                                offset: Some(*offset),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingAffineCommit { binding_id }) => {
                    let baseline = if let Some(
                        crate::ui_bridge::scrub::ResolvedScrub::MappingAffine { baseline, .. },
                    ) = &self.scrub.active
                    {
                        let b = *baseline;
                        self.scrub.active = None;
                        Some(b)
                    } else {
                        None
                    };
                    if let (Some((old_scale, old_offset)), Some(t)) = (baseline, self.mapping_target())
                        && let Some((_, _, new_scale, new_offset)) =
                            self.watched_reshape(binding_id)
                        && ((old_scale - new_scale).abs() > f32::EPSILON
                            || (old_offset - new_offset).abs() > f32::EPSILON)
                    {
                        self.commit_mapping_with_reverse(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                scale: Some(new_scale),
                                offset: Some(new_offset),
                                ..Default::default()
                            },
                            BindingMappingEdit {
                                scale: Some(old_scale),
                                offset: Some(old_offset),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::Root(RootAction::EffectMappingGotoNode { binding_id }) => {
                    // Read-only navigation: resolve the binding's stable NodeId
                    // from the live snapshot (outer routing → node handle → id)
                    // and centre the editor canvas on it. Same path as the
                    // card-label jump-to-node, triggered from the mapping drawer.
                    if let Some(ui_snap) = self.editor_ui_snapshot()
                        && let Some(node_id) =
                            crate::graph_canvas::resolve_card_param_node_id(&ui_snap, binding_id)
                        && let Some(canvas) = self.graph_canvas.as_mut()
                    {
                        canvas.focus_node(&ui_snap, &node_id);
                    }
                    continue;
                }
                PanelAction::Project(ProjectAction::EnterPerformMode) => {
                    self.perform.pending_enter = true;
                    continue;
                }
                PanelAction::Project(ProjectAction::SaveProject) => {
                    self.save_project();
                    continue;
                }
                PanelAction::Project(ProjectAction::SaveProjectAs) => {
                    self.save_project_as();
                    continue;
                }
                PanelAction::Project(ProjectAction::ExportVideo) => {
                    self.start_export();
                    continue;
                }
                PanelAction::Project(ProjectAction::ExportFrame) => {
                    self.export_frame();
                    continue;
                }
                PanelAction::Project(ProjectAction::OpenProject) => {
                    self.open_project();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::Project(ProjectAction::OpenRecent) => {
                    self.open_recent_project();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::Params(ParamsAction::PasteEffects) => {
                    // Browser popup paste button → route through same logic as Cmd+V
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    let target = match tab {
                        manifold_ui::InspectorTab::Master => {
                            manifold_editing::commands::effect_target::EffectTarget::Master
                        }
                        manifold_ui::InspectorTab::Layer
                        | manifold_ui::InspectorTab::Group
                        | manifold_ui::InspectorTab::Clip => {
                            let layer_id = self.active_layer_id.clone().unwrap_or_default();
                            manifold_editing::commands::effect_target::EffectTarget::Layer {
                                layer_id,
                            }
                        }
                    };
                    let effects_len = match tab {
                        manifold_ui::InspectorTab::Master => {
                            self.local_project.settings.master_effects.len()
                        }
                        manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .map(|e| e.len())
                            .unwrap_or(0),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .map(|c| c.effects.len())
                            .unwrap_or(0),
                    };
                    let clones = self.effect_clipboard.get_paste_clones();
                    for (offset, fx) in clones.into_iter().enumerate() {
                        // Fresh, independent copy: new EffectId + dropped hardware
                        // bindings. Drop group membership too — cross-chain paste,
                        // the source's group isn't in the destination chain.
                        let mut fx = fx.duplicated();
                        fx.group_id = None;
                        let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                            target.clone(),
                            fx,
                            effects_len + offset,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::Params(ParamsAction::BrowserSearchClicked) => {
                    let r = self
                        .ws
                        .ui_root
                        .browser_popup
                        .search_bar_rect(&self.ws.ui_root.tree);
                    self.text_input.begin_owned(
                        crate::text_input::TextSessionOwner::MainOverlay(
                            crate::ui_root::OverlayId::BrowserPopup,
                        ),
                        crate::text_input::TextInputField::SearchFilter,
                        self.ws.ui_root.browser_popup.current_filter(),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::Transport(TransportAction::BpmFieldClicked) => {
                    let bpm = Some(&self.local_project).map_or(120.0, |p| p.settings.bpm.0);
                    let r = if let Some(id) = self.ws.ui_root.transport.bpm_field_id() {
                        self.ws.ui_root.tree.get_bounds(id)
                    } else {
                        manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0)
                    };
                    self.text_input.begin(
                        crate::text_input::TextInputField::Bpm,
                        &format!("{:.1}", bpm),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        14.0,
                    );
                    continue;
                }
                PanelAction::Transport(TransportAction::FpsFieldClicked) => {
                    let fps = Some(&self.local_project).map_or(60.0, |p| p.settings.frame_rate);
                    let r = if let Some(id) = self.ws.ui_root.footer.fps_field_id() {
                        self.ws.ui_root.tree.get_bounds(id)
                    } else {
                        manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0)
                    };
                    self.text_input.begin(
                        crate::text_input::TextInputField::Fps,
                        &format!("{:.0}", fps),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::Root(RootAction::BeginParamTextInput {
                    target,
                    param_id,
                    anchor,
                    value,
                    min: _,
                    max: _,
                    whole_numbers,
                }) => {
                    // Prefill the box with the base (set) value, formatted as a
                    // plain number so editing in place stays parseable.
                    let initial = if *whole_numbers {
                        format!("{}", value.round() as i64)
                    } else {
                        format!("{:.3}", value)
                    };
                    self.text_input.begin(
                        crate::text_input::TextInputField::InspectorParam,
                        &initial,
                        crate::text_input::AnchorRect::new(
                            anchor.x,
                            anchor.y,
                            anchor.width,
                            anchor.height,
                        ),
                        11.0,
                    );
                    self.text_input.inspector_param = Some(crate::text_input::InspectorParamCtx {
                        target: target.clone(),
                        param_id: param_id.clone(),
                        whole_numbers: *whole_numbers,
                    });
                    continue;
                }
                PanelAction::Root(RootAction::SceneSetupBeginNumericTextInput {
                    layer_id,
                    scope_path,
                    node_doc_id,
                    param_id,
                    value,
                    cell_node_id,
                    degrees,
                }) => {
                    // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P4, D8/D10: same
                    // early-intercept shape as `BeginParamTextInput` above.
                    // The panel has no `&UITree` in `handle_event`, so the
                    // cell's anchor rect is resolved here from its own node
                    // id. D10: degrees rows prefill in degrees (the panel
                    // boundary is the ONLY place this conversion happens —
                    // the stored `value` stays radians).
                    let r = self.ws.ui_root.tree.get_bounds(*cell_node_id);
                    let display = if *degrees { value.to_degrees() } else { *value };
                    let initial = format!("{display:.3}");
                    self.text_input.begin(
                        crate::text_input::TextInputField::SceneNumericParam(*node_doc_id),
                        &initial,
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    self.text_input.scene_numeric_param =
                        Some(crate::text_input::SceneNumericParamCtx {
                            layer_id: layer_id.clone(),
                            scope_path: scope_path.clone(),
                            param_id: param_id.clone(),
                            degrees: *degrees,
                        });
                    continue;
                }
                PanelAction::Root(RootAction::AudioSendGainBeginTextInput(send_id, value, cell_node_id)) => {
                    // P4 audio-dock sibling of `SceneSetupBeginNumericTextInput`.
                    let r = self.ws.ui_root.tree.get_bounds(*cell_node_id);
                    let initial = format!("{value:.1}");
                    self.text_input.begin(
                        crate::text_input::TextInputField::AudioSendGainParam,
                        &initial,
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    self.text_input.audio_send_gain_param =
                        Some(crate::text_input::AudioSendGainParamCtx { send_id: send_id.clone() });
                    continue;
                }
                PanelAction::Root(RootAction::BeginDriverPeriodTextInput {
                    target,
                    param_id,
                    anchor,
                    value,
                }) => {
                    // Prefill with the current period in beats (whole numbers
                    // without a decimal), select-all so the first keystroke
                    // replaces it.
                    let initial = if (value.fract()).abs() < 1e-3 {
                        format!("{}", value.round() as i64)
                    } else {
                        format!("{value:.2}")
                    };
                    self.text_input.begin(
                        crate::text_input::TextInputField::DriverFreePeriod,
                        &initial,
                        crate::text_input::AnchorRect::new(
                            anchor.x,
                            anchor.y,
                            anchor.width,
                            anchor.height,
                        ),
                        11.0,
                    );
                    self.text_input.driver_free_period =
                        Some(crate::text_input::DriverFreePeriodCtx {
                            target: target.clone(),
                            param_id: param_id.clone(),
                        });
                    continue;
                }
                PanelAction::Layer(LayerAction::LayerDoubleClicked(id)) => {
                    // Open text input for layer rename. The action carries a
                    // stable LayerId, stored on `text_input.layer_id` and
                    // re-resolved to the live row at commit time (BUG-031) —
                    // `pos` here only sizes the anchor rect for THIS frame's
                    // overlay, a read-only, open-time-only use.
                    {
                        let project = &self.local_project;
                        if let Some((pos, layer)) = project.timeline.find_layer_by_id(id) {
                            let r = if let Some(nid) =
                                self.ws.ui_root.layer_headers.name_node_id(pos)
                            {
                                self.ws.ui_root.tree.get_bounds(nid)
                            } else {
                                manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0)
                            };
                            let name = layer.name.clone();
                            self.text_input.begin(
                                crate::text_input::TextInputField::LayerName,
                                &name,
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                            self.text_input.layer_id = Some(id.clone());
                        }
                    }
                    continue;
                }
                PanelAction::Marker(MarkerAction::MarkerDoubleClicked(marker_id_str)) => {
                    // Open text input for marker rename
                    let marker_id = manifold_core::MarkerId::new(marker_id_str.as_str());
                    if let Some(marker) = self.local_project.timeline.find_marker(&marker_id) {
                        let beat = marker.beat;
                        let name = marker.name.clone();
                        // Anchor to marker flag position in the ruler
                        let px = self.ws.ui_root.viewport.beat_to_pixel(beat);
                        let ruler = self.ws.ui_root.viewport.ruler_rect();
                        let flag_w = manifold_ui::color::MARKER_FLAG_WIDTH;
                        let r = crate::text_input::AnchorRect::new(
                            px + flag_w * 0.5 + 2.0,
                            ruler.y,
                            80.0,
                            manifold_ui::color::MARKER_FLAG_HEIGHT,
                        );
                        self.text_input.begin(
                            crate::text_input::TextInputField::MarkerName,
                            &name,
                            r,
                            9.0,
                        );
                        self.text_input.marker_id = Some(marker_id);
                    }
                    continue;
                }
                PanelAction::Clip(ClipAction::ClipBpmClicked) => {
                    // Open text input for clip recorded BPM editing.
                    // Unity: ClipInspector.OnBitmapBpmClicked → BitmapTextInput.BeginEdit
                    if let Some(clip_id) = &self.selection.primary_selected_clip_id {
                        let bpm_text = Some(&self.local_project)
                            .and_then(|p| {
                                p.timeline
                                    .layers
                                    .iter()
                                    .flat_map(|l| l.clips.iter())
                                    .find(|c| c.id == *clip_id)
                            })
                            .map(|c| {
                                if c.recorded_bpm > 0.0 {
                                    format!("{:.1}", c.recorded_bpm)
                                } else {
                                    "Auto".to_string()
                                }
                            })
                            .unwrap_or_else(|| "Auto".to_string());
                        let r = self
                            .ws
                            .ui_root
                            .inspector
                            .clip_chrome_mut()
                            .bpm_button_rect(&self.ws.ui_root.tree);
                        self.text_input.begin(
                            crate::text_input::TextInputField::ClipBpm,
                            &bpm_text,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            10.0,
                        );
                    }
                    continue;
                }
                PanelAction::Params(ParamsAction::GenStringParamClicked(sp_idx)) => {
                    // Open text input for a generator string param.
                    if let Some(gp) = self.ws.ui_root.inspector.gen_params()
                        && let Some(sp) = gp.string_param(*sp_idx)
                    {
                        let current = sp.value.clone();
                        if let Some(r) = gp.string_param_rect(&self.ws.ui_root.tree, *sp_idx) {
                            self.text_input.begin(
                                crate::text_input::TextInputField::GenStringParam(*sp_idx),
                                &current,
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                        }
                    }
                    continue;
                }
                PanelAction::Params(ParamsAction::GenStringParamDropdownClicked(sp_idx)) => {
                    // Open a dropdown for a string param (e.g. font selector).
                    if let Some(gp) = self.ws.ui_root.inspector.gen_params()
                        && let Some(sp) = gp.string_param(*sp_idx)
                    {
                        let key = sp.key.clone();
                        if let Some(r) = gp.string_param_rect(&self.ws.ui_root.tree, *sp_idx) {
                            // Typed (2b.11): each font carries its GenStringParamSelected.
                            let items: Vec<manifold_ui::panels::dropdown::DropdownItem> = if key
                                == "fontFamily"
                            {
                                manifold_renderer::text_rasterizer::TextRasterizer::available_font_families()
                                        .into_iter()
                                        .map(|name| manifold_ui::panels::dropdown::DropdownItem::new(&name)
                                            .with_action(PanelAction::Params(ParamsAction::GenStringParamSelected(*sp_idx, name.clone()))))
                                        .collect()
                            } else {
                                vec![]
                            };
                            if !items.is_empty() {
                                let trigger =
                                    manifold_ui::node::Rect::new(r.x, r.y, r.width, r.height);
                                self.ws.ui_root.open_dropdown_typed(items, trigger);
                            }
                        }
                    }
                    continue;
                }
                PanelAction::Root(RootAction::AudioSendLabelClicked(send_id)) => {
                    if let Some(send) = self.local_project.audio_setup.find_send(send_id)
                        && let Some(r) = self
                            .ws
                            .ui_root
                            .audio_setup_panel
                            .send_label_rect(&self.ws.ui_root.tree, send_id)
                    {
                        let label = send.label.clone();
                        self.text_input.audio_send_id = Some(send_id.clone());
                        self.text_input.begin(
                            crate::text_input::TextInputField::AudioSendLabel,
                            &label,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            11.0,
                        );
                    }
                    continue;
                }
                PanelAction::Params(ParamsAction::MacroLabelRename(idx)) => {
                    if let Some(slot) = self.local_project.settings.macro_bank.slots.get(*idx)
                        && let Some(r) = self
                            .ws
                            .ui_root
                            .inspector
                            .macro_label_rect(&self.ws.ui_root.tree, *idx)
                    {
                        self.text_input.begin(
                            crate::text_input::TextInputField::MacroLabel(*idx),
                            &slot.label,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            11.0,
                        );
                    }
                    continue;
                }
                PanelAction::Project(ProjectAction::NewProject) => {
                    let action = self.project_io.new_project();
                    self.apply_project_io_action(action);
                    needs_structural_sync = true;
                    continue;
                }
                // Transport controller actions — intercept here for Application-level access
                // CycleClockAuthority removed — authority is auto-determined from enabled sources
                PanelAction::Transport(TransportAction::ToggleLink) => {
                    self.send_content_cmd(ContentCommand::ToggleLink);
                    continue;
                }
                PanelAction::Transport(TransportAction::ToggleMidiClock) => {
                    self.send_content_cmd(ContentCommand::ToggleMidiClock);
                    continue;
                }
                PanelAction::Transport(TransportAction::ToggleSyncOutput) => {
                    self.send_content_cmd(ContentCommand::ToggleOscSyncMode);
                    continue;
                }
                PanelAction::Transport(TransportAction::SetMidiClockDevice(index)) => {
                    self.send_content_cmd(ContentCommand::SetMidiClockDevice(*index));
                    continue;
                }
                PanelAction::Transport(TransportAction::ResetBpm) => {
                    self.send_content_cmd(ContentCommand::ResetBpm);
                    self.needs_rebuild = true;
                    continue;
                }
                // ── Selection-follows ──────────────────────────────────────
                // Clicking a card in the main inspector retargets an ALREADY
                // OPEN graph editor to that card's graph, so the editor surface
                // tracks the selection ("click an effect → you're on its
                // graph"). The cog (OpenGraphEditor / OpenGeneratorGraphEditor)
                // still owns OPENING the window; these arms only retarget. No
                // `continue` — fall through to `ui_bridge::dispatch` so the
                // card's own selection visuals still apply. When no editor is
                // open, this is a no-op and opening stays a deliberate cog
                // action (keeps the authoring/perform boundary intact).
                //
                // Gated to the MAIN-window segment (`action_idx <
                // editor_card_seg_start`): the editor's own card lane emits the
                // same two actions, and resolving those against the main
                // inspector's tab/selection would retarget to the wrong graph.
                PanelAction::Params(ParamsAction::EffectCardClicked(ei)) => {
                    if action_idx < editor_card_seg_start
                        && self.graph_editor_window_id.is_some()
                        && let Some(eid) = self.resolve_effect_card_id(*ei)
                    {
                        self.watch_effect_graph(eid);
                    }
                }
                PanelAction::Params(ParamsAction::GenCardClicked) => {
                    if action_idx < editor_card_seg_start
                        && self.graph_editor_window_id.is_some()
                        && let Some(lid) = self.active_layer_id.clone()
                    {
                        self.watch_generator_graph(lid);
                    }
                }
                _ => {}
            }
            let content_tx = self.content_tx.as_ref().unwrap();
            let mut dctx = crate::ui_bridge::DispatchCtx {
                project: &mut self.local_project,
                content_tx,
                content_state: &self.content_state,
                ui: &mut self.ws.ui_root,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer_id,
                user_prefs: &mut self.user_prefs,
                editor_target: None,
                scrub: &mut self.scrub,
            };
            let result = crate::ui_bridge::dispatch(action, &mut dctx);
            if result.structural_change {
                needs_structural_sync = true;
            }
            if result.resolution_changed {
                needs_resolution_resize = true;
            }
            if let Some((kind, def, destination)) = result.begin_save_preset {
                self.begin_save_preset_prompt(kind, def, destination);
            }
            if let Some((kind, id, source, initial_name)) = result.begin_rename_preset {
                self.begin_rename_preset_prompt(kind, id, source, initial_name);
            }
        }

        // ── Editor inspector segment ────────────────────────────────────────
        // The graph-editor window hosts its OWN inspector instance
        // (`ed.ui_root.inspector`), mirroring the main window's selection /
        // active-layer. Its actions dispatch against the editor's UIRoot with
        // `editor_target = None` (mirror) so param edits resolve identically and
        // only the editor's transient tree visuals (collapse, selection
        // highlight, card-drag) land on the editor tree. Card clicks additionally
        // retarget the canvas to that card's graph. Card-click retargets are
        // collected and applied after the editor-workspace borrow drops (they call
        // `self.watch_*`). See docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md.
        if actions.len() > editor_card_seg_start {
            let mut retarget_effect: Option<usize> = None;
            let mut retarget_generator = false;
            // Deferred like the retargets above: `self.begin_save_preset_prompt`
            // needs `&mut self`, which would conflict with `ed`'s live borrow of
            // `self.graph_editor` for the loop's duration.
            let mut pending_save_preset: Option<(
                manifold_core::preset_def::PresetKind,
                manifold_core::effect_graph_def::EffectGraphDef,
                crate::text_input::SavePresetDestination,
            )> = None;
            // BUG-121 root fix: the mapping-drawer chevron (Author-context
            // cards only, now that the editor's inspector carries
            // `CardContext::Author`) resolves to `OpenCardMapping`, but
            // nothing ever opened the popover it names — `ui_bridge::
            // dispatch` just marks it handled as a no-op. Resolve the
            // watched target's current reshape here, before `ed` borrows
            // `self.graph_editor` mutably (`watched_full_reshape` needs
            // `&self`); the loop below anchors it off the clicked card's
            // own chevron rect and actually opens the popover.
            let pending_mapping_open = actions[editor_card_seg_start..]
                .iter()
                .find_map(|a| match a {
                    PanelAction::Root(RootAction::OpenCardMapping(pid)) => {
                        Some((pid.to_string(), self.watched_full_reshape(pid.as_ref())))
                    }
                    _ => None,
                });
            let (screen_w, screen_h) = self
                .graph_editor_window_id
                .and_then(|wid| self.window_registry.get(&wid))
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                })
                .unwrap_or((1280.0, 720.0));
            if let Some(ed) = self.graph_editor.as_mut() {
                let content_tx = self.content_tx.as_ref().unwrap();
                for action in &actions[editor_card_seg_start..] {
                    match action {
                        PanelAction::Params(ParamsAction::EffectCardClicked(ei)) => retarget_effect = Some(*ei),
                        PanelAction::Params(ParamsAction::GenCardClicked) => retarget_generator = true,
                        PanelAction::Root(RootAction::OpenCardMapping(param_id)) => {
                            if let Some((_, Some((label, min, max, invert, curve, scale, offset)))) =
                                pending_mapping_open
                                    .as_ref()
                                    .filter(|(pid, _)| pid == param_id.as_ref())
                                && let Some(anchor) = ed
                                    .ui_root
                                    .inspector
                                    .mapping_chevron_rect(&ed.ui_root.tree, param_id.as_ref())
                            {
                                self.editor_mapping_popover.open(
                                    param_id.to_string(),
                                    label.clone(),
                                    *min,
                                    *max,
                                    *invert,
                                    crate::ui_translate::macro_curve_to_ui(*curve),
                                    *scale,
                                    *offset,
                                    None,
                                    None,
                                    manifold_ui::graph_canvas::Rect::new(
                                        anchor.x,
                                        anchor.y,
                                        anchor.width,
                                        anchor.height,
                                    ),
                                    manifold_ui::graph_canvas::Rect::new(
                                        0.0, 0.0, screen_w, screen_h,
                                    ),
                                );
                            }
                        }
                        _ => {}
                    }
                    let mut dctx = crate::ui_bridge::DispatchCtx {
                        project: &mut self.local_project,
                        content_tx,
                        content_state: &self.content_state,
                        ui: &mut ed.ui_root,
                        selection: &mut self.selection,
                        active_layer: &mut self.active_layer_id,
                        user_prefs: &mut self.user_prefs,
                        editor_target: None,
                        scrub: &mut self.scrub,
                    };
                    let result = crate::ui_bridge::dispatch(action, &mut dctx);
                    if result.structural_change {
                        needs_structural_sync = true;
                    }
                    if result.resolution_changed {
                        needs_resolution_resize = true;
                    }
                    if result.begin_save_preset.is_some() {
                        pending_save_preset = result.begin_save_preset;
                    }
                }
            }
            if let Some((kind, def, destination)) = pending_save_preset {
                self.begin_save_preset_prompt(kind, def, destination);
            }
            // Retarget the canvas to the clicked card's graph (opening the window
            // stays a deliberate cog action, so only retarget when it's open).
            if self.graph_editor_window_id.is_some() {
                if let Some(ei) = retarget_effect {
                    if let Some(eid) = self.resolve_effect_card_id(ei) {
                        self.watch_effect_graph(eid);
                    }
                } else if retarget_generator
                    && let Some(lid) = self.active_layer_id.clone()
                {
                    self.watch_generator_graph(lid);
                }
            }
        }

        // ── Graph-editor edits (Phase 4.3) ──────────────────────────────────
        // The canvas + sidebar emit `GraphEditCommand` (their own vocabulary,
        // off the PanelAction god-enum). Translate each into the matching
        // `manifold_editing::commands::graph::*`, resolving the watched target +
        // catalog default + canvas scope here at the boundary — exactly what the
        // old PanelAction arms did. `canvas_scope` (computed above) is the level
        // the user is viewing (group depth). Each arm keeps `continue` (now
        // "next edit"); the loop body is the match alone.
        for cmd in &graph_edits {
            match cmd {
                manifold_ui::GraphEditCommand::AddGraphNode { type_id } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        // Drop below the auto-laid catalog row so the
                        // new node is visible without panning. Auto
                        // layout uses (60,60) origin + (220,130)
                        // spacing, so y≈350 sits one row below the
                        // typical 4-node Mirror chain. The user drags
                        // it into place from there.
                        let drop_pos = (300.0, 350.0);
                        let cmd = manifold_editing::commands::graph::AddGraphNodeCommand::new(
                            eid.clone(),
                            type_id.clone(),
                            Some(drop_pos),
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                // Open the node picker over the editor canvas. This is the
                // editor window's OWN BrowserPopupPanel (`graph_editor.ui_root
                // .browser_popup`), not the main window's — same widget, its
                // own tree and input path. `screen_pos` anchors the popup in
                // editor-window logical pixels; `graph_pos` (captured against
                // the palette-origin canvas viewport in graph_canvas) is
                // stashed on the popup and passed straight back out on
                // selection so the spawned node lands under the cursor.
                manifold_ui::GraphEditCommand::OpenNodePicker {
                    screen_pos,
                    graph_pos,
                } => {
                    use manifold_renderer::node_graph::{Category, descriptor_for};
                    use manifold_ui::panels::browser_popup::*;
                    use manifold_ui::panels::picker_core::PickerItem;

                    // Editor-window logical size — drives the popup's
                    // edge-clamping. Falls back to a sane default if the
                    // window isn't registered yet (shouldn't happen with
                    // the editor open, but stay defensive).
                    let (screen_w, screen_h) = self
                        .graph_editor_window_id
                        .and_then(|wid| self.window_registry.get(&wid))
                        .map(|ws| {
                            let s = ws.window.scale_factor();
                            let sz = ws.window.inner_size();
                            (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                        })
                        .unwrap_or((1280.0, 720.0));

                    // Search haystack per item: the friendly label plus the
                    // descriptor's aliases (old names, plain-English, the
                    // TouchDesigner-equivalent operator). Typing "blur top"
                    // or a legacy name finds the node.
                    let items: Vec<PickerItem> = self
                        .palette_atoms_cache
                        .iter()
                        .map(|a| {
                            let aliases = descriptor_for(&a.type_id)
                                .map(|d| d.aliases.join(" "))
                                .unwrap_or_default();
                            let search_text = if aliases.is_empty() {
                                None
                            } else {
                                Some(format!("{} {}", a.label, aliases))
                            };
                            PickerItem {
                                label: a.label.clone(),
                                type_id: a.type_id.clone(),
                                category: Some(a.category.clone()),
                                search_text,
                                badge: None,
                                // Node mode has no source concept
                                // (PRESET_LIBRARY_DESIGN P5, D6) — the
                                // graph-editor's node picker never renders
                                // the source row or the management menu.
                                source: None,
                                missing_from_library: false,
                                // Node mode never has a thumbnail (only the
                                // Effect/Generator preset browser does).
                                thumbnail: None,
                            }
                        })
                        .collect();
                    let cat_names: Vec<String> =
                        Category::ALL.iter().map(|c| c.label().to_string()).collect();

                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root.browser_popup.set_screen_size(screen_w, screen_h);
                        ed.ui_root.browser_popup.open(BrowserPopupRequest {
                            mode: BrowserPopupMode::Node,
                            tab: manifold_ui::panels::InspectorTab::Master,
                            layer_id: None,
                            items,
                            category_names: cat_names,
                            spawn_graph_pos: Some(*graph_pos),
                            paste_count: 0,
                            screen_anchor: manifold_ui::Vec2::new(screen_pos.0, screen_pos.1),
                        });
                        ed.offscreen_dirty = true;
                    }
                    // Auto-focus the search field so the user types
                    // immediately. The popup tree isn't built yet (it builds
                    // next frame in present_graph_editor_window), so anchor
                    // the overlay at the click point; the field rect is
                    // cosmetic for the picker — keystrokes route by the
                    // active SearchFilter field, not by hit position.
                    self.text_input.begin_owned(
                        crate::text_input::TextSessionOwner::EditorOverlay(
                            crate::ui_root::OverlayId::BrowserPopup,
                        ),
                        crate::text_input::TextInputField::SearchFilter,
                        "",
                        crate::text_input::AnchorRect::new(
                            screen_pos.0,
                            screen_pos.1,
                            200.0,
                            24.0,
                        ),
                        11.0,
                    );
                    continue;
                }
                manifold_ui::GraphEditCommand::AddGraphNodeAt { type_id, graph_pos } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::AddGraphNodeCommand::new(
                            eid.clone(),
                            type_id.clone(),
                            Some(*graph_pos),
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::ConnectPorts {
                    from_node,
                    from_port,
                    to_node,
                    to_port,
                } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::ConnectPortsCommand::new(
                            eid.clone(),
                            *from_node,
                            from_port.clone(),
                            *to_node,
                            to_port.clone(),
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::RevertEffectGraph => {
                    if let Some(eid) = self.watched_graph_target.as_ref() {
                        let cmd =
                            manifold_editing::commands::graph::RevertEffectGraphCommand::new(
                                eid.clone(),
                            );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::SaveGraphToLibrary { anchor }
                | manifold_ui::GraphEditCommand::SaveGraphToProject { anchor } => {
                    // Save to Library / Save to Project (PRESET_LIBRARY_DESIGN
                    // D4, P3), triggered from the graph editor header. The
                    // watched instance's CURRENT effective definition — its
                    // diverged `graph` if `Some`, else the catalog default —
                    // with the card's live slider values snapshotted in, same
                    // resolution `ui_bridge::inspector::preset_source_def`
                    // does for the card-menu path.
                    let destination = if matches!(cmd, manifold_ui::GraphEditCommand::SaveGraphToLibrary { .. }) {
                        crate::text_input::SavePresetDestination::Library
                    } else {
                        crate::text_input::SavePresetDestination::Project
                    };
                    if let Some(target) = self.watched_graph_target.clone()
                        && let Some(inst) = self.local_project.preset_instance(&target)
                        && let Some(mut def) = inst
                            .graph
                            .clone()
                            .or_else(|| self.watched_catalog_default.clone())
                    {
                        inst.snapshot_values_into_def(&mut def);
                        let kind = target.preset_kind();
                        self.text_input.begin(
                            crate::text_input::TextInputField::SavePresetName,
                            "",
                            crate::text_input::AnchorRect::new(anchor.0, anchor.1, anchor.2, anchor.3),
                            11.0,
                        );
                        self.text_input.save_preset = Some(crate::text_input::SavePresetCtx {
                            kind,
                            def,
                            destination,
                        });
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::PushGraphToLibrary { anchor } => {
                    // Push to Library (PRESET_LIBRARY_DESIGN D3, P4): only
                    // reachable while diverged (the header pill is gated on
                    // `has_graph_mod`), so the source is the instance's OWN
                    // diverged graph — no catalog-default fallback (there
                    // would be nothing meaningful to push).
                    if let Some(target) = self.watched_graph_target.clone()
                        && let Some(inst) = self.local_project.preset_instance(&target)
                        && let Some(mut def) = inst.graph.clone()
                    {
                        let preset_id = inst.effect_type().clone();
                        inst.snapshot_values_into_def(&mut def);
                        let kind = target.preset_kind();
                        let lib = crate::user_library::UserLibrary::new();
                        if lib.is_user_entry(kind, &preset_id) {
                            if let Err(e) = lib.push(kind, &preset_id, &def) {
                                log::error!("[preset] push to library failed: {e}");
                            }
                        } else {
                            // Factory/stock id — no file to overwrite; fall
                            // back to the same Save to Library (as new)
                            // prompt the header's own Save pill opens.
                            self.text_input.begin(
                                crate::text_input::TextInputField::SavePresetName,
                                "",
                                crate::text_input::AnchorRect::new(
                                    anchor.0, anchor.1, anchor.2, anchor.3,
                                ),
                                11.0,
                            );
                            self.text_input.save_preset = Some(crate::text_input::SavePresetCtx {
                                kind,
                                def,
                                destination: crate::text_input::SavePresetDestination::Library,
                            });
                        }
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::DisconnectPorts { to_node, to_port } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::DisconnectPortsCommand::new(
                            eid.clone(),
                            *to_node,
                            to_port.clone(),
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::RemoveGraphNode { node_id } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        // Which card sliders would this delete orphan? Detect
                        // against the live diverged graph if there is one, else
                        // the catalog default. If any, confirm before deleting —
                        // a node that backs card controls takes them with it.
                        let orphaned = {
                            let def = self
                                .local_project
                                .preset_instance(eid)
                                .and_then(|i| i.graph.as_ref())
                                .unwrap_or(default);
                            manifold_editing::commands::graph::exposed_param_labels_for_node(
                                def,
                                &canvas_scope,
                                *node_id,
                            )
                        };
                        let proceed =
                            orphaned.is_empty() || Self::confirm_remove_node_orphans(&orphaned);
                        if proceed {
                            let cmd =
                                manifold_editing::commands::graph::RemoveGraphNodeCommand::new(
                                    eid.clone(),
                                    *node_id,
                                    default.clone(),
                                )
                                .with_scope(canvas_scope.clone());
                            self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::MoveGraphNode { node_id, new_pos } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::MoveGraphNodeCommand::new(
                            eid.clone(),
                            *node_id,
                            *new_pos,
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::RelayoutGraph {
                    scope_path,
                    positions,
                } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::LayoutGraphNodesCommand::new(
                            eid.clone(),
                            positions.clone(),
                            default.clone(),
                        )
                        .with_scope(scope_path.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::SetGraphNodeParam {
                    node_id,
                    param_name,
                    new_value,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.clone(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        // D1 (`docs/PARAM_TWO_WAY_BINDING_DESIGN.md`): a
                        // card-bound param's graph slot is `apply_bindings`'
                        // stomp target — it re-writes the slot from the card
                        // every rebuild — so a node-face edit on a bound
                        // param must write the CARD param instead, through
                        // the inverse reshape, never the def slot directly
                        // (never dual-write). Step 1: the wired backstop
                        // (D5/D6 — wire beats binding; P2's input-layer
                        // scrub prevention is the primary defense, this is
                        // the enforcement debug_assert). Step 2: bound →
                        // reroute. Step 3: unbound → existing path,
                        // unchanged.
                        let wired =
                            self.watched_node_param_is_wired(&canvas_scope, *node_id, param_name);
                        debug_assert!(
                            !wired,
                            "node-face scrub started on a wired param row — P2's \
                             input-layer prevention should have blocked this before \
                             it reached dispatch"
                        );
                        let bound = if wired {
                            None
                        } else {
                            self.watched_binding_for_node_param(&canvas_scope, *node_id, param_name)
                        };
                        if let Some((outer_id, min, max, invert, curve, scale, offset)) = bound {
                            let core_value =
                                crate::ui_translate::serialized_param_value_to_core(new_value);
                            if let Some(gesture_value) = serialized_value_as_f32(&core_value)
                                && let Some(card_value) = manifold_core::effects::invert_card_reshape(
                                    gesture_value,
                                    min,
                                    max,
                                    invert,
                                    curve,
                                    scale,
                                    offset,
                                )
                            {
                                let is_new_session = !matches!(
                                    &self.scrub.active,
                                    Some(crate::ui_bridge::scrub::ResolvedScrub::BoundNodeParam(d))
                                        if d.target == target
                                            && d.node_id == *node_id
                                            && d.param_name == *param_name
                                );
                                if is_new_session {
                                    self.scrub.check_single_active_on_begin("bound-node-param");
                                    let old_value = self
                                        .local_project
                                        .with_preset_graph_mut(&target, |inst| {
                                            inst.get_base_param(&outer_id)
                                        })
                                        .unwrap_or(card_value);
                                    self.scrub.active = Some(
                                        crate::ui_bridge::scrub::ResolvedScrub::BoundNodeParam(
                                            Box::new(BoundNodeParamDrag {
                                                target: target.clone(),
                                                node_id: *node_id,
                                                param_name: param_name.clone(),
                                                outer_param_id: outer_id.clone(),
                                                old_value,
                                                current_value: old_value,
                                            }),
                                        ),
                                    );
                                }
                                if let Some(
                                    crate::ui_bridge::scrub::ResolvedScrub::BoundNodeParam(drag),
                                ) = self.scrub.active.as_mut()
                                {
                                    drag.current_value = card_value;
                                }
                                // Live write — the same arms
                                // `PanelAction::ParamChanged` uses
                                // (`ui_bridge/inspector.rs`): mutate the
                                // local mirror synchronously (so the
                                // card slider follows every move) and
                                // push a cheap `MutateProjectLive` for
                                // the render — no undo-stack entry here,
                                // that's `EndGraphNodeParamScrub`'s job.
                                self.local_project.with_preset_graph_mut(&target, |inst| {
                                    inst.set_base_param(&outer_id, card_value);
                                });
                                let t = target.clone();
                                let oid = outer_id.clone();
                                self.send_content_cmd(ContentCommand::MutateProjectLive(
                                    Box::new(move |p| {
                                        p.with_preset_graph_mut(&t, |inst| {
                                            inst.set_base_param(&oid, card_value);
                                        });
                                    }),
                                ));
                            }
                            // else: degenerate scale (D1 §3) — read-only, no
                            // write; the row keeps showing the bound badge.
                            continue;
                        }
                        // BUG-282: mirror the bound arm above — one session
                        // opened on the first move, live-written every tick
                        // via `MutateProjectLive` (no undo entry), committed
                        // as ONE undo-worthy `Execute` on
                        // `EndGraphNodeParamScrub` with the true pre-drag
                        // value seeded through `with_previous` (graph.rs
                        // `SetGraphNodeParamCommand::with_previous` doc).
                        let core_value =
                            crate::ui_translate::serialized_param_value_to_core(new_value);
                        let is_new_session = !matches!(
                            &self.scrub.active,
                            Some(crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(d))
                                if d.target == target
                                    && d.node_id == *node_id
                                    && d.param_name == *param_name
                                    && d.scope_path == canvas_scope
                        );
                        if is_new_session {
                            self.scrub.check_single_active_on_begin("unbound-node-param");
                            let pre_drag_value = self.watched_current_node_param_value(
                                &canvas_scope,
                                *node_id,
                                param_name,
                                default,
                            );
                            self.scrub.active = Some(
                                crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(
                                    Box::new(UnboundNodeParamDrag {
                                        target: target.clone(),
                                        node_id: *node_id,
                                        param_name: param_name.clone(),
                                        scope_path: canvas_scope.clone(),
                                        catalog_default: default.clone(),
                                        pre_drag_value,
                                        current_value: core_value.clone(),
                                    }),
                                ),
                            );
                        }
                        if let Some(
                            crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(drag),
                        ) = self.scrub.active.as_mut()
                        {
                            drag.current_value = core_value.clone();
                        }
                        let mut live_cmd =
                            manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                                target,
                                *node_id,
                                param_name.clone(),
                                core_value,
                                default.clone(),
                            )
                            .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::MutateProjectLive(Box::new(
                            move |p| {
                                live_cmd.execute(p);
                            },
                        )));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::EndGraphNodeParamScrub { node_id, param_name } => {
                    // Close out whichever node-param gesture `active` holds — a
                    // bound (D1) or unbound (BUG-282) session — with ONE
                    // undo-worthy command for the whole drag. A foreign gesture
                    // (or none) is left in place; a matching-but-unmoved one is
                    // dropped without a command.
                    match self.scrub.active.take() {
                        Some(crate::ui_bridge::scrub::ResolvedScrub::BoundNodeParam(drag)) => {
                            if drag.node_id == *node_id
                                && drag.param_name == *param_name
                                && (drag.old_value - drag.current_value).abs() > f32::EPSILON
                            {
                                let cmd =
                                    manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                                        drag.target,
                                        drag.outer_param_id,
                                        drag.old_value,
                                        drag.current_value,
                                    );
                                self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                            }
                        }
                        Some(crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(drag)) => {
                            // Seeded with the true pre-drag value via
                            // `with_previous` so undo restores it (not the
                            // post-drag value `execute()`'s self-capture would
                            // otherwise see). No-op if the drag never touched this
                            // `(node_id, param_name)`, or never actually moved.
                            if drag.node_id == *node_id && drag.param_name == *param_name {
                                let moved = match &drag.pre_drag_value {
                                    Some(prev) => *prev != drag.current_value,
                                    // Key was absent before the drag — inserting
                                    // it now is always a real change.
                                    None => true,
                                };
                                if moved {
                                    let cmd =
                                        manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                                            drag.target,
                                            drag.node_id,
                                            drag.param_name,
                                            drag.current_value,
                                            drag.catalog_default,
                                        )
                                        .with_scope(drag.scope_path)
                                        .with_previous(drag.pre_drag_value);
                                    self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                                }
                            }
                        }
                        // Not a node-param gesture — EndScrub shouldn't fire
                        // against a panel/mapping/marker gesture; leave it live.
                        other => self.scrub.active = other,
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::SetOuterParam {
                    outer_param_id,
                    new_value,
                } => {
                    // D6 parity invariant (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md`
                    // §2): a group-face row mirrors an already-exposed card
                    // param, so it re-dispatches through the IDENTICAL
                    // `PanelAction::ParamChanged` handler the card's own
                    // slider uses (`ui_bridge/inspector.rs`) — never a
                    // second write path. `idx` in `GraphParamTarget::Effect`
                    // is a don't-care here: passing the editor's own
                    // `watched_graph_target` explicitly as `editor_target`
                    // makes `resolve_effect_id` resolve by stable id before
                    // it ever consults `idx` (its early-return branch).
                    if let Some(target) = self.watched_graph_target.as_ref() {
                        let gpt = match target {
                            manifold_core::GraphTarget::Effect(_) => {
                                manifold_ui::panels::GraphParamTarget::Effect(0)
                            }
                            manifold_core::GraphTarget::Generator(_) => {
                                manifold_ui::panels::GraphParamTarget::Generator
                            }
                        };
                        let action = PanelAction::Scrub(
                            ValueRef::Param(
                                gpt,
                                manifold_core::effects::ParamId::from(outer_param_id.clone()),
                            ),
                            ScrubPhase::Move(ScrubValue::Scalar(*new_value)),
                        );
                        let content_tx = self.content_tx.as_ref().unwrap();
                        let editor_target = self.watched_graph_target.as_ref();
                        let mut dctx = crate::ui_bridge::DispatchCtx {
                            project: &mut self.local_project,
                            content_tx,
                            content_state: &self.content_state,
                            ui: &mut self.ws.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_id,
                            user_prefs: &mut self.user_prefs,
                            editor_target,
                            scrub: &mut self.scrub,
                        };
                        let _ = crate::ui_bridge::dispatch(&action, &mut dctx);
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::BrowseGraphNodePath { node_id, param_name } => {
                    // Blocking native picker — fine for authoring (same as preset
                    // import/export). `is_path_param` (graph_canvas/model.rs)
                    // groups "folder"/"path"/"file"/"dir" together to decide
                    // whether a click opens a browser at all, but the browser
                    // kind still has to match the param's actual shape: only a
                    // "folder"/"dir"-named param (e.g. node.image_folder's
                    // `folder`) wants a directory picker — a "path"/"file"-named
                    // param (node.hdri_source, node.gltf_texture_source, …)
                    // names one file on disk, and `pick_folder()` can't select a
                    // file at all (the bug behind not being able to pick an .exr
                    // for the HDRI node's `path` param). On a pick, set the param
                    // to the path through the same command SetGraphNodeParam uses.
                    let wants_folder = {
                        let n = param_name.to_ascii_lowercase();
                        n.contains("folder") || n.contains("dir")
                    };
                    let picked = if wants_folder {
                        rfd::FileDialog::new().pick_folder()
                    } else {
                        rfd::FileDialog::new().pick_file()
                    };
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) && let Some(folder) = picked
                    {
                        let path = folder.to_string_lossy().to_string();
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            eid.clone(),
                            *node_id,
                            param_name.clone(),
                            manifold_core::effect_graph_def::SerializedParamValue::String {
                                value: path,
                            },
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::EditGraphNodeStringParam {
                    node_id,
                    param_name,
                    current,
                    anchor,
                } => {
                    // Open the inline editor over the value cell. The param name
                    // (not `Copy`) rides on the text-input state; commit routes
                    // through SetGraphNodeParamCommand with a String value.
                    self.text_input.begin(
                        crate::text_input::TextInputField::GraphStringParam(*node_id),
                        current,
                        crate::text_input::AnchorRect::new(
                            anchor.0,
                            anchor.1,
                            anchor.2.max(120.0),
                            anchor.3,
                        ),
                        12.0,
                    );
                    self.text_input.graph_param_name = Some(param_name.clone());
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::EditGraphNodeWgsl {
                    node_id,
                    current,
                    anchor: _,
                } => {
                    // The kernel editor is multiline and large — anchor it over
                    // the canvas (top-left) rather than the small sidebar button.
                    let anchor = self
                        .editor_canvas_viewport()
                        .map(|vp| {
                            crate::text_input::AnchorRect::new(
                                vp.x + 24.0,
                                40.0,
                                (vp.w - 48.0).max(240.0),
                                22.0,
                            )
                        })
                        .unwrap_or_else(|| {
                            crate::text_input::AnchorRect::new(360.0, 40.0, 520.0, 22.0)
                        });
                    self.text_input.begin(
                        crate::text_input::TextInputField::GraphWgsl(*node_id),
                        current,
                        anchor,
                        12.0,
                    );
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::EditGraphNodeNumericParam {
                    node_id,
                    param_name,
                    current,
                    min,
                    max,
                    whole_numbers,
                    outer_param_id,
                    anchor,
                } => {
                    // The contract's `(ValueCell, DoubleClick) -> EditValue`
                    // row going live on the canvas (P5d) — same anchor +
                    // prefill convention as the inspector sidebar's
                    // `BeginParamTextInput` (InspectorParam).
                    let initial = if *whole_numbers {
                        format!("{}", current.round() as i64)
                    } else {
                        format!("{:.3}", current)
                    };
                    self.text_input.begin(
                        crate::text_input::TextInputField::GraphNumericParam(*node_id),
                        &initial,
                        crate::text_input::AnchorRect::new(
                            anchor.0, anchor.1, anchor.2, anchor.3,
                        ),
                        11.0,
                    );
                    self.text_input.graph_numeric_param = Some(crate::text_input::GraphNumericParamCtx {
                        param_name: param_name.clone(),
                        min: *min,
                        max: *max,
                        whole_numbers: *whole_numbers,
                        outer_param_id: outer_param_id.clone(),
                    });
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::EditGraphNodeTableCell {
                    node_id,
                    param_name,
                    row,
                    col,
                    current,
                    rows,
                    anchor,
                } => {
                    // Open the inline numeric editor over the cell; stash the
                    // whole table so commit can rebuild just this cell.
                    self.text_input.begin(
                        crate::text_input::TextInputField::GraphTableCell,
                        &fmt_table_cell_seed(*current),
                        crate::text_input::AnchorRect::new(
                            anchor.0,
                            anchor.1,
                            anchor.2.max(48.0),
                            anchor.3,
                        ),
                        12.0,
                    );
                    self.text_input.graph_table_edit = Some(crate::text_input::TableCellEdit {
                        node_id: *node_id,
                        param_name: param_name.clone(),
                        row: *row,
                        col: *col,
                        rows: rows.clone(),
                    });
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::GroupSelection {
                    scope_path,
                    node_ids,
                    handle,
                    centroid,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::GroupNodesCommand::new(
                            target.clone(),
                            scope_path.clone(),
                            node_ids.clone(),
                            handle.clone(),
                            *centroid,
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::Ungroup {
                    scope_path,
                    group_id,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::UngroupNodeCommand::new(
                            target.clone(),
                            scope_path.clone(),
                            *group_id,
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::SetGroupTint {
                    scope_path,
                    group_id,
                    tint,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::SetGroupTintCommand::new(
                            target.clone(),
                            scope_path.clone(),
                            *group_id,
                            *tint,
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::ToggleNodeParamExpose {
                    node_id,
                    node_u32_id,
                    node_handle,
                    inner_param,
                    expose,
                    label,
                    min,
                    max,
                    default_value,
                    convert,
                    is_angle,
                    value_labels,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        // Address the node exactly like every other graph command:
                        // the canvas scope (current view depth) plus the node's
                        // u32 doc id, so `descend_level` reaches a node nested in
                        // a group. Matching by the stable `node_id` alone failed
                        // — it's empty on bundled-preset nodes and the old command
                        // only scanned the top level.
                        let cmd =
                            manifold_editing::commands::graph::ToggleNodeParamExposeCommand::new(
                                target.clone(),
                                node_id.clone(),
                                *node_u32_id,
                                node_handle.clone(),
                                inner_param.clone(),
                                *expose,
                                default.clone(),
                                label.clone(),
                                *min,
                                *max,
                                *default_value,
                                crate::ui_translate::param_convert_to_core(*convert),
                                *is_angle,
                                value_labels.clone(),
                            )
                            .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::SetNodePreviewNormalize(on) => {
                    // Preview-only display preference — no undo, no model
                    // mutation. Update the UI mirror and tell the content
                    // thread to flip the node-preview blit.
                    self.node_preview_normalize = *on;
                    self.send_content_cmd(ContentCommand::SetNodePreviewNormalize(*on));
                    continue;
                }
                manifold_ui::GraphEditCommand::AddSceneObject {
                    scope_path,
                    render_scene_node_id,
                    next_index,
                    centroid,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::AddSceneObjectCommand::new(
                            target.clone(),
                            scope_path.clone(),
                            *render_scene_node_id,
                            *next_index,
                            *centroid,
                            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.phong_material"),
                            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.transform_3d"),
                            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.scene_object"),
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::AddSceneLight {
                    scope_path,
                    render_scene_node_id,
                    next_index,
                    pos,
                } => {
                    if let (Some(target), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::AddSceneLightCommand::new(
                            target.clone(),
                            scope_path.clone(),
                            *render_scene_node_id,
                            *next_index,
                            *pos,
                            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.light"),
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
            }
        }

        // Resize compositor + generator when resolution preset or render scale changes.
        if needs_resolution_resize {
            let p = &self.local_project;
            let w = p.settings.output_width.max(1) as u32;
            let h = p.settings.output_height.max(1) as u32;
            let rs = p.settings.render_scale;
            self.send_content_cmd(ContentCommand::ResizeContent(w, h, rs));
            log::info!(
                "Resolution changed to {}x{} @ {:.2}x render scale",
                w,
                h,
                rs
            );
        }

        // Selection version change → sync inspector so it shows the newly selected clip
        if self.selection.selection_version != prev_sel_version && !needs_structural_sync {
            let active_idx = self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_inspector_data(
                &mut self.ws.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
                &self.content_state.automation_latched_params,
            );
            needs_structural_sync = true;
        }

        if needs_structural_sync {
            let active_idx = self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_project_data(
                &mut self.ws.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
            );
            crate::ui_bridge::sync_inspector_data(
                &mut self.ws.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
                &self.content_state.automation_latched_params,
            );
        } else if self.active_layer_id != prev_active_layer {
            let active_idx = self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_project_data(
                &mut self.ws.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
            );
            crate::ui_bridge::sync_inspector_data(
                &mut self.ws.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
                &self.content_state.automation_latched_params,
            );
            needs_structural_sync = true; // Inspector content changed — needs rebuild
        }
        // Mirror the inspector sync onto the editor window's own inspector
        // instance so its column stays in lockstep with the main one (same
        // snapshot, same selection). Gated on `needs_structural_sync`, which is
        // set by every branch above that re-synced the main inspector — so the
        // two never drift, and reconfigure (which resets transient card state)
        // only fires when it does for the main window.
        if needs_structural_sync && self.graph_editor.is_some() {
            let active_idx = self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            if let Some(ed) = self.graph_editor.as_mut() {
                crate::ui_bridge::sync_inspector_data(
                    &mut ed.ui_root,
                    &self.local_project,
                    active_idx,
                    &self.selection,
                    &self.content_state.automation_latched_params,
                );
            }
        }
        // 2a. Per-frame drag polling with auto-scroll (B11: move/trim/rubber-band —
        // InteractionOverlay.PollMoveDrag, extended). Continues edge autoscroll
        // when the mouse is stationary; also drives B13's live readout, which
        // must reflect the post-poll (already-snapped) clip state (D5: preview
        // == committed result).
        {
            use manifold_ui::interaction_overlay::DragMode;
            let drag_mode = self.overlay.drag_mode();
            if matches!(
                drag_mode,
                DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight | DragMode::RegionSelect
            ) {
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate_layers,
                    &mut self.pre_drag_commands,
                );
                self.overlay.poll_drag(
                    self.cursor_pos,
                    &mut host,
                    &mut self.selection,
                    &mut self.ws.ui_root.viewport,
                );

                let readout = self
                    .overlay
                    .drag_readout_clip_id()
                    .and_then(|id| host.find_clip_by_id(&id))
                    .map(|c| (c.start_beat, c.duration_beats, c.layer_index));
                self.ws.ui_root.viewport.set_drag_readout(readout);
            } else {
                self.ws.ui_root.viewport.set_drag_readout(None);
            }
        }
        // Legacy drag polling removed — overlay.poll_drag() handles it above.

        // 2b. Process deferred export (keyboard shortcut sets flag, processed here
        // where Application has full access for the file dialog).
        if self.pending_export {
            self.pending_export = false;
            self.start_export();
        }

        // 2c. Auto-scroll check for playback (BEFORE build so rebuild includes new scroll)
        let auto_scroll_changed = crate::ui_bridge::check_auto_scroll(
            &mut self.ws.ui_root,
            &self.content_state,
            &self.local_project,
        );
        // Auto-scroll during playback is horizontal-only.
        if auto_scroll_changed {
            self.scroll_dirty.scroll_x = true;
        }
        let overlay_changed = self.ws.ui_root.overlay_dirty;
        self.ws.ui_root.overlay_dirty = false;
        if overlay_changed {
            self.scroll_dirty.visual = true;
        }

        // Overlays (dropdown, browser/generator picker, Ableton picker, Audio
        // Setup) build their nodes into the shared tree and are recorded into
        // `overlay_draw` by `build_overlays`. Opening one already flags
        // `overlay_dirty`; closing one only flips `is_open`, and the programmatic
        // `close()` paths (e.g. entering perform mode) don't route through the
        // event-driven flag — so the closed overlay's nodes and its stale
        // `overlay_draw` range would survive as ghost text. The driver owns the
        // invariant instead: it snapshots the open-set at each build and, when the
        // live set differs (open OR close, by any path), feeds the established
        // visual-rebuild path — which re-records the overlay region and recomposites
        // the offscreen. One detection point, every close site covered.
        if self.ws.ui_root.detect_overlay_open_change() {
            self.scroll_dirty.visual = true;
        }

        let scroll_dirty = self.scroll_dirty;
        self.scroll_dirty.clear();

        self.ui_profile.add("process_events", seg.elapsed());
        seg = std::time::Instant::now();

        // 3. Rebuild if needed
        // Full rebuild: structural changes, data mutations, or explicit needs_rebuild.
        // Partial rebuild: only scroll/zoom changed — rebuild viewport + layer_headers,
        // preserve transport, header, footer, inspector nodes.
        // Horizontal-only scroll skips layer header rebuild entirely.
        //
        // GUARD: If the inspector has an active drag (slider being dragged), defer
        // the rebuild to prevent node destruction mid-drag which causes snap-back.
        //
        // The decision block itself now lives in
        // `ui_frame::apply_ui_frame_invalidations` (P1, D3) — the app and the
        // headless harness call the identical function. `signals` carries the
        // scroll-in-place flag captured earlier this tick (:960) alongside the
        // rebuild flags; the residual `needs_rebuild` (kept set when a drag
        // defers the rebuild) is copied back after the call.
        let mut signals = crate::ui_frame::UiFrameSignals {
            needs_rebuild: self.needs_rebuild,
            needs_structural_sync,
            scroll_dirty,
            scrolled_in_place,
        };
        crate::ui_frame::apply_ui_frame_invalidations(
            &mut self.ws.ui_root,
            self.ui_cache_manager.as_mut(),
            &mut signals,
        );
        self.needs_rebuild = signals.needs_rebuild;

        #[cfg(target_os = "macos")]
        self.sync_workspace_preview_size();

        self.ui_profile.add("rebuild_tree", seg.elapsed());
        seg = std::time::Instant::now();

        // 4. Push engine state to UI panels (AFTER build so new nodes get state)
        let active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        crate::ui_bridge::push_state(
            &mut self.ws.ui_root,
            &self.local_project,
            &self.content_state,
            active_idx,
            &self.selection,
            self.content_state.editing_is_dirty,
            self.current_project_path.as_deref(),
            &mut self.transport_cache,
        );

        // 4b. Sync clip positions — only during drag or structural change.
        // During drag, InteractionOverlay mutates clip data directly in the
        // project model. Outside of drag with no version change, the viewport
        // cache is already current. Skipping saves 50+ string clones per frame.
        if self.mouse_pressed || needs_structural_sync {
            crate::ui_bridge::sync_clip_positions(
                &mut self.ws.ui_root,
                &self.local_project,
                self.selection.automation_mode_visible,
                &self.selection.chosen_automation_params,
            );
        }

        // 4c. Apply per-layer bitmap invalidation from editing operations.
        for layer_idx in self.invalidate_layers.drain(..) {
            self.ws.ui_root.viewport.invalidate_layer_bitmap(layer_idx);
        }

        // 5. Push performance metrics to HUD
        if self.ws.ui_root.perf_hud.is_visible() {
            let bpm = Some(&self.local_project)
                .map(|p| p.settings.bpm)
                .unwrap_or(manifold_core::Bpm(120.0));
            let clock_source = Some(&self.local_project)
                .map(|p| p.settings.clock_authority.display_name().to_string())
                .unwrap_or_else(|| "Internal".to_string());
            self.ws
                .ui_root
                .perf_hud
                .set_metrics(manifold_ui::panels::perf_hud::PerfMetrics {
                    ui_fps: self.frame_timer.current_fps() as f32,
                    ui_frame_time_ms: (self.frame_timer.last_dt() * 1000.0) as f32,
                    render_fps: self.content_state.content_fps,
                    render_frame_time_ms: self.content_state.content_frame_time_ms,
                    gpu_fence_wait_ms: self.content_state.gpu_fence_wait_ms,
                    render_target_fps: self.content_state.frame_rate as f32,
                    active_clips: self.content_state.active_clips,
                    preparing_clips: 0,
                    current_beat: self.content_state.current_beat,
                    current_time_secs: self.content_state.current_time.as_f32(),
                    bpm,
                    clock_source,
                    is_playing: self.content_state.is_playing,
                    data_version: self.content_state.data_version,
                    profiling_active: self.content_state.profiling_active,
                    profiling_frame_count: self.content_state.profiling_frame_count,
                });
        }

        // 6. Lightweight update (playhead, insert cursor, layer selection, HUD values)
        self.ws.ui_root.update();

        // 6·drag-motion. P2 drag-visual tweens (`UI_CRAFT_AND_MOTION_PLAN.md`
        // D15/D17: grab lift, duplicate ghost, grid settle, landing-line
        // flash, error shake). The GPU clip-body pass (Pass 4b below) already
        // re-emits every frame unconditionally, so ticking here is enough —
        // no `needs_rebuild` flag to set, unlike the UITree-driven panels.
        self.overlay.tick((dt * 1000.0) as f32);

        // 6·motion. P1 drawer open/close tween: while any inspector drawer-height
        // tween is in flight, force a rebuild each frame so the interpolated height
        // re-lays-out and the content below reflows. Mirrors the is_dragging()
        // rebuild poll above (a panel bool read after update → needs_rebuild). The
        // forced rebuild's own invalidate_all repaints the inspector, so no
        // separate invalidate is needed here. Reduced motion settles instantly, so
        // this is false at once — no per-frame rebuild churn.
        if self.ws.ui_root.inspector.drawer_anim_active() {
            self.needs_rebuild = true;
        }

        // P2 "panel-split snap-back" (D15): while a double-click-reset tween
        // on either main split is in flight, force a rebuild each frame so
        // every panel re-lays-out from the eased ratio/width — same poll
        // shape as `drawer_anim_active` just above.
        if self.ws.ui_root.layout.is_split_reset_animating() {
            self.needs_rebuild = true;
        }

        // 6·motion. `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6: the redraw
        // keepalive aggregate — while any tree overlay is still animating
        // (today: the D11 toast's enter/hold/fade), force `offscreen_dirty`
        // so the overlay pass (gated on it, `present_all_windows`) keeps
        // recomposing every frame instead of freezing the moment an
        // unrelated input stops re-dirtying the frame.
        if self.ws.ui_root.overlay_redraw_needed() {
            self.ws.offscreen_dirty = true;
        }

        // 6·fire-meter. D6 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
        // P3c, BUG-082's fix): push this tick's live shaped-signal levels onto
        // every open fire-mode drawer's Amount meter in the inspector — in
        // place, no rebuild. Unconditional (unlike the Audio Setup meters
        // below): a fire-mode drawer can be open in the inspector whether or
        // not the Audio Setup dock is. `dt` (BUG-109 P5) drives each meter's
        // UI-side peak-hold — the same frame delta `tick_and_render` already
        // computed at the top of this function.
        self.ws.ui_root.update_fire_meters(&self.content_state.fire_meters, dt as f32);

        // 6·audio. Live per-send level meters in the Audio Setup modal — in-place
        // node resize from the latest content-state levels, no rebuild.
        if self.ws.ui_root.audio_setup_panel.is_open() {
            let count = self.content_state.audio_send_count;
            let levels = self.content_state.audio_send_levels;
            self.ws.ui_root.update_audio_meters(&levels[..count]);

            // Scope hover readout: freq + pink-weighted dB under the cursor, so
            // the number matches the colour. dB is sampled from last frame's ring
            // (1-frame stale is imperceptible); freq is geometric.
            let fmin = self.content_state.spectrogram_fmin;
            let fmax = self.content_state.spectrogram_fmax;
            let freq_log_ratio = if fmin > 0.0 && fmax > fmin { (fmax / fmin).log2() } else { 0.0 };

            // Feed the panel the current crossovers + range so it can hit-test the
            // band-divider lines for dragging.
            self.ws.ui_root.update_audio_scope_bands(
                self.content_state.spectrogram_low_hz,
                self.content_state.spectrogram_mid_hz,
                fmin,
                fmax,
            );

            // Per-band level meters: the tapped send's Low/Mid/High amplitudes.
            let band_amps = self.content_state.spectrogram_features.map(|f| {
                use manifold_core::AudioBand;
                [
                    f.bands[AudioBand::Low.index()].amplitude,
                    f.bands[AudioBand::Mid.index()].amplitude,
                    f.bands[AudioBand::High.index()].amplitude,
                ]
            });
            self.ws.ui_root.update_audio_band_meters(band_amps);

            // The matrix's per-row trigger meter feed (`update_audio_trigger_levels`)
            // is deleted with the matrix (P3, D2). The D6 fire meter that replaces
            // it lives in the audio-mod drawer — deferred to a follow-up phase.

            // Hover readout, suppressed while a divider drag owns the gesture.
            let readout = if self.ws.ui_root.audio_band_dragging() {
                None
            } else {
                self.scope_hover_uv().map(|(ux, uy, freq)| {
                    let db = self
                        .spectrogram
                        .as_ref()
                        .map_or(-120.0, |s| s.sample_db_weighted(ux, uy, freq_log_ratio));
                    format_scope_readout(freq, db)
                })
            };
            self.ws.ui_root.update_audio_scope_readout(readout.as_deref());
        }

        // 6·audio·scope. Push the scope's selected send to the content thread
        // (drives the worker's VQT column producer). Only on change — closing the
        // panel sends `None`, stopping column production.
        //
        // P7 tap-follow (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
        // §7.2 item 5): a currently-open fire-mode drawer (clip trigger or
        // `is_trigger_gate` param card) wins over the panel's own selected
        // send — collapsing it falls straight back to the panel's selection,
        // since this is computed fresh every frame, never persisted (§7.3 P7
        // "Tap-follow state is session-only").
        {
            let desired = if self.ws.ui_root.audio_setup_panel.is_open() {
                self.ws
                    .ui_root
                    .open_fire_mode_drawer_send()
                    .or_else(|| self.ws.ui_root.audio_setup_panel.selected_send().cloned())
            } else {
                None
            };
            if desired != self.spectrogram_send_sent {
                self.send_content_cmd(ContentCommand::SetSpectrogramSend(desired.clone()));
                self.spectrogram_send_sent = desired;
            }
        }

        // 6b. Repaint dirty layer GRID bitmaps. Clip bodies/content + the region /
        // cursor / marker overlays are all GPU now (§24 5b), so the grid is a pure
        // function of the viewport and needs no selection/hover state here.
        self.ws.ui_root.viewport.repaint_dirty_layers();

        // 6c. Upload dirty layer GRID textures + the lane/stem/overview/group
        // panel bitmaps to the single layer-bitmap instance (§24 5b — the per-layer
        // "front" buffer is gone; waveforms are per-clip GPU textures, overlays are
        // GPU rects). Grid uses per-layer indices; panels use 1000/1001/1002/2000+.
        if let (Some(gpu), Some(bitmap_gpu)) = (&self.gpu, &mut self.layer_bitmap_gpu) {
            for (layer_idx, pixels, tw, th) in self.ws.ui_root.viewport.dirty_layer_iter() {
                bitmap_gpu.upload_layer(&gpu.device, layer_idx, pixels, tw as u32, th as u32);
            }

            // 6f. Repaint + upload overview strip bitmap
            self.ws.ui_root.viewport.repaint_overview();
            if let Some((pixels, tw, th)) = self.ws.ui_root.viewport.overview_bitmap() {
                bitmap_gpu.upload_layer(&gpu.device, 1002, pixels, tw as u32, th as u32);
            }

            // 6g. Repaint + upload collapsed group bitmaps
            self.ws.ui_root.viewport.repaint_collapsed_groups();
            for (track_idx, pixels, tw, th) in self.ws.ui_root.viewport.dirty_collapsed_group_iter()
            {
                bitmap_gpu.upload_layer(
                    &gpu.device,
                    2000 + track_idx,
                    pixels,
                    tw as u32,
                    th as u32,
                );
            }
        }

        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        // Advance the UI frame fence once per frame, before any ring-owning
        // encoder below (present_all_windows' layer/clip/UI passes) claims a
        // slot this tick — those claims stamp with this frame number.
        if let Some(fence) = &self.ui_frame_fence {
            fence.advance();
        }

        // Workspace preview via IOSurface (dual device, zero GPU copy).
        #[cfg(target_os = "macos")]
        {
            // Detect preview bridge resize (generation changed) and re-import workspace textures.
            if let Some(ref bridge) = self.preview_texture_bridge {
                let bridge_gen = bridge.generation();
                if bridge_gen != self.last_preview_bridge_generation {
                    self.last_preview_bridge_generation = bridge_gen;
                    let ui_textures: [manifold_gpu::GpuTexture;
                        crate::shared_texture::SURFACE_COUNT] = std::array::from_fn(|i| unsafe {
                        bridge.import_texture_native(&gpu.device, i)
                    });
                    self.ui_preview_textures = ui_textures.map(Some);
                    log::info!(
                        "[UI] re-imported {} workspace preview IOSurface textures after resize (gen={})",
                        crate::shared_texture::SURFACE_COUNT,
                        bridge_gen
                    );
                }
            }
            // Read the workspace preview front surface published by the content thread.
            let front = self
                .preview_texture_bridge
                .as_ref()
                .map_or(0, |b| b.front_index()) as usize;
            if front != self.last_output_front_index {
                self.last_output_front_index = front;
                self.ws.offscreen_dirty = true;
            }
            // Mark dirty if panel nodes changed (structural UI changes, transport
            // text, slider drags, etc.). Overlay nodes (perf HUD, dropdowns,
            // popups) are excluded — they render every frame via the overlay
            // pass and don't need the full offscreen re-render.
            let panel_end = self.ws.ui_root.overlay_region_start;
            if self.ws.ui_root.tree.has_dirty_in_range(0, panel_end) {
                self.ws.offscreen_dirty = true;
            }
            // The Audio Setup scope is a live waterfall: force a full redraw each
            // frame it's open so new VQT columns scroll in (and the meters move)
            // even when nothing else changed. It's a modal authoring surface, so
            // continuous repaint here never competes with a live show.
            if self.ws.ui_root.audio_setup_panel.is_open() {
                self.ws.offscreen_dirty = true;
            }
            self.ui_profile.add("update_repaint_upload", seg.elapsed());
            self.present_all_windows(front);
            let g0 = std::time::Instant::now();
            self.present_graph_editor_window(dt as f32);
            self.ui_profile.add("present_graph_editor", g0.elapsed());
            // Frame-fence sentinel must be the LAST commit of the frame's UI
            // encoders: the graph-editor window shares UIRenderer's vertex
            // rings, so a sentinel committed before it would mark slots
            // retired while that encoder is still in flight.
            if let (Some(fence), Some(gpu)) = (&self.ui_frame_fence, &self.gpu) {
                fence.commit_frame(&gpu.device);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.ui_profile.add("update_repaint_upload", seg.elapsed());
            self.present_all_windows(0);
            let g0 = std::time::Instant::now();
            self.present_graph_editor_window(dt as f32);
            self.ui_profile.add("present_graph_editor", g0.elapsed());
            // Frame-fence sentinel: see the macos branch comment above.
            if let (Some(fence), Some(gpu)) = (&self.ui_frame_fence, &self.gpu) {
                fence.commit_frame(&gpu.device);
            }
        }

        let display_hz = self
            .ws
            .ui_display_link
            .as_ref()
            .map_or(0.0, |dl| dl.actual_refresh_hz());
        self.ui_profile.frame_end(
            frame_t0.elapsed(),
            std::time::Duration::from_secs_f64(dt),
            display_hz,
        );
        self.frame_count += 1;
    }





}

// ── BUG-060 surface dump (env-gated debug instrumentation) ──────────────────
// The stale-sliver artifact (docs/BUG_BACKLOG.md BUG-060) reproduces only on
// the live rig; every headless probe of the atlas has come back clean. These
// dumps attribute observed dirt to a surface: present in the atlas PNG → the
// cache/clear layer; in the offscreen PNG only → composite/blit; on screen but
// in neither → IOSurface/present. Readback + PNG encode stall the render
// thread — they run only under MANIFOLD_BUG060_DUMP. Remove with BUG-060.




/// Build the graph editor's bottom mini-timeline view-model from a project +
/// playhead beat: `(clips, layer_labels, row_count, total_beats,
/// beats_per_bar, readout)`. Every layer becomes a row (and a gutter label);
/// each clip a coloured bar via the shared `get_clip_color` (so the strip
/// matches the main timeline). Shared by the live present pass and the
/// headless snapshot so both draw the same strip.
pub(crate) fn mini_timeline_data(
    project: &manifold_core::project::Project,
    current_beat: f32,
) -> (Vec<manifold_ui::MiniClip>, Vec<manifold_ui::MiniLayerLabel>, usize, f32, f32, String) {
    let mut clips: Vec<manifold_ui::MiniClip> = Vec::new();
    let mut layer_labels: Vec<manifold_ui::MiniLayerLabel> = Vec::new();
    for (row, layer) in project.timeline.layers.iter().enumerate() {
        let is_gen = layer.layer_type == manifold_core::LayerType::Generator;
        let lc = layer.layer_color;
        layer_labels.push(manifold_ui::MiniLayerLabel {
            name: layer.name.clone(),
            color: manifold_ui::Color32::new(
                (lc.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (lc.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (lc.b * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            ),
        });
        for clip in &layer.clips {
            let c = clip.color_override.unwrap_or(layer.layer_color);
            let c32 = manifold_ui::Color32::new(
                (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            );
            let color = manifold_ui::bitmap_painter::get_clip_color(
                false,
                false,
                clip.is_muted || layer.is_muted,
                false,
                is_gen,
                c32,
            );
            clips.push(manifold_ui::MiniClip {
                row,
                start_beat: clip.start_beat.as_f32(),
                end_beat: clip.end_beat().as_f32(),
                color,
            });
        }
    }
    let bpb = project.settings.time_signature_numerator.max(1) as f32;
    let bar = (current_beat / bpb).floor() as i64 + 1;
    let beat_in_bar = (current_beat - (bar - 1) as f32 * bpb).floor() as i64 + 1;
    let readout = format!(
        "Bar {bar}.{beat_in_bar} · {:.0} BPM · {}/{}",
        project.settings.bpm.0,
        project.settings.time_signature_numerator,
        project.settings.time_signature_denominator,
    );
    (
        clips,
        layer_labels,
        project.timeline.layers.len(),
        project.timeline.duration_beats().as_f32(),
        bpb,
        readout,
    )
}


// ── Text input overlay rendering (free function to avoid borrow conflicts) ──

/// Render the text input overlay using immediate-mode draw calls.
pub(crate) fn render_text_input_overlay(
    ti: &crate::text_input::TextInputState,
    timer: &crate::frame_timer::FrameTimer,
    ui: &mut UIRenderer,
) {
    use crate::text_input::*;

    let a = &ti.anchor;
    let fs = ti.font_size;
    let pad_h = TEXT_INPUT_PAD_H;
    let pad_v = TEXT_INPUT_PAD_V;
    let line_h = fs + 3.0; // line height with leading

    let bg_x = a.x;
    let bg_y = a.y;
    let bg_w = a.width.max(40.0);

    let text = ti.model.text();
    let sel = ti.model.selection();
    let has_selection = ti.model.has_selection();

    // For multiline fields, compute height from line count (minimum 3 lines).
    let line_count = if ti.multiline { text.split('\n').count().max(3) } else { 1 };
    let bg_h = (line_count as f32 * line_h + pad_v * 2.0).max(a.height.max(fs + pad_v * 2.0));

    ui.draw_bordered_rect(
        bg_x,
        bg_y,
        bg_w,
        bg_h,
        TEXT_INPUT_BG,
        3.0,
        1.0,
        manifold_ui::Color32::new(89, 115, 179, 204), // sRGB, was [0.35, 0.45, 0.7, 0.8]
    );

    let text_x = bg_x + pad_h;
    let width = |ui: &mut UIRenderer, s: &str| ui.measure_text_cached(s, fs as u16, FontWeight::Medium).x;

    if ti.multiline {
        // Draw each line separately.
        for (i, line) in text.split('\n').enumerate() {
            let ly = bg_y + pad_v + i as f32 * line_h;
            // This line's byte range within `text` (offsets, not indices).
            let line_start = text
                .split('\n')
                .take(i)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            let line_end = line_start + line.len();
            if has_selection && sel.start < line_end && sel.end > line_start {
                let hl_start = sel.start.max(line_start) - line_start;
                let hl_end = sel.end.min(line_end) - line_start;
                let hx = text_x + width(ui, &line[..hl_start]);
                let hw = width(ui, &line[..hl_end]) - width(ui, &line[..hl_start]);
                ui.draw_rect(hx, ly, hw.max(2.0), line_h, TEXT_INPUT_SELECT_BG);
            }
            ui.draw_text(text_x, ly, line, fs, TEXT_INPUT_FG);
        }

        // Blinking caret — find which line it's on.
        if !has_selection {
            let elapsed = timer.realtime_since_start();
            let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
            if blink_on {
                let before = &text[..ti.model.caret()];
                let cursor_line = before.matches('\n').count();
                let line_start = before.rfind('\n').map_or(0, |p| p + 1);
                let before_on_line = &before[line_start..];
                let cursor_x = text_x + width(ui, before_on_line);
                let cursor_y = bg_y + pad_v + cursor_line as f32 * line_h;
                ui.draw_rect(cursor_x, cursor_y, TEXT_INPUT_CURSOR_W, line_h, TEXT_INPUT_CURSOR);
            }
        }
    } else {
        // Single-line rendering.
        let text_y = bg_y + pad_v;
        if has_selection {
            let hx = text_x + width(ui, &text[..sel.start]);
            let hw = width(ui, &text[..sel.end]) - width(ui, &text[..sel.start]);
            ui.draw_rect(
                hx,
                bg_y + pad_v,
                hw.min(bg_w - pad_h * 2.0).max(2.0),
                line_h,
                TEXT_INPUT_SELECT_BG,
            );
        }
        ui.draw_text(text_x, text_y, text, fs, TEXT_INPUT_FG);

        if !has_selection {
            let elapsed = timer.realtime_since_start();
            let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
            if blink_on {
                let before = &text[..ti.model.caret()];
                let cursor_x = text_x + width(ui, before);
                ui.draw_rect(
                    cursor_x,
                    bg_y + pad_v,
                    TEXT_INPUT_CURSOR_W,
                    bg_h - pad_v * 2.0,
                    TEXT_INPUT_CURSOR,
                );
            }
        }
    }
}










// The `build_card_exposures` / `build_outer_driven_map` / `build_wire_driven_keys`
// / `build_static_block_targets` joins that fed the deleted inner-node param
// sidebar are gone: the canvas now derives exposed / wire-driven / outer-driven
// state itself from the snapshot (see `GraphCanvas::apply_driven_state`), and the
// per-node expose checkbox lives on the node face.






/// BUG-281 regression. A card-bound graph-node-face scrub live-writes
/// `local_project` every tick via `bound_node_param_drag` (the reroute arm
/// above, `with_preset_graph_mut` + `set_base_param` on `outer_param_id`),
/// but the snapshot-acceptance restore path only re-applied
/// `active_inspector_drag` — so a snapshot landing mid-gesture stomped the
/// bound value back to `old_value` until the next tick (visible revert on
/// the card slider). Mirrors the BUG-262 `mapping_undo_baseline` shape:
/// given the guard a live drag installs, a stale pre-drag snapshot must come
/// back carrying the dragged value.
#[cfg(test)]
mod bound_node_param_drag_tests {
    use super::BoundNodeParamDrag;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::macro_bank::MacroCurve;
    use manifold_core::params::Param;
    use manifold_core::project::Project;
    use manifold_core::{GraphTarget, PresetTypeId};

    /// One master effect carrying a single outer card param ("amount"),
    /// default 0.0 — the write target `BoundNodeParamDrag::apply` reaches
    /// via `with_preset_graph_mut` + `set_base_param`.
    fn project_with_amount_param() -> (Project, GraphTarget) {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("Test"));
        let effect_id = fx.id.clone();
        fx.params.push(Param::bundled(ParamSpecDef {
            id: "amount".into(),
            name: "Amount".into(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: MacroCurve::Linear,
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }));
        project.settings.master_effects.push(fx);
        (project, GraphTarget::Effect(effect_id))
    }

    #[test]
    fn bound_node_param_drag_survives_snapshot_stomp() {
        let (project, target) = project_with_amount_param();
        let before = project.settings.master_effects[0].get_base_param("amount");
        assert_eq!(before, 0.0, "fixture starts at the default value");

        // The guard a live bound-node-param scrub installs (in-flight value
        // 0.7, matching the live write at app_render.rs's
        // `SetGraphNodeParam` reroute arm).
        let guard = BoundNodeParamDrag {
            target: target.clone(),
            node_id: 1,
            param_name: "inner".to_string(),
            outer_param_id: "amount".to_string(),
            old_value: 0.0,
            current_value: 0.7,
        };
        // A full snapshot lands mid-drag carrying the stale pre-drag
        // project; app_render restores the guarded drag onto it.
        let mut stomped = project.clone();
        guard.apply(&mut stomped);

        let after = stomped.settings.master_effects[0].get_base_param("amount");
        assert_eq!(
            after, 0.7,
            "bound-node-param stomp must be undone so the card doesn't revert mid-gesture"
        );
    }
}

/// BUG-282 regression. An UNBOUND node-face param scrub used to push a
/// fresh undo-worthy `SetGraphNodeParamCommand::execute` on EVERY
/// pointer-move tick — an N-tick drag flooded the 200-cap undo stack
/// instead of coalescing to one entry. The fix mirrors the bound-row
/// pattern: N in-flight ticks land via `MutateProjectLive` — a direct
/// `Command::execute` call on the content-thread `Project` that never
/// touches an undo manager — and only the ONE release-time command, seeded
/// with `with_previous(pre_drag_value)` (the seam
/// `SetGraphNodeParamCommand::with_previous`'s doc comment describes for
/// exactly this drag-cadence-commit case), goes through
/// `UndoRedoManager::execute`. This test drives that same sequence against
/// a real `UndoRedoManager`: N direct `execute()` calls (the tick writes)
/// followed by one `UndoRedoManager::execute` (the release commit), then
/// asserts `undo_count() == 1` (not `N + 1`) and that `undo()` restores the
/// true pre-drag value rather than whatever `execute()`'s self-capture
/// would have seen post-drag.
#[cfg(test)]
mod unbound_node_param_drag_tests {
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
    use manifold_core::effects::PresetInstance;
    use manifold_core::project::Project;
    use manifold_core::{GraphTarget, NodeId, PresetTypeId};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::SetGraphNodeParamCommand;
    use manifold_editing::undo::UndoRedoManager;
    use std::collections::BTreeMap;

    fn empty_def() -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![],
            wires: vec![],
        }
    }

    /// One master effect carrying a per-instance graph override with a
    /// single node (id 1) holding one param ("amount") at `initial` — the
    /// unbound node-face scrub's write target.
    fn project_with_node_param(initial: f32) -> (Project, GraphTarget) {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("Test"));
        let effect_id = fx.id.clone();
        let mut params = BTreeMap::new();
        params.insert("amount".to_string(), SerializedParamValue::Float { value: initial });
        let mut def = empty_def();
        def.nodes.push(EffectGraphNode {
            id: 1,
            node_id: NodeId::new("inner"),
            type_id: "node.test".to_string(),
            handle: None,
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        fx.graph = Some(def);
        project.settings.master_effects.push(fx);
        (project, GraphTarget::Effect(effect_id))
    }

    fn read_amount(project: &Project) -> f32 {
        let def = project.settings.master_effects[0].graph.as_ref().unwrap();
        match def.nodes[0].params.get("amount") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a Float amount, got {other:?}"),
        }
    }

    #[test]
    fn n_tick_scrub_is_one_undo_entry_and_undo_restores_pre_drag_value() {
        let (mut project, target) = project_with_node_param(0.0);
        let pre_drag_value = Some(SerializedParamValue::Float { value: 0.0 });

        // N pointer-move ticks: each is a live write — the app's
        // `MutateProjectLive` path (`live_cmd.execute(p)` in the
        // `SetGraphNodeParam` arm) calling `Command::execute` directly on
        // the content-thread project, never touching an undo manager.
        let ticks = [0.1_f32, 0.3, 0.55, 0.72, 0.9];
        for &v in &ticks {
            let mut live = SetGraphNodeParamCommand::new(
                target.clone(),
                1,
                "amount".to_string(),
                SerializedParamValue::Float { value: v },
                empty_def(),
            );
            live.execute(&mut project);
        }
        let last_tick_value = *ticks.last().unwrap();
        assert_eq!(
            read_amount(&project),
            last_tick_value,
            "live writes DO land on the project every tick"
        );

        // Release: ONE command, seeded with the true pre-drag baseline via
        // `with_previous`, pushed through the real undo manager — mirrors
        // `EndGraphNodeParamScrub`'s unbound-drag close-out.
        let mut undo_mgr = UndoRedoManager::new();
        assert_eq!(undo_mgr.undo_count(), 0, "sanity: fresh manager");
        let commit = SetGraphNodeParamCommand::new(
            target,
            1,
            "amount".to_string(),
            SerializedParamValue::Float { value: last_tick_value },
            empty_def(),
        )
        .with_previous(pre_drag_value);
        undo_mgr.execute(Box::new(commit), &mut project);

        assert_eq!(
            undo_mgr.undo_count(),
            1,
            "an N-tick drag must be EXACTLY one undo-worthy commit, not one per tick"
        );
        assert_eq!(read_amount(&project), last_tick_value);

        let _ = undo_mgr.undo(&mut project);
        assert_eq!(
            read_amount(&project),
            0.0,
            "undo must restore the true pre-drag value, not whatever execute()'s \
             self-capture would have seen post-drag"
        );
    }
}


