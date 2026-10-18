//! Rendering methods for Application — extracted from app.rs.
//!
//! Contains `tick_and_render()`, `present_all_windows()`, and the text input
//! overlay rendering helper. All methods are `impl Application` blocks that
//! operate on the struct defined in app.rs.

use manifold_ui::{EditingAction, ParamsAction, ProjectAction, RootAction, TransportAction};
use manifold_ui_paint::ui_renderer::UIRenderer;

use manifold_ui::node::FontWeight;
use manifold_ui::panels::{PanelAction, ScrubPhase, ScrubValue, ValueRef};
use manifold_ui::timeline_editing_host::TimelineEditingHost;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use manifold_editing::command::Command;

fn perf_metrics_from_content_state(
    content_state: &ContentState,
    ui_fps: f32,
    ui_frame_time_ms: f32,
    bpm: manifold_core::Bpm,
    clock_source: &'static str,
) -> manifold_ui::panels::perf_hud::PerfMetrics {
    manifold_ui::panels::perf_hud::PerfMetrics {
        ui_fps,
        ui_frame_time_ms,
        render_fps: content_state.content_fps,
        render_frame_time_ms: content_state.content_frame_time_ms,
        gpu_fence_wait_ms: content_state.gpu_fence_wait_ms,
        physics_cpu_ms: content_state.physics_cpu_ms,
        physics_body_count: content_state.physics_body_count,
        physics_backlog_seconds: content_state.physics_backlog_seconds,
        sim_step_cap_hit: content_state.sim_step_cap_hit,
        sim_nonfinite: content_state.sim_nonfinite,
        render_target_fps: content_state.frame_rate as f32,
        active_clips: content_state.active_clips,
        preparing_clips: 0,
        current_beat: content_state.current_beat,
        current_time_secs: content_state.current_time.as_f32(),
        bpm,
        clock_source,
        is_playing: content_state.is_playing,
        data_version: content_state.data_version,
        profiling_active: content_state.profiling_active,
        profiling_frame_count: content_state.profiling_frame_count,
    }
}

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
/// subsequent move via `MutateProject` (Values or Shapes) (no undo entry), closed on the
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
    /// Preparation-only local controls are held in the same draft slot but
    /// commit through the content-owned modifier action instead of a live
    /// graph write.
    preparation: Option<(manifold_core::NodeId, String)>,
    /// Value before the drag started. `None` means the key was absent —
    /// the same `Option<SerializedParamValue>` shape `with_previous` takes.
    pre_drag_value: Option<manifold_core::effect_graph_def::SerializedParamValue>,
    /// Value as of the last move — the undo redo target.
    current_value: manifold_core::effect_graph_def::SerializedParamValue,
}

fn invert_preparation_value(
    node_value: f32,
    target: &crate::scene_modifier_edit::PreparationTarget,
) -> Option<f32> {
    if !node_value.is_finite()
        || !target.min.is_finite()
        || !target.max.is_finite()
        || target.min > target.max
        || !target.scale.is_finite()
        || !target.offset.is_finite()
    {
        return None;
    }
    let value = manifold_core::effects::invert_card_reshape(
        node_value,
        target.min,
        target.max,
        target.invert,
        target.curve,
        target.scale,
        target.offset,
    )?;
    if !value.is_finite() || value < target.min || value > target.max {
        return None;
    }
    let roundtrip = manifold_core::effects::apply_card_reshape(
        value,
        target.min,
        target.max,
        target.invert,
        target.curve,
        target.scale,
        target.offset,
    );
    let tolerance = 1e-4 * node_value.abs().max(1.0);
    (roundtrip.is_finite() && (roundtrip - node_value).abs() <= tolerance).then_some(value)
}

#[cfg(test)]
mod preparation_inversion_tests {
    use super::invert_preparation_value;
    use crate::scene_modifier_edit::PreparationTarget;
    use manifold_core::NodeId;
    use manifold_core::macro_bank::MacroCurve;

    fn target(scale: f32) -> PreparationTarget {
        PreparationTarget {
            modifier_id: NodeId::new("modifier"),
            param_id: "prep".into(),
            baseline: 0.5,
            min: 0.0,
            max: 1.0,
            invert: false,
            curve: MacroCurve::Linear,
            scale,
            offset: 0.0,
        }
    }

    #[test]
    fn affine_inverse_accepts_representable_value() {
        assert_eq!(invert_preparation_value(0.75, &target(2.0)), Some(0.375));
    }

    #[test]
    fn affine_inverse_rejects_degenerate_and_unrepresentable_values() {
        assert!(invert_preparation_value(0.5, &target(0.0)).is_none());
        assert!(invert_preparation_value(3.0, &target(2.0)).is_none());
    }
}

impl Application {
    /// Take the newest state from the content thread: its export notice, its
    /// project snapshot and stamps, its modulation capture and spectrogram
    /// columns, then the state itself. The one reader of the handoff, for the
    /// normal frame and perform mode alike; the handoff folds any state the
    /// UI never read into the one it holds, so one take per frame sees all.
    pub(crate) fn intake_content_state(&mut self) {
        let Some(state) = self.state_rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.ws.ui_root.consume_export_notification(&state);
        if state.is_bare_notice() {
            return;
        }
        let drag_active = self.overlay.drag_mode()
            != manifold_ui::interaction_overlay::DragMode::None
            || self.overlay.has_pending_automation_press();
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

        // Accept the project snapshot. A drag in flight lives in the
        // gesture preview, which re-covers the fresh clone below, so
        // no drag holds snapshots back (D12).
        if let Some(snapshot) = state.project_snapshot
            && !suppressed
        {
            // `ContentState` travels every tick, so only an accepted
            // snapshot folds stamps, and a Values, Shapes or
            // MappingRanges move alone never re-projects (D23).
            let moved = self.ui_edits.stamps.fold_snapshot(&state.stamps);
            let version_changed = state.data_version != self.content_state.data_version
                || !moved.minus(manifold_core::change::ChangeSet::LIVE).is_empty();
            // Only deep-clone from Arc when it's a different allocation
            // (new data_version). Modulation-only frames send the same
            // Arc pointer — skip the clone (values are 1 frame stale,
            // imperceptible).
            let is_new_arc = self
                .last_snapshot_arc
                .as_ref()
                .is_none_or(|prev| !std::sync::Arc::ptr_eq(prev, &snapshot));
            if is_new_arc {
                manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::ProjectClone);
                *self.local_project.intake() = (*snapshot).clone();
                self.last_snapshot_arc = Some(snapshot);
                // Re-cover the clone with the gesture in flight.
                // With no drag live, a leftover preview is stale
                // and drops with the content's truth.
                if drag_active {
                    self.ui_edits.preview.reapply(self.local_project.intake());
                } else {
                    drop(self.ui_edits.preview.take_clip_gesture());
                    self.ui_edits.preview.clear_lanes();
                }
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
            self.scrub.restore_dragged(self.local_project.intake());
            // Clear suppression once we've accepted a post-load snapshot
            self.suppress_snapshot_until = 0;

            // An edit snapshot: the stamp fold above already moved the
            // parts the planner re-projects (D5 source a). Here only
            // the selection drops references the edit removed.
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
            }
        }
        // Apply lightweight modulation snapshot (param_values only)
        // to local_project — no full Project clone needed.
        if !suppressed
            && let Some(ref mod_snap) = state.modulation_snapshot
        {
            mod_snap.apply(self.local_project.intake());
            // Restore the one actively-dragged gesture so modulation
            // doesn't overwrite the value the user is manipulating —
            // every family, panel-wired and frame-resident (BUG-281's
            // bound-node-param card scrub included, now in `active`).
            self.scrub.restore_dragged(self.local_project.intake());
        }
        // The columns accumulate until the render path draws (clears)
        // them; `content_state` keeps only the newest state. Perform mode
        // draws no scope, so it keeps none.
        if self.perform.active {
            self.pending_spectrogram_columns.clear();
            self.pending_spectrogram_scalars.clear();
        } else {
            crate::content_state::append_spectrogram(
                &mut self.pending_spectrogram_columns,
                &mut self.pending_spectrogram_scalars,
                &state.spectrogram_columns,
                &state.spectrogram_col_scalars,
                state.spectrogram_num_bins,
            );
        }
        self.content_state = ContentState {
            project_snapshot: None,      // consumed above
            modulation_snapshot: None,   // consumed above
            spectrogram_columns: Vec::new(), // accumulated above
            spectrogram_col_scalars: Vec::new(), // accumulated above
            ..state
        };
    }

    pub(crate) fn tick_and_render(&mut self) {
        self.work_trace.frame("ui");
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

        // 1. Take the newest state from the content thread.
        self.intake_content_state();
        self.tick_sync_calibration();
        // "Show gizmos" follows whichever scene the Scene panel shows; the
        // content thread reports that layer's rendered view (D7a).
        let gizmo_layer = self.ws.ui_root.scene_setup_panel.gizmo_layer();
        if gizmo_layer != self.ws.gizmo_layer_sent.as_ref()
            && let Some(tx) = self.content_tx.as_ref()
        {
            self.ws.gizmo_layer_sent = gizmo_layer.cloned();
            ContentCommand::send(tx, ContentCommand::SetPhysicsOverlay(self.ws.gizmo_layer_sent.clone()));
        }

        // The applied selection moves its own versions.
        crate::edit_selection::apply_update(
            &mut self.ws.ui_root, &self.local_project,
            self.content_state.edit_selection_update.as_deref(),
            &mut self.selection, &mut self.active_layer_id,
        );

        // 1a. Debounced background autosave (GIG_RESILIENCE_DESIGN section 6). Runs
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

        // 1a1. Breadcrumb sidecar (GIG_RESILIENCE_DESIGN section 5.1). Unlike
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
        // The clip list is walked only when Clips or Layers moved (P4c).
        let clips_key = self.ui_edits.stamps.parts().key(manifold_core::change::ChangeSet::of(&[
            manifold_core::change::Part::Clips,
            manifold_core::change::Part::Layers,
        ]));
        let project = &self.local_project;
        let gather = || -> Vec<(manifold_core::id::ClipId, String)> {
            project
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
                .collect()
        };
        // A decode that lands this frame must be attached now: the viewport clip
        // snapshot is only rebuilt on drag / structural change, so without this the
        // waveform would stay blank (and look like it "cleared" while scrolling)
        // until the next unrelated edit. Re-sync so the new renderer attaches; the
        // per-layer fingerprint (waveform.is_some()) then repaints the lane once.
        if self.ws.ui_root.audio_waveforms.drive(clips_key, gather) {
            crate::ui_bridge::sync_clip_positions(&mut self.ws.ui_root, &self.local_project);
        }

        // 1c. Push the latest graph snapshot into the editor canvas
        // (read-only viewer of the watched effect's graph). Translate the
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
        if let (Some(canvas), Some(ui_snap), Some(ed)) =
            (self.graph_canvas.as_mut(), ui_snap, self.graph_editor.as_mut())
        {
            // The "Reset to Default" pill shows only when the watched graph
            // diverges from its bundled preset; keyed on its inputs.
            let has_mod = self.editor_graph.has_graph_mod(
                &self.local_project,
                self.watched_graph_target.as_ref(),
                self.watched_catalog_default.as_ref(),
                &self.ui_edits.stamps.parts(),
            );
            // The canvas draws again only when its feed moved (D17).
            ed.offscreen_dirty |= crate::editor_frame::feed_canvas(
                canvas,
                &mut ed.editor_frame.canvas,
                &ui_snap,
                preview_aspect,
                (dt * 1000.0) as f32,
                &self.content_state.live_node_params,
                has_mod,
            );
        }

        // 1e. Sync percussion pipeline status to header panel
        // Port of Unity WorkspaceController.RefreshPercussionImportStatusLabel
        {
            let msg = if self.ws.ui_root.export_progress.is_open() {
                ""
            } else {
                self.content_state.percussion_status_message.as_ref()
            };
            let progress = self.content_state.percussion_progress;
            let show = self.content_state.percussion_show_progress && !msg.is_empty();
            self.ws.ui_root.header.set_import_status(
                msg,
                if progress < 0.0 {
                    0.0
                } else {
                    progress.clamp(0.0, 1.0)
                },
                show,
            );
            // The progress bar is a chrome input; the imported clips and layers
            // arrive as an edit snapshot, whose stamps re-project them.
        }

        // Export owns its progress surface. Do not redraw a stale loading
        // overlay beneath it; warmup itself only needs changed-state rebuilds.
        {
            let warmup = if self.ws.ui_root.export_progress.is_open() {
                None
            } else {
                self.content_state.warmup.clone()
            };
            if self.ws.ui_root.warmup != warmup {
                self.ws.ui_root.warmup = warmup;
                self.ws.ui_root.mark_overlay_dirty();
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
        let mut actions = self.ws.ui_root.process_events();

        // A session dies with its overlay; the browser popup takes its search
        // session and pins it over its search bar (`text_session.rs`).
        crate::text_session::pump_main_window(&mut self.text_input, &mut self.ws.ui_root);

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
            if self.ws.ui_root.export_progress.is_open() {
                continue;
            }
            use crate::menu::MenuAction as M;
            use manifold_ui::panels::PanelAction as P;
            match ma {
                M::New => actions.push(P::Project(ProjectAction::NewProject)),
                M::Open => actions.push(P::Project(ProjectAction::OpenProject)),
                M::OpenRecentPath(path) => {
                    self.open_project_from_path(path);
                }
                M::ClearRecentProjects => {
                    self.project_io.clear_recent_projects(&mut self.user_prefs);
                    self.refresh_recent_menu();
                }
                M::Save => actions.push(P::Project(ProjectAction::SaveProject)),
                M::SaveAs => actions.push(P::Project(ProjectAction::SaveProjectAs)),
                M::CollectAllAndSave => self.collect_all_and_save(),
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
                    self.selection.clear_automation_selection();
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
                    self.selection.clear_automation_selection();
                    if let Some(tx) = self.content_tx.as_ref() {
                        crate::ui_bridge::redo(tx);
                    }
                }
                M::Settings => self.pending_open_settings = true,
                M::RtQuality => {
                    self.ws.ui_root.rt_quality_panel.open();
                    self.ws.ui_root.mark_overlay_dirty();
                }
            }
        }

        // Settings… (⌘, or the MANIFOLD menu) → open the floating settings popup.
        if std::mem::take(&mut self.pending_open_settings) {
            self.ws.ui_root.settings_popup.open();
            // Programmatic open: nudge the overlay driver to rebuild + draw it.
            self.ws.ui_root.mark_overlay_dirty();
        }

        // An in-place inspector scroll (the wheel, or a scrollbar drag inside
        // process_events) offset the content nodes without a rebuild; the
        // frame seam re-renders just the inspector's atlas slot. One drain
        // point for both scroll inputs.
        let scrolled_in_place = self.ws.ui_root.inspector.take_scrolled_in_place();
        // The editor window's tree, canvas and popovers (`editor_input.rs`).
        // Canvas and sidebar edits dispatch through their own vocabulary
        // (`GraphEditCommand`) below; card-lane actions dispatch against the
        // editor's own tree after the main segment; the popovers' mapping
        // actions carry their owner and join the main queue.
        let editor_drain = crate::editor_input::drain(
            self.graph_editor.as_mut(),
            self.graph_canvas.as_mut(),
            &mut self.editor_mapping_popover,
            &mut self.text_input,
            crate::editor_input::SmartPreviewToggle {
                intents: &mut self.editor_sidebar_intents,
                id: self.editor_smart_preview_toggle_id,
                normalize: self.node_preview_normalize,
            },
        );
        let graph_edits = editor_drain.graph_edits;
        let editor_card_actions = editor_drain.card_actions;
        actions.extend(editor_drain.actions);

        // 2a. Route viewport tracks-area events through InteractionOverlay.
        // These events were stashed by process_events() because the overlay
        // needs &mut TimelineEditingHost which UIRoot can't provide.
        {
            let viewport_events = self.ws.ui_root.drain_viewport_events();
            if !viewport_events.is_empty() {
                if viewport_events.iter().any(|event| matches!(event, manifold_ui::input::UIEvent::PointerDown { .. })) {
                    crate::ui_bridge::sync_automation_lane_order(&self.local_project, &mut self.selection);
                }
                // Sync modifier state to overlay (Unity reads Keyboard.current inline)
                self.overlay.set_modifiers(self.modifiers);
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    self.local_project.edit(&mut self.ui_edits, content_tx),
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.ws.offscreen_dirty,
                );
                for event in &viewport_events {
                    use manifold_ui::input::UIEvent;
                    if let UIEvent::PointerDown { modifiers, .. }
                        | UIEvent::Click { modifiers, .. }
                        | UIEvent::DoubleClick { modifiers, .. }
                        | UIEvent::RightClick { modifiers, .. }
                        | UIEvent::DragBegin { modifiers, .. }
                        | UIEvent::Drag { modifiers, .. } = event
                    {
                        self.overlay.set_modifiers(*modifiers);
                    }
                    match event {
                        UIEvent::PointerDown { pos, .. } => {
                            self.overlay.on_pointer_down(
                                *pos, &mut host, &mut self.selection, &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::PointerUp { .. } => {
                            self.overlay.on_pointer_up(&mut host, &mut self.selection);
                        }
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
                        UIEvent::DragBegin { origin, pos, .. } => {
                            self.overlay.on_begin_drag(
                                *origin,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                            if self.overlay.is_automation_drag() {
                                self.overlay.on_drag(
                                    *pos, &mut host, &mut self.selection, &mut self.ws.ui_root.viewport,
                                );
                            }
                        }
                        UIEvent::Drag { pos, .. } => {
                            self.overlay.on_drag(
                                *pos,
                                &mut host,
                                &mut self.selection,
                                &mut self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { pos, .. } => {
                            if self.overlay.is_automation_drag() {
                                self.overlay.on_drag(
                                    *pos, &mut host, &mut self.selection, &mut self.ws.ui_root.viewport,
                                );
                            }
                            self.overlay.on_end_drag(&mut host);
                        }
                        _ => {}
                    }
                }

                // Drain actions generated by the host during overlay processing
                // (right-click context menus: ClipRightClicked, TrackRightClicked).
                actions.append(&mut host.pending_actions);
            }
            // A selected point in another lane folds into `automation_view`
            // when the planner reads it (D8).
        }

        // Overlay-generated right-click actions (TrackRightClicked, ClipRightClicked)
        // arrive AFTER process_events() has already run its try_open_dropdown pass.
        // Route them through the dropdown system now so context menus actually open.
        self.ws.ui_root.intercept_overlay_actions(&mut actions);

        // Update effect clipboard count for browser popup
        self.ws.ui_root.effect_clipboard_count = self.ws.ui_root.effect_clipboard.count();

        // Trigger Ableton re-discovery when the picker opens so it shows fresh data.
        if self.ws.ui_root.ableton_rediscovery_needed {
            self.ws.ui_root.ableton_rediscovery_needed = false;
            self.send_content_cmd(ContentCommand::AbletonRediscover);
        }

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
            if self.ws.ui_root.export_progress.is_open()
                && !matches!(action, PanelAction::Project(ProjectAction::CancelExport))
            {
                continue;
            }
            if self.dispatch_inspector_host_action(action, false) { continue; }
            if crate::text_session::begin_for_action(
                &mut self.text_input,
                &mut self.ws.ui_root,
                &self.local_project,
                &self.selection,
                action,
            ) {
                continue;
            }
            if let PanelAction::Root(action) = action
                && self.dispatch_mapping_action(action)
            {
                continue;
            }
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
                PanelAction::Project(ProjectAction::ChangeRtQuality(new_settings)) => {
                    use manifold_editing::commands::settings::ChangeRtQualityCommand;
                    let old_settings = self.content_state.rt_quality;
                    let cmd = ChangeRtQualityCommand::new(old_settings, *new_settings);
                    self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
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
                    // this is a true toggle regardless of entry state. The
                    // dock's view version rebuilds the tree at the new
                    // geometry and runs its device query. The toggle lives on
                    // UIRoot so the headless script harness reaches the same
                    // one via ui_bridge::dispatch.
                    self.ws.ui_root.toggle_audio_dock();
                    continue;
                }
                PanelAction::Root(RootAction::OpenRtQuality) => {
                    // Settings popup ▸ RT Quality ▸ Configure… — floating
                    // overlay (not a dock): open + dirty nudge, same shape as
                    // the pending_open_settings block below.
                    self.ws.ui_root.rt_quality_panel.open();
                    self.ws.ui_root.mark_overlay_dirty();
                    continue;
                }
                PanelAction::Root(RootAction::PreviewSceneModifierObject(layer, modifier_id, object)) => {
                    let target = manifold_core::GraphTarget::SceneModifier {
                        owner: Box::new(manifold_core::GraphTarget::Generator(layer.clone())),
                        modifier_id: modifier_id.clone(),
                    };
                    self.watch_graph_target(target.clone());
                    self.modifier_preview_object = Some((target, manifold_core::scene_modifier_preset::SceneNodeRef {
                        scope: object.scope.clone(), node: object.node.clone(),
                    }));
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::Root(RootAction::OpenGraphTarget(target)) => {
                    self.watch_graph_target(crate::editing_host::to_graph_target(target));
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::Root(RootAction::SceneSetupCopyViewportCamera(layer_id)) => {
                    let view = self.graph_editor.as_ref().and_then(|ws| crate::scene_view_copy::live_view(ws, layer_id));
                    if let Some(content_tx) = self.content_tx.as_ref() {
                        crate::scene_view_copy::run(
                            &mut self.local_project.edit(&mut self.ui_edits, content_tx),
                            &mut self.ws.ui_root, layer_id, view.as_ref(),
                        );
                    }
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
                            self.editor_graph.catalog_default_changed();
                        }
                    }
                    self.pending_open_graph_editor = true;
                    continue;
                }
                // ── Graph-editor mutations moved to the `graph_edits` loop
                // below (Phase 4.3) — they're `GraphEditCommand` now, not
                // `PanelAction`. ──
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
                PanelAction::Project(ProjectAction::CancelExport) => {
                    self.send_content_cmd(ContentCommand::CancelExport);
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
                    continue;
                }
                PanelAction::Project(ProjectAction::OpenRecent) => {
                    self.open_recent_project();
                    continue;
                }
                PanelAction::Params(ParamsAction::PasteEffects) => {
                    self.edit_inspector_cards(manifold_ui::panels::actions::CardEditAction::Paste);
                    continue;
                }
                PanelAction::Params(ParamsAction::EditCards(action)) => {
                    self.edit_inspector_cards(*action);
                    continue;
                }
                PanelAction::Editing(EditingAction::AutomationPointEditValue(
                    target,
                    param_id,
                    beat,
                    value_norm,
                )) => {
                    crate::app::automation_point_input::begin_value(self, target, param_id, *beat, *value_norm);
                    continue;
                }
                PanelAction::Editing(EditingAction::AutomationPointEditTime(
                    target,
                    param_id,
                    beat,
                    value_norm,
                )) => {
                    crate::app::automation_point_input::begin_time(self, target, param_id, *beat, *value_norm);
                    continue;
                }
                PanelAction::Params(ParamsAction::GenStringParamDropdownClicked(sp_idx)) => {
                    // The font param opens the searchable font list: each
                    // name in its own font, opened on the current font.
                    let font_pick = self.ws.ui_root.inspector.gen_params().and_then(|gp| {
                        let sp = gp.string_param(*sp_idx).filter(|sp| sp.key == "fontFamily")?;
                        let r = gp.string_param_rect(&self.ws.ui_root.tree, *sp_idx)?;
                        Some((sp.value.clone(), r))
                    });
                    if let Some((current, r)) = font_pick {
                        let families =
                            manifold_nodes_image::text_rasterizer::TextRasterizer::available_font_families();
                        let actions = families
                            .iter()
                            .map(|name| PanelAction::Params(ParamsAction::GenStringParamSelected(*sp_idx, name.clone())))
                            .collect();
                        let list = manifold_ui::panels::browser_popup::ActionListOptions {
                            empty_label: "No fonts match",
                            label_in_own_font: true,
                            current: families.iter().position(|f| *f == current),
                            ..Default::default()
                        };
                        let trigger = manifold_ui::node::Rect::new(r.x, r.y, r.width, r.height);
                        self.ws.ui_root.open_action_list(families, actions, list, trigger);
                        continue;
                    }
                    // Other string params open a plain dropdown.
                    if let Some(gp) = self.ws.ui_root.inspector.gen_params()
                        && let Some(sp) = gp.string_param(*sp_idx)
                    {
                        let key = sp.key.clone();
                        if let Some(r) = gp.string_param_rect(&self.ws.ui_root.tree, *sp_idx) {
                            // Typed (2b.11): each choice carries its
                            // GenStringParamSelected. Audio-send choices are
                            // projected onto the generator card by
                            // `attach_audio_sends`, so this reuses the same
                            // stable payload path as effect cards.
                            let items: Vec<manifold_ui::panels::dropdown::DropdownItem> = if key == "audioSend" {
                                sp.dropdown_choices
                                    .iter()
                                    .map(|choice| {
                                        if choice.disabled {
                                            manifold_ui::panels::dropdown::DropdownItem::disabled(
                                                &choice.label,
                                            )
                                        } else {
                                            manifold_ui::panels::dropdown::DropdownItem::new(
                                                &choice.label,
                                            )
                                            .with_action(PanelAction::Params(
                                                ParamsAction::GenStringParamSelected(
                                                    *sp_idx,
                                                    choice.value.clone(),
                                                ),
                                            ))
                                        }
                                    })
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
                PanelAction::Project(ProjectAction::NewProject) => {
                    let action = self.project_io.new_project();
                    self.apply_project_io_action(action);
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
                project: self.local_project.edit(&mut self.ui_edits, content_tx),
                content_tx,
                content_state: &self.content_state,
                ui: &mut self.ws.ui_root,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer_id,
                user_prefs: &mut self.user_prefs,
                editor_target: None,
                scrub: &mut self.scrub,
                dialogs: crate::file_dialogs::host(&self.window_registry, self.dialog_window_id, self.primary_window_id, self.current_project_path.as_deref()),
            };
            let result = crate::ui_bridge::dispatch(action, &mut dctx);
            if let Some((kind, def, destination)) = result.begin_save_preset {
                self.begin_save_preset_prompt(kind, def, destination);
            }
            if let Some((kind, id, source, initial_name)) = result.begin_rename_preset {
                self.begin_rename_preset_prompt(kind, id, source, initial_name);
            }
        }

        // ── Editor inspector segment ────────────────────────────────────────
        // The graph-editor window hosts its OWN inspector instance, mirroring
        // the main window's selection and active layer; its actions dispatch
        // against the editor's tree (`editor_input::dispatch_card_lane`). Card
        // clicks also retarget the canvas to that card's graph, once the
        // editor borrow drops. See docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md.
        if !self.ws.ui_root.export_progress.is_open() && actions.len() > editor_card_seg_start {
            let (screen_w, screen_h) = self
                .graph_editor_window_id
                .and_then(|wid| self.window_registry.get(&wid))
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                })
                .unwrap_or((1280.0, 720.0));
            let card_actions = &actions[editor_card_seg_start..];
            let mappings = match self.graph_editor.as_mut() {
                Some(ed) => crate::editor_input::card_lane_entries(
                    &mut self.text_input,
                    ed,
                    &self.local_project,
                    &self.selection,
                    &self.active_layer_id,
                    card_actions,
                ),
                None => Vec::new(),
            };
            for (target, param_id, anchor_node_id) in mappings {
                self.open_card_mapping(target, param_id, anchor_node_id,
                    manifold_ui::graph_canvas::Rect::new(0.0, 0.0, screen_w, screen_h));
            }
            let outcome = match (self.graph_editor.as_mut(), self.content_tx.as_ref()) {
                (Some(ed), Some(content_tx)) => crate::editor_input::dispatch_card_lane(
                    crate::editor_input::CardLane {
                        ws: ed,
                        project: self.local_project.edit(&mut self.ui_edits, content_tx),
                        content_tx,
                        content_state: &self.content_state,
                        selection: &mut self.selection,
                        active_layer: &mut self.active_layer_id,
                        user_prefs: &mut self.user_prefs,
                        scrub: &mut self.scrub,
                        dialogs: crate::file_dialogs::host(&self.window_registry, self.dialog_window_id, self.primary_window_id, self.current_project_path.as_deref()),
                    },
                    card_actions,
                ),
                _ => Default::default(),
            };
            if let Some((kind, def, destination)) = outcome.save_preset {
                self.begin_save_preset_prompt(kind, def, destination);
            }
            // Retarget the canvas to the clicked card's graph (opening the window
            // stays a deliberate cog action, so only retarget when it's open).
            if self.graph_editor_window_id.is_some() {
                if let Some(ei) = outcome.retarget_effect {
                    if let Some(eid) = self.resolve_effect_card_id(ei) {
                        self.watch_effect_graph(eid);
                    }
                } else if outcome.retarget_generator
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
                    use manifold_node_engine::descriptor::{Category, descriptor_for};
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
                                // Node mode has no source concept
                                // (PRESET_LIBRARY_DESIGN P5, D6) — the
                                // graph-editor's node picker never renders
                                // the source row or the management menu.
                                source: None,
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
                    // next frame in present_graph_editor_window), so the
                    // anchor starts as a zero placeholder — the editor
                    // window's re-anchor pass below pins the overlay over
                    // the popup's real search bar as soon as the tree
                    // exists (same fix as the main window's open path).
                    self.text_input.begin_owned(
                        crate::text_input::TextSessionOwner::EditorOverlay(
                            crate::ui_root::OverlayId::BrowserPopup,
                        ),
                        crate::text_input::TextInputField::SearchFilter,
                        "",
                        crate::text_input::AnchorRect::zero(),
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
                        let mut cmd =
                            manifold_editing::commands::graph::RevertEffectGraphCommand::new(
                                eid.clone(),
                            );
                        if matches!(eid, manifold_core::GraphTarget::SceneModifier { .. }) {
                            let Some(id) = self.local_project.instance_preset_id(eid) else { continue; };
                            let Some(def) = manifold_nodes::bundled_presets::bundled_preset_def(&id) else {
                                log::error!("[preset] local modifier preset {id} is unavailable for revert");
                                continue;
                            };
                            cmd = cmd.with_resolved_def(def.as_ref().clone());
                        }
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
                        && let Some((def, _)) = crate::ui_bridge::preset_source_def(&target, &self.local_project)
                    {
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
                        && let Some((def, preset_id)) = crate::ui_bridge::preset_source_def(&target, &self.local_project)
                    {
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

                        if let Some(reason) = crate::scene_modifier_edit::node_parameter_lock_reason(
                            &self.local_project, &target, &canvas_scope, *node_id, param_name,
                        ) {
                            self.send_content_cmd(ContentCommand::GraphEditRejected(reason.into()));
                            continue;
                        }
                        let preparation = if matches!(
                            &target,
                            manifold_core::GraphTarget::SceneModifier { .. }
                        ) {
                            match crate::scene_modifier_edit::preparation_target_for_node_param(
                                &self.local_project,
                                &target,
                                &canvas_scope,
                                *node_id,
                                param_name,
                            ) {
                                Ok(target) => target,
                                Err(error) => {
                                    self.send_content_cmd(ContentCommand::GraphEditRejected(error));
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        if let Some(preparation) = preparation {
                            let core_value =
                                crate::ui_translate::serialized_param_value_to_core(new_value);
                            let Some(node_value) = serialized_value_as_f32(&core_value) else {
                                self.send_content_cmd(ContentCommand::GraphEditRejected(
                                    format!(
                                        "Preparation control {}.{} is not numeric",
                                        preparation.modifier_id, preparation.param_id
                                    ),
                                ));
                                continue;
                            };
                            let Some(preparation_value) = invert_preparation_value(
                                node_value,
                                &preparation,
                            ) else {
                                self.send_content_cmd(ContentCommand::GraphEditRejected(
                                    format!(
                                        "Preparation control {}.{} has an unrepresentable value",
                                        preparation.modifier_id, preparation.param_id
                                    ),
                                ));
                                continue;
                            };
                            let prep_key =
                                (preparation.modifier_id.clone(), preparation.param_id.clone());
                            let is_new_session = !matches!(
                                &self.scrub.active,
                                Some(crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(d))
                                    if d.target == target
                                        && d.node_id == *node_id
                                        && d.param_name == *param_name
                                        && d.scope_path == canvas_scope
                                        && d.preparation.as_ref() == Some(&prep_key)
                            );
                            if is_new_session {
                                self.scrub.check_single_active_on_begin("preparation-node-param");
                                self.scrub.active = Some(
                                    crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(
                                        Box::new(UnboundNodeParamDrag {
                                            target: target.clone(),
                                            node_id: *node_id,
                                            param_name: param_name.clone(),
                                            scope_path: canvas_scope.clone(),
                                            catalog_default: default.clone(),
                                            preparation: Some(prep_key),
                                            pre_drag_value: Some(
                                                manifold_core::effect_graph_def::SerializedParamValue::Float {
                                                    value: preparation.baseline,
                                                },
                                            ),
                                            current_value:
                                                manifold_core::effect_graph_def::SerializedParamValue::Float {
                                                    value: preparation_value,
                                                },
                                        }),
                                    ),
                                );
                            } else if let Some(
                                crate::ui_bridge::scrub::ResolvedScrub::UnboundNodeParam(drag),
                            ) = self.scrub.active.as_mut()
                            {
                                drag.current_value =
                                    manifold_core::effect_graph_def::SerializedParamValue::Float {
                                        value: preparation_value,
                                    };
                            }
                            // Preparation changes are drafts. No local or
                            // content live mutation occurs until End.
                            continue;
                        }
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
                                        .preset_instance(&target)
                                        .map(|inst| inst.get_base_param(&outer_id))
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
                                // push a cheap `MutateProject` (Values or Shapes) for
                                // the render — no undo-stack entry here,
                                // that's `EndGraphNodeParamScrub`'s job.
                                if let Some(tx) = self.content_tx.as_ref() {
                                    let t = target.clone();
                                    let oid = outer_id.clone();
                                    self.local_project.edit(&mut self.ui_edits, tx).gesture_move(
                                        crate::content_command::touch::VALUES,
                                        move |p| {
                                            p.with_preset_graph_mut(&t, |inst| {
                                                inst.set_base_param(&oid, card_value);
                                            });
                                        },
                                    );
                                }
                            }
                            // else: degenerate scale (D1 section 3) — read-only, no
                            // write; the row keeps showing the bound badge.
                            continue;
                        }
                        // BUG-282: mirror the bound arm above — one session
                        // opened on the first move, live-written every tick
                        // via `MutateProject` (Values or Shapes) (no undo entry), committed
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
                                        preparation: None,
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
                        let graph_absent = self
                            .local_project
                            .preset_instance(&target)
                            .is_none_or(|inst| inst.graph.is_none());
                        let mut live_cmd =
                            manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                                target,
                                *node_id,
                                param_name.clone(),
                                core_value,
                                default.clone(),
                            )
                            .with_scope(canvas_scope.clone());
                        // Unbound node param values are Shapes; the session's
                        // first move may materialize the override (D21).
                        let touches = crate::content_command::touch::shapes_first_move(
                            is_new_session && graph_absent,
                        );
                        self.send_content_cmd(ContentCommand::gesture_move(touches, Box::new(
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
                                    if let Some((modifier_id, param_id)) = drag.preparation {
                                        let Some(layer_id) = drag.target.host_target().and_then(
                                            |target| match target {
                                                manifold_core::GraphTarget::Generator(layer_id) => {
                                                    Some(layer_id.clone())
                                                }
                                                _ => None,
                                            },
                                        ) else {
                                            self.send_content_cmd(ContentCommand::GraphEditRejected(
                                                "Preparation target has no generator owner".into(),
                                            ));
                                            continue;
                                        };
                                        let Some(value) =
                                            serialized_value_as_f32(&drag.current_value)
                                        else {
                                            self.send_content_cmd(ContentCommand::GraphEditRejected(
                                                "Preparation value is not numeric".into(),
                                            ));
                                            continue;
                                        };
                                        self.send_content_cmd(ContentCommand::SceneModifier(
                                            crate::scene_modifier_edit::SceneModifierAction::Preparation(
                                                layer_id,
                                                modifier_id,
                                                param_id,
                                                value,
                                            ),
                                        ));
                                    } else {
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
                    // section 2): a group-face row mirrors an already-exposed card
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
                            manifold_core::GraphTarget::SceneModifier { .. } => {
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
                            project: self.local_project.edit(&mut self.ui_edits, content_tx),
                            content_tx,
                            content_state: &self.content_state,
                            ui: &mut self.ws.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_id,
                            user_prefs: &mut self.user_prefs,
                            editor_target,
                            scrub: &mut self.scrub,
                            dialogs: crate::file_dialogs::host(&self.window_registry, self.dialog_window_id, self.primary_window_id, self.current_project_path.as_deref()),
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
                    let host = crate::file_dialogs::host(
                        &self.window_registry,
                        self.dialog_window_id,
                        self.primary_window_id,
                        self.current_project_path.as_deref(),
                    );
                    let picked = if wants_folder {
                        crate::file_dialogs::pick_folder(
                            crate::file_dialogs::DialogAction::NodeFolder,
                            host,
                            &mut self.user_prefs,
                        )
                    } else {
                        crate::file_dialogs::pick_file(
                            crate::file_dialogs::DialogAction::NodeFile,
                            host,
                            &mut self.user_prefs,
                        )
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
                            manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.pbr_material"),
                            manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.transform_3d"),
                            manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.scene_object"),
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
                            manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.light"),
                            default.clone(),
                        );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
            }
        }

        // 2-4. Plan the frame, then apply it (`ui_frame::apply_frame_plan`,
        // shared with the script runner): projections, drag polling, the
        // planned region work, the `push_state` pieces, then the drag-time
        // clip and lane re-projection. The overlay intake and playhead follow
        // run first, so the plan sees what they move as inputs.
        crate::ui_bridge::intake_overlay_state(&mut self.ws.ui_root, &self.content_state);
        crate::ui_bridge::check_auto_scroll(&mut self.ws.ui_root, &self.content_state);
        let automation_draw_mode = self.selection.automation_draw_mode;
        let inputs = crate::ui_frame::gather_frame_inputs(
            &mut self.frame_planner,
            &self.ws.ui_root,
            &mut self.selection,
            &self.active_layer_id,
            &self.ui_edits,
            &self.content_state,
            crate::frame_plan::ChromeSource {
                project: &self.local_project,
                is_dirty: self.content_state.editing_is_dirty,
                project_path: self.current_project_path.as_deref(),
                automation_draw_mode,
            },
        );
        let plan = self.frame_planner.plan(inputs);
        let changes = crate::ui_frame::ProjectionChanges { scrolled_in_place };

        // Deferred export (keyboard shortcut sets the flag, processed here
        // where Application has full access for the file dialog).
        if self.pending_export {
            self.pending_export = false;
            self.start_export();
        }

        let content_tx = self.content_tx.as_ref();
        let overlay = &mut self.overlay;
        let cursor_pos = self.cursor_pos;
        let content_state = &self.content_state;
        let cursor_manager = &mut self.cursor_manager;
        let ui_edits = &mut self.ui_edits;
        let ui_profile = &mut self.ui_profile;
        let seg_ref = &mut seg;
        crate::ui_frame::apply_frame_plan(
            &mut self.ws.ui_root,
            self.graph_editor.as_mut().map(|ed| &mut ed.ui_root),
            self.ui_cache_manager.as_mut(),
            &mut self.local_project,
            &mut self.selection,
            &mut self.active_layer_id,
            &mut self.ws.offscreen_dirty,
            &plan,
            changes,
            crate::ui_frame::PushInputs {
                content_state,
                is_dirty: content_state.editing_is_dirty,
                project_path: self.current_project_path.as_deref(),
                transport_cache: &mut self.transport_cache,
            },
            |ui_root, project, selection, active_layer, redraw| {
                // Per-frame drag polling with auto-scroll (move/trim/rubber-band and
                // automation point/segment/group/marquee/pencil gestures).
                // Continues edge autoscroll when the mouse is stationary; also
                // drives the live readout, which must reflect the post-poll
                // (already-snapped) clip state.
                use manifold_ui::interaction_overlay::{DragMode, InteractionOverlay};
                let drag_mode = overlay.drag_mode();
                if matches!(
                    drag_mode,
                    DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight | DragMode::RegionSelect
                ) || InteractionOverlay::autoscrolls_at_edge(drag_mode)
                {
                    let tx = content_tx.expect("content thread spawned before a drag");
                    let mut host = crate::editing_host::AppEditingHost::new(
                        project.edit(ui_edits, tx),
                        tx,
                        content_state,
                        cursor_manager,
                        active_layer,
                        redraw,
                    );
                    overlay.poll_drag(cursor_pos, &mut host, selection, &mut ui_root.viewport);
                    let readout = overlay
                        .drag_readout_clip_id()
                        .and_then(|id| host.find_clip_by_id(&id))
                        .map(|c| (c.start_beat, c.duration_beats, c.layer_index));
                    ui_root.viewport.set_drag_readout(readout);
                } else {
                    ui_root.viewport.set_drag_readout(None);
                }
                ui_profile.add("process_events", seg_ref.elapsed());
                *seg_ref = std::time::Instant::now();
                matches!(overlay.drag_mode(), DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight)
            },
        );

        #[cfg(target_os = "macos")]
        self.sync_workspace_preview_size();

        self.ui_profile.add("rebuild_tree", seg.elapsed());
        seg = std::time::Instant::now();

        // 5. Push one per-frame metric snapshot to whichever HUDs are open.
        // The editor and main window must report the same content frame; keep
        // the snapshot shared before handing each panel its owned copy.
        let editor_perf_hud_visible = self
            .graph_editor
            .as_ref()
            .is_some_and(|ed| ed.ui_root.perf_hud.is_visible());
        if self.ws.ui_root.perf_hud.is_visible() || editor_perf_hud_visible {
            let bpm = Some(&self.local_project)
                .map(|p| p.settings.bpm)
                .unwrap_or(manifold_core::Bpm(120.0));
            let clock_source = Some(&self.local_project)
                .map(|p| p.settings.clock_authority.display_name())
                .unwrap_or("Internal");
            let metrics = perf_metrics_from_content_state(
                &self.content_state,
                self.frame_timer.current_fps() as f32,
                (self.frame_timer.last_dt() * 1000.0) as f32,
                bpm,
                clock_source,
            );
            if self.ws.ui_root.perf_hud.is_visible() {
                self.ws.ui_root.perf_hud.set_metrics(metrics);
            }
            if editor_perf_hud_visible
                && let Some(ed) = self.graph_editor.as_mut()
            {
                ed.ui_root.perf_hud.set_metrics(metrics);
            }
        }

        // 6. Lightweight update (playhead, insert cursor, layer selection, HUD values)
        self.ws.ui_root.update();

        // 6·drag-motion. P2 drag-visual tweens (`UI_CRAFT_AND_MOTION_PLAN.md`
        // D15/D17: grab lift, duplicate ghost, grid settle, landing-line
        // flash, error shake). The clip-body pass draws them at paint time, so
        // a live tween only asks for a repaint (D8). The drawer and object-card
        // tweens need nothing here: they move their nodes in place in the
        // panel update. The split tween moves the layout rects, which the next
        // plan lays out in place.
        if self.overlay.tick((dt * 1000.0) as f32) {
            self.ws.offscreen_dirty = true;
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
        // section 7.2 item 5): a currently-open fire-mode drawer (clip trigger or
        // `is_trigger_gate` param card) wins over the panel's own selected
        // send — collapsing it falls straight back to the panel's selection,
        // since this is computed fresh every frame, never persisted (section 7.3 P7
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
        // cursor / marker overlays are all GPU now (section 24 5b), so the grid is a pure
        // function of the viewport and needs no selection/hover state here.
        self.ws.ui_root.viewport.repaint_dirty_layers();

        // 6c. Upload dirty layer GRID textures + the lane/stem/overview/group
        // panel bitmaps to the single layer-bitmap instance (section 24 5b — the per-layer
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

            // 6h. LED composite preview band (LED_STRIPS_DESIGN MVP-P4): the
            // DMX lane's gen card holds the send path's latest readback as a
            // dirty-flagged CPU bitmap — upload on version change only; the
            // quad rides pass 4c (index 1003).
            if let Some((pixels, w, h)) = self.ws.ui_root.inspector.led_preview_bitmap() {
                bitmap_gpu.upload_layer(
                    &gpu.device,
                    manifold_ui::panels::param_card::LED_BAND_BITMAP_INDEX,
                    pixels,
                    w as u32,
                    h as u32,
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
            if self.ws.ui_root.atlas_dirty() {
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
/// The editor mini-timeline, keyed on what it reads: the Clips, Layers and
/// Settings stamps and the bar and beat the readout shows. A steady frame
/// rebuilds nothing (P4c).
#[derive(Default)]
pub(crate) struct MiniTimelineCache {
    key: Option<(u64, i64, i64)>,
    data: (Vec<manifold_ui::MiniClip>, Vec<manifold_ui::MiniLayerLabel>, usize, f32, f32, String),
}

impl MiniTimelineCache {
    pub(crate) fn get(
        &mut self,
        project: &manifold_core::project::Project,
        current_beat: f32,
        parts: &manifold_core::change::PartStamps,
    ) -> &(Vec<manifold_ui::MiniClip>, Vec<manifold_ui::MiniLayerLabel>, usize, f32, f32, String) {
        use manifold_core::change::{ChangeSet, Part};
        let bpb = project.settings.time_signature_numerator.max(1) as f32;
        let bar = (current_beat / bpb).floor() as i64 + 1;
        let beat_in_bar = (current_beat - (bar - 1) as f32 * bpb).floor() as i64 + 1;
        let key = (parts.key(ChangeSet::of(&[Part::Clips, Part::Layers, Part::Settings])), bar, beat_in_bar);
        if self.key != Some(key) {
            self.key = Some(key);
            self.data = mini_timeline_data(project, current_beat);
        }
        &self.data
    }

    /// What the cached strip was built from: the clip, layer and settings
    /// stamps and the shown bar and beat. A window keys its redraw on it.
    pub(crate) fn key(&self) -> Option<(u64, i64, i64)> {
        self.key
    }

    /// The strip as the last [`Self::get`] left it.
    pub(crate) fn data(&self) -> &(Vec<manifold_ui::MiniClip>, Vec<manifold_ui::MiniLayerLabel>, usize, f32, f32, String) {
        &self.data
    }
}

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
    tm: &dyn manifold_ui::text::TextMeasure,
) {
    use crate::text_input::*;

    let fs = ti.font_size;
    let width = |s: &str| tm.measure_text(s, fs as u16, FontWeight::Medium).x;
    let mut lines = Vec::new();
    let g = ti.overlay_geometry(&mut lines, &mut |s| width(s));
    let line_h = g.line_h;

    let text = ti.model.text();
    let sel = ti.model.selection();
    let has_selection = ti.model.has_selection();
    let caret = ti.model.caret();

    ui.draw_bordered_rect(
        g.x,
        g.y,
        g.w,
        g.h,
        TEXT_INPUT_BG,
        3.0,
        1.0,
        manifold_ui::Color32::new(89, 115, 179, 204), // sRGB, was [0.35, 0.45, 0.7, 0.8]
    );

    // Clip to the box: a long single-line field scrolls, so its text and
    // selection run past the edges.
    ui.push_immediate_clip(g.x + 1.0, g.y + 1.0, g.w - 2.0, g.h - 2.0);
    for (i, l) in lines.iter().enumerate() {
        let ly = g.text_y + i as f32 * line_h;
        let line = &text[l.clone()];
        if has_selection && sel.start < l.end && sel.end > l.start {
            let hl_start = sel.start.max(l.start) - l.start;
            let hl_end = sel.end.min(l.end) - l.start;
            let hx = g.text_x + width(&line[..hl_start]);
            let hw = width(&line[..hl_end]) - width(&line[..hl_start]);
            ui.draw_rect(hx, ly, hw.max(2.0), line_h, TEXT_INPUT_SELECT_BG);
        }
        ui.draw_text(g.text_x, ly, line, fs, TEXT_INPUT_FG);
    }

    if !has_selection {
        let elapsed = timer.realtime_since_start();
        let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
        if blink_on {
            let i = manifold_ui::text_edit::line_of(&lines, caret);
            let cursor_x = g.text_x + width(&text[lines[i].start..caret]);
            let cursor_y = g.text_y + i as f32 * line_h;
            ui.draw_rect(cursor_x, cursor_y, TEXT_INPUT_CURSOR_W, line_h, TEXT_INPUT_CURSOR);
        }
    }
    ui.pop_immediate_clip();
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
            tooltip: None,
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
            card_visible: true,
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
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
/// pattern: N in-flight ticks land via `MutateProject` (Values or Shapes) — a direct
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
            scene_modifiers: Vec::new(),
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
        // `MutateProject` (Values or Shapes) path (`live_cmd.execute(p)` in the
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

#[cfg(test)]
mod live_sim_hud_flow_tests {
    use super::perf_metrics_from_content_state;
    use crate::content_state::ContentState;
    use crate::ui_root::UIRoot;

    fn hud_texts(root: &UIRoot) -> Vec<String> {
        root.tree
            .all_nodes()
            .filter_map(|node| node.text.clone())
            .collect()
    }

    #[test]
    fn content_snapshot_updates_main_and_editor_huds_then_holds_when_paused() {
        let mut main = UIRoot::new();
        main.resize(640.0, 480.0);
        main.perf_hud.toggle();
        main.build();

        let mut editor = UIRoot::new();
        editor.perf_hud.toggle();

        let mut snapshot = ContentState {
            is_playing: true,
            physics_backlog_seconds: 0.125,
            sim_step_cap_hit: true,
            sim_nonfinite: true,
            ..ContentState::default()
        };

        for (expected_lag, expected_cap, expected_state) in [
            ("sim behind by 125.0 ms", "step cap: WARNING — CAP HIT", "sim state: ERROR — SHOW RUNNING"),
            ("sim behind by 250.0 ms", "step cap: OK", "sim state: OK"),
            ("sim behind by 250.0 ms", "step cap: OK", "sim state: OK"),
        ] {
            let metrics = perf_metrics_from_content_state(
                &snapshot,
                60.0,
                16.6,
                manifold_core::Bpm(snapshot.bpm as f32),
                "Internal",
            );
            main.perf_hud.set_metrics(metrics);
            editor.perf_hud.set_metrics(metrics);

            // The main window uses UIRoot::update(), while the cacheless graph
            // editor uses its targeted presentation tick after rebuilding its
            // overlay tree. Exercise both production update paths.
            main.update();
            editor.tree.clear();
            editor.build_overlays_for_screen(640.0, 480.0);
            editor.tick_editor_perf_hud();

            for texts in [hud_texts(&main), hud_texts(&editor)] {
                assert!(texts.iter().any(|text| text == expected_lag));
                assert!(texts.iter().any(|text| text == expected_cap));
                assert!(texts.iter().any(|text| text == expected_state));
            }

            snapshot.physics_backlog_seconds = 0.25;
            snapshot.is_playing = false;
            snapshot.sim_step_cap_hit = false;
            snapshot.sim_nonfinite = false;
        }
    }
}
