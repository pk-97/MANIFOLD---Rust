//! Implementation of TimelineInputHost for the Application.
//!
//! Wraps Application fields to implement the TimelineInputHost trait.
//! Same split-borrow pattern as AppEditingHost — borrows individual fields
//! so InputHandler, UIState, and viewport can be borrowed separately.
use manifold_core::{Beats, ClipId, LayerId, Seconds};
use manifold_core::effects::SegmentShape;
use manifold_editing::command::Command;
use manifold_editing::commands::clip::MuteClipCommand;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::RemoveEffectCommand;
use manifold_editing::service::EditingService;
use manifold_ui::InspectorTab;
use manifold_ui::cursor_nav;
use manifold_ui::timeline_input_host::TimelineInputHost;
use manifold_ui::ui_state::UIState;
use manifold_ui::view::UiSegmentShape;

use crate::content_command::ContentCommand;
use crate::ui_root::UIRoot;

pub(crate) mod automation;

/// Wrapper implementing TimelineInputHost by borrowing Application fields.
///
/// Selection (UIState) is available for host methods that need to read/write
/// selection state (paste, duplicate, navigate_cursor, select_all, etc.).
pub struct AppInputHost<'a> {
    pub project: &'a mut manifold_core::project::Project,
    pub content_tx: &'a crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    pub content_state: &'a crate::content_state::ContentState,
    pub ui_root: &'a mut UIRoot,
    pub selection: &'a mut UIState,
    pub active_layer: &'a mut Option<LayerId>,
    pub needs_rebuild: &'a mut bool,
    pub needs_structural_sync: &'a mut bool,
    pub scroll_dirty: &'a mut crate::ui_root::ScrollDirty,
    #[cfg_attr(not(feature = "profiling"), allow(dead_code))]
    pub current_project_path: &'a Option<std::path::PathBuf>,
    pub has_output_window: bool,
    pub pending_close_output: &'a mut bool,
    pub pending_export: &'a mut bool,
    /// D5 (docs/TIMELINE_INGEST_DESIGN.md): Finder-pasted files route through
    /// the same ingest path a Finder drag-drop uses.
    pub project_io: &'a mut crate::project_io::ProjectIOService,
    /// D4: the general pasteboard's changeCount snapshotted at the last
    /// internal clip copy. Read/written only on macOS — the underlying
    /// AppKit pasteboard type doesn't exist elsewhere, and the arbitration
    /// always keeps the internal path on other platforms (see
    /// `pasteboard_change_count`).
    #[cfg(target_os = "macos")]
    pub internal_clipboard_change_count: &'a mut Option<i64>,
}

impl TimelineInputHost for AppInputHost<'_> {
    fn handle_inspector_keyboard(&mut self) -> bool {
        // Future: inspector arrow key stepping for loop duration.
        // Stub returns false — correct until clip inspector is ported.
        false
    }

    fn toggle_performance_hud(&mut self) {
        self.ui_root.perf_hud.toggle();
        *self.needs_rebuild = true;

        // When profiling feature is enabled, toggling the perf HUD also
        // starts/stops a profiling session on the content thread.
        #[cfg(feature = "profiling")]
        {
            if self.ui_root.perf_hud.is_visible() {
                // Starting — send project info for session metadata
                let (project_name, resolution, target_fps) = (
                    self.project.project_name.clone(),
                    (
                        self.project.settings.output_width as u32,
                        self.project.settings.output_height as u32,
                    ),
                    self.project.settings.frame_rate,
                );
                let project_path = self
                    .current_project_path
                    .as_ref()
                    .map_or_else(String::new, |p| p.display().to_string());
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::StartProfiling {
                        project_name,
                        project_path,
                        resolution,
                        target_fps,
                        gpu_name: String::from("Metal GPU"),
                    },
                );
            } else {
                // Stopping — dump session
                ContentCommand::send(self.content_tx, ContentCommand::StopProfiling);
            }
        }
    }

    fn is_monitor_output_active(&self) -> bool {
        self.has_output_window
    }

    fn close_output_window(&mut self) {
        *self.pending_close_output = true;
    }

    fn request_rebuild(&mut self) {
        *self.needs_rebuild = true;
    }

    fn on_undo_redo(&mut self) {
        // Unity WorkspaceController lines 378-386:
        //   needsRebuild = true; RefreshAllInspectors();
        //   playbackController.RefreshActiveClips(); playbackController.MarkCompositorDirty();
        //   ApplyProjectResolutionFromFooter(); ApplyProjectFpsFromFooter();
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );

        // TODO: Re-apply resolution/FPS from project settings after undo/redo.
        // Unity calls ApplyProjectResolutionFromFooter() and ApplyProjectFpsFromFooter()
        // to sync render pipeline with potentially changed settings.
        // Requires PlaybackEngine.set_resolution()/set_fps() (not yet ported).

        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn on_selection_cleared(&mut self) {
        // Unity WorkspaceController.OnSelectionCleared (lines 388-393):
        //   InvalidateAllLayerBitmaps();
        //   ResetAllInspectors();
        //   masterInspector?.Show();
        let ui = &mut self.ui_root;
        ui.inspector.clear_effect_selection(&mut ui.tree);
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
        self.scroll_dirty.visual = true;
    }

    fn mark_compositor_dirty(&mut self) {
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );
    }

    fn invalidate_all_layer_bitmaps(&mut self) {
        self.scroll_dirty.visual = true;
    }

    fn update_zoom_label(&mut self) {
        // Zoom label is updated during push_state
    }

    fn get_playhead_viewport_x(&self) -> f32 {
        let beat = self.content_state.current_beat.as_f32();
        let ppb = self.ui_root.viewport.pixels_per_beat();
        let scroll = self.ui_root.viewport.scroll_x_beats().as_f32();
        (beat - scroll) * ppb
    }

    fn get_viewport_width(&self) -> f32 {
        self.ui_root.viewport.tracks_rect().width
    }

    fn get_seconds_per_beat(&self) -> f32 {
        let bpm = Some(&*self.project)
            .map(|p| p.settings.bpm.0)
            .unwrap_or(120.0);
        if bpm > 0.0 { 60.0 / bpm } else { 0.5 }
    }

    fn on_clip_selected(&mut self, _clip_id: &str) {
        *self.needs_structural_sync = true;
    }

    // ── Effect keyboard shortcuts (Unity EffectSelectionManager) ──

    fn handle_effect_select_all(&mut self) -> bool {
        let selected = if self.ui_root.inspector.has_modifier_selection() {
            self.ui_root.inspector.select_all_modifiers()
        } else { self.ui_root.inspector.select_all_effects() };
        if selected {
            self.ui_root
                .inspector
                .apply_selection_visuals(&mut self.ui_root.tree);
        }
        selected
    }

    fn handle_effect_copy(&mut self) -> bool {
        // An effect selection takes precedence on a scene layer: the modifier
        // scope is also present while the generator card is displayed.
        if self.ui_root.inspector.has_effect_selection() {
            let tab = self.ui_root.inspector.last_effect_tab();
            let indices = self.ui_root.inspector.get_selected_effect_indices();
            let effects = resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection);
            if let Some(effects) = effects {
                let selected: Vec<_> = indices
                    .iter()
                    .filter_map(|&i| effects.get(i).cloned())
                    .collect();
                if selected.len() == 1 {
                    self.ui_root.effect_clipboard.copy_single(&selected[0]);
                } else if !selected.is_empty() {
                    self.ui_root.effect_clipboard.copy_all(&selected);
                }
                if !selected.is_empty() {
                    self.ui_root.scene_modifier_clipboard = None;
                }
                return !selected.is_empty();
            }
            return false;
        }
        // Selected modifiers own Cmd+C within their generator scope.
        if let Some(layer) = self.ui_root.inspector.modifier_scope_id().cloned()
            && !self.ui_root.inspector.has_effect_selection()
            && self.ui_root.inspector.has_modifier_selection()
        {
            let selected = self.ui_root.inspector.selected_modifier_ids();
            match crate::scene_modifier_transfer::ModifierClipboard::capture(
                self.project,
                &layer,
                &selected,
            ) {
                Ok(clipboard) => self.ui_root.set_scene_modifier_clipboard(Some(clipboard)),
                Err(reason) => ContentCommand::send(
                    self.content_tx,
                    ContentCommand::GraphEditRejected(reason),
                ),
            }
            return true;
        }
        false
    }

    fn handle_effect_cut(&mut self) -> bool {
        if !self.ui_root.inspector.has_effect_selection() {
            return false;
        }
        let tab = self.ui_root.inspector.last_effect_tab();
        let indices = self.ui_root.inspector.get_selected_effect_indices();
        let target = resolve_effect_target(tab, &*self.active_layer, self.selection);

        // Copy first
        let effects = resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection);
        if let Some(effects) = effects {
            let selected: Vec<_> = indices
                .iter()
                .filter_map(|&i| effects.get(i).cloned())
                .collect();
            if selected.len() == 1 {
                self.ui_root.effect_clipboard.copy_single(&selected[0]);
            } else if !selected.is_empty() {
                self.ui_root.effect_clipboard.copy_all(&selected);
            }
            if !selected.is_empty() {
                self.ui_root.scene_modifier_clipboard = None;
            }
        }

        // Remove in reverse index order (Unity lines 242-246)
        for &idx in indices.iter().rev() {
            let effects_slice =
                resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection);
            if let Some(effects) = effects_slice
                && let Some(fx) = effects.get(idx)
            {
                let cmd = RemoveEffectCommand::new(target.clone(), fx.clone(), idx);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(self.project);
                ContentCommand::send(self.content_tx, ContentCommand::Execute(boxed));
            }
        }

        let ui = &mut self.ui_root;
        ui.inspector.clear_effect_selection(&mut ui.tree);
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }

    fn handle_effect_paste(&mut self) -> bool {
        // Paste remains in the scene-modifier context, including an empty
        // destination stack. The content thread owns the actual mutation.
        if let Some(layer) = self.ui_root.inspector.modifier_scope_id().cloned()
            && !self.ui_root.inspector.has_effect_selection()
            && (self.ui_root.inspector.has_modifier_selection()
                || self.ui_root.scene_modifier_clipboard.is_some())
        {
            if let Some(clipboard) = self.ui_root.scene_modifier_clipboard.clone() {
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::SceneModifier(
                        crate::scene_modifier_edit::SceneModifierAction::Paste(
                            layer,
                            clipboard,
                        ),
                    ),
                );
                *self.needs_structural_sync = true;
                *self.needs_rebuild = true;
            }
            return true;
        }
        if !self.ui_root.effect_clipboard.has_content() {
            return false;
        }
        let tab = self.ui_root.inspector.last_effect_tab();
        let target = resolve_effect_target(tab, &*self.active_layer, self.selection);

        // Insert after last selected card, or append to end (Unity lines 257-263)
        let indices = self.ui_root.inspector.get_selected_effect_indices();
        let effects_len =
            resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection)
                .map(|e| e.len())
                .unwrap_or(0);
        let insert_at = if let Some(&last) = indices.last() {
            last + 1
        } else {
            effects_len
        };

        let clones = self.ui_root.effect_clipboard.get_paste_clones();
        for (offset, fx) in clones.into_iter().enumerate() {
            // Fresh, independent copy: new EffectId + dropped hardware bindings.
            // Drop group membership too — this is a cross-chain paste, so the
            // source's group doesn't exist in the destination chain.
            let mut fx = fx.duplicated();
            fx.group_id = None;
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target.clone(),
                fx,
                insert_at + offset,
            );
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            boxed.execute(self.project);
            ContentCommand::send(self.content_tx, ContentCommand::Execute(boxed));
        }

        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }

    fn handle_effect_delete(&mut self) -> bool {
        if self.ui_root.inspector.has_modifier_selection() {
            if let Some(layer) = self.ui_root.inspector.modifier_scope_id().cloned() {
                let ids = self.ui_root.inspector.selected_modifier_ids();
                ContentCommand::send(self.content_tx, ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::RemoveMany(layer, ids)));
                *self.needs_structural_sync = true;
                *self.needs_rebuild = true;
            }
            return true;
        }
        if !self.ui_root.inspector.has_effect_selection() {
            return false;
        }
        let tab = self.ui_root.inspector.last_effect_tab();
        let indices = self.ui_root.inspector.get_selected_effect_indices();
        let target = resolve_effect_target(tab, &*self.active_layer, self.selection);

        // Collect commands in reverse index order (Unity lines 274-289)
        let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
        for &idx in indices.iter().rev() {
            let effects_slice =
                resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection);
            if let Some(effects) = effects_slice
                && let Some(fx) = effects.get(idx)
            {
                commands.push(Box::new(RemoveEffectCommand::new(
                    target.clone(),
                    fx.clone(),
                    idx,
                )));
            }
        }

        if !commands.is_empty() {
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::ExecuteBatch(
                    commands,
                    "Delete effects".into(),
                ),
            );
        }

        let ui = &mut self.ui_root;
        ui.inspector.clear_effect_selection(&mut ui.tree);
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }

    fn handle_effect_group(&mut self) -> bool {
        let tab = self.ui_root.inspector.last_effect_tab();
        let indices = self.ui_root.inspector.get_selected_effect_indices();
        if indices.is_empty() {
            return false;
        }
        let target = resolve_effect_target(tab, &*self.active_layer, self.selection);
        let cmd = manifold_editing::commands::effect_groups::GroupEffectsCommand::new(
            target,
            indices,
            "Modifier Group".to_string(),
        );
        let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::ExecuteOnContent(boxed),
        );
        *self.needs_rebuild = true;
        true
    }

    fn handle_effect_ungroup(&mut self) -> bool {
        let tab = self.ui_root.inspector.last_effect_tab();
        let indices = self.ui_root.inspector.get_selected_effect_indices();
        if indices.is_empty() {
            return false;
        }
        let primary_idx = indices[0];
        let target = resolve_effect_target(tab, &*self.active_layer, self.selection);
        // Get the group_id of the primary selected effect
        let effects = resolve_effects_ref(tab, self.project, &*self.active_layer, self.selection);
        let group_id = effects
            .and_then(|e| e.get(primary_idx))
            .and_then(|fx| fx.group_id.clone());
        if let Some(gid) = group_id {
            let cmd =
                manifold_editing::commands::effect_groups::UngroupEffectsCommand::new(target, gid);
            let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::ExecuteOnContent(boxed),
            );
            *self.needs_rebuild = true;
            true
        } else {
            false
        }
    }

    fn clear_effect_selection(&mut self) {
        let ui = &mut self.ui_root;
        ui.inspector.clear_effect_selection(&mut ui.tree);
        *self.needs_rebuild = true;
    }

    fn show_toast(&mut self, message: &str) {
        log::info!("[Toast] {}", message);
    }

    fn undo(&mut self) {
        self.selection.clear_automation_selection();
        crate::ui_bridge::undo(self.content_tx);
    }

    fn redo(&mut self) {
        self.selection.clear_automation_selection();
        crate::ui_bridge::redo(self.content_tx);
    }

    fn save_project(&mut self) {
        // Save requires the rfd dialog and window handle, which are owned by
        // Application (not borrowed here). For now, the actual save logic
        // stays in the legacy block in app.rs. InputHandler returns false
        // for Cmd+S so it falls through to the legacy handler.
        //
        // TODO: When the legacy block is deleted, refactor save to use a
        // flag that Application picks up after the host call returns.
        log::info!("Save requested via keyboard shortcut");
    }

    fn open_project(&mut self) {
        // Same as save — needs rfd dialog + window handle.
        log::info!("Open requested via keyboard shortcut");
    }

    fn new_project(&mut self) {
        // Same as save — needs to create project + initialize engine.
        log::info!("New project requested via keyboard shortcut");
    }

    fn play_pause(&mut self, insert_cursor_beat: Option<Beats>) {
        if self.content_state.is_playing {
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Pause,
            );
        } else {
            // Unity: if paused and insert cursor exists, seek to cursor first (Ableton behavior)
            if let Some(beat) = insert_cursor_beat {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::SeekToBeat(beat),
                );
            }
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Play,
            );
        }
    }

    fn seek_to(&mut self, time: Seconds) {
        let mut sought_beat = None;
        if time.0 == f64::MAX {
            // Sentinel for "seek to end" — Unity InputHandler line 380-390
            let mut max_beat = Beats::ZERO;
            for layer in &self.project.timeline.layers {
                for clip in &layer.clips {
                    let end = clip.start_beat + clip.duration_beats;
                    if end > max_beat {
                        max_beat = end;
                    }
                }
            }
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::SeekToBeat(max_beat),
            );
            sought_beat = Some(max_beat);
        } else {
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::SeekTo(time),
            );
            if time == Seconds::ZERO {
                sought_beat = Some(Beats::ZERO);
            }
        }

        // Home/End must leave the sought position visible. Reuse the viewport's
        // anchored-zoom entry point with its current zoom; this preserves the
        // vertical scroll owned by the viewport while moving only horizontally.
        if let Some(beat) = sought_beat {
            let viewport = &mut self.ui_root.viewport;
            let before = viewport.scroll_x_beats();
            let tracks = viewport.tracks_rect();
            let visible_beats = tracks.width / viewport.pixels_per_beat().max(f32::EPSILON);
            let start = before.as_f32();
            let end = start + visible_beats.max(0.0);
            let target = beat.as_f32();
            if target < start || target > end {
                viewport.zoom_to(viewport.pixels_per_beat(), target, tracks.x);
                if viewport.scroll_x_beats() != before {
                    self.scroll_dirty.scroll_x = true;
                }
            }
        }
    }

    fn current_beat(&self) -> f32 {
        self.content_state.current_beat.as_f32()
    }

    fn is_playing(&self) -> bool {
        self.content_state.is_playing
    }

    fn select_all_clips(&mut self) {
        // Unity EditingService.SelectAllClips (lines 264-276). D1: select-all is
        // a pure `Clips` selection — no bounding region is synthesised (the old
        // `update_region_from_clip_selection_inline` sync is deleted).
        if let Some(project) = Some(&*self.project) {
            let ids: Vec<ClipId> = project
                .timeline
                .layers
                .iter()
                .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                .collect();
            self.selection.select_clips(ids);
        }
        *self.needs_structural_sync = true;
    }

    fn copy_clips(&mut self, clip_ids: &[ClipId]) {
        // Send copy to content thread (EditingService owns the clipboard)
        let region = self
            .selection
            .current_region()
            .map(crate::ui_translate::selection_region_to_core);
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::CopyClips {
                clip_ids: clip_ids.to_vec(),
                region,
            },
        );
        self.snapshot_pasteboard_change_count();
    }

    fn cut_clips(&mut self, clip_ids: &[ClipId], has_region: bool) {
        // Copy first (via content thread), then delete locally + record commands
        let region = if has_region {
            self.selection
                .current_region()
                .map(crate::ui_translate::selection_region_to_core)
        } else {
            None
        };
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::CopyClips {
                clip_ids: clip_ids.to_vec(),
                region,
            },
        );
        // Delete from local project + send commands to content thread
        let project = &mut *self.project;
        let spb = 60.0 / project.settings.bpm.0.max(1.0);
        let del_region = if has_region {
            self.selection
                .current_region()
                .map(crate::ui_translate::selection_region_to_core)
        } else {
            None
        };
        let commands = EditingService::delete_clips(project, clip_ids, del_region.as_ref(), spb);
        if !commands.is_empty() {
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::ExecuteBatch(
                    commands,
                    "Delete clips".into(),
                ),
            );
        }
        self.selection.clear_selection();
        self.snapshot_pasteboard_change_count();
    }

    fn paste_clips(&mut self, target_beat: f32, target_layer: i32) {
        // Send paste to content thread and wait for result (pasted clip IDs)
        let (tx, rx) = std::sync::mpsc::channel();
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::PasteClips {
                target_beat: Beats::from_f32(target_beat),
                target_layer,
                result_tx: tx,
            },
        );
        // Wait briefly for pasted IDs to select them in the UI
        if let Ok(pasted_ids) = rx.recv_timeout(std::time::Duration::from_millis(100))
            && !pasted_ids.is_empty()
        {
            self.selection.select_clips(pasted_ids);
        }
        *self.needs_structural_sync = true;
    }

    fn duplicate_clips(&mut self, clip_ids: &[ClipId]) {
        // Unity EditingService.DuplicateSelectedClips (lines 678-781):
        // Region mode: use the ACTUAL UI region (preserves gaps/spacing).
        // Individual mode: offset by the clips' own span.
        // After region duplicate, shift the region forward (Ableton-style).
        if let Some(project) = Some(&mut *self.project) {
            // D1: a region exists only when the selection is a `TimeRange`; a
            // clip selection yields the default (inactive) region → individual
            // (clip-span) duplicate mode, which is the correct D3 behaviour.
            let region = self.selection.current_region().cloned().unwrap_or_default();
            let used_region_mode = region.is_active;

            // Snapshot existing IDs to find new ones after execute
            let before_ids: std::collections::HashSet<ClipId> = project
                .timeline
                .layers
                .iter()
                .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                .collect();

            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            let region_core = crate::ui_translate::selection_region_to_core(&region);
            let mut commands =
                EditingService::duplicate_clips(project, clip_ids, &region_core, spb);
            if !commands.is_empty() {
                // Execute locally for read-back (need new clip IDs for selection).
                for c in commands.iter_mut() {
                    c.execute(project);
                }
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(
                        commands,
                        "Duplicate clips".into(),
                    ),
                );

                // Find newly created clips and select them
                let new_ids: Vec<ClipId> = project
                    .timeline
                    .layers
                    .iter()
                    .flat_map(|l| {
                        l.clips
                            .iter()
                            .filter(|c| !before_ids.contains(&c.id))
                            .map(|c| c.id.clone())
                    })
                    .collect();

                // Select the copies (a pure `Clips` selection).
                self.selection.select_clips(new_ids.clone());

                if used_region_mode && !new_ids.is_empty() {
                    // Region-mode duplicate: shift the region forward by its
                    // duration (Ableton-style, Unity lines 743-758). `set_region`
                    // installs the shifted `TimeRange`, replacing the `Clips`
                    // selection just set above — matching the old behaviour where
                    // `set_region` cleared the freshly inserted clip ids.
                    let duration = region.duration_beats();
                    let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
                    let (lo, hi) = region.layer_index_range(&ui_layers).unwrap_or((0, 0));
                    self.selection.set_region(
                        region.end_beat,
                        region.end_beat + duration,
                        lo as i32,
                        hi as i32,
                        &ui_layers,
                    );
                }
                // Individual mode: the `Clips` selection of the copies stands as
                // set — D1 no longer synthesises a bounding region from it.
            }
        }
        *self.needs_structural_sync = true;
    }

    fn delete_clips(&mut self, clip_ids: &[ClipId], has_region: bool) {
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm.0;
            // Step 4i: pass actual region from UIState when active
            let region = if has_region {
                self.selection
                    .current_region()
                    .map(crate::ui_translate::selection_region_to_core)
            } else {
                None
            };
            let commands = EditingService::delete_clips(project, clip_ids, region.as_ref(), spb);
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(
                        commands,
                        "Delete clips".into(),
                    ),
                );
            }
        }
        *self.needs_structural_sync = true;
    }

    fn delete_layer(&mut self, layer_index: usize) {
        if let Some(project) = Some(&mut *self.project)
            && project.timeline.layers.len() > 1
            && let Some(layer) = project.timeline.layers.get(layer_index)
        {
            let layer_clone = layer.clone();
            let cmd = manifold_editing::commands::layer::DeleteLayerCommand::new(layer_clone);
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Execute(Box::new(cmd)),
            );
        }
        *self.needs_rebuild = true;
    }

    fn split_clips_at_playhead(&mut self, clip_ids: &[ClipId]) {
        let beat = self.content_state.current_beat.as_f32();
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm.0;
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for id in clip_ids {
                if let Some(cmd) = EditingService::split_clip_at_beat(
                    project,
                    id,
                    manifold_core::Beats::from_f32(beat),
                    spb,
                ) {
                    // D17 "clip split flick": both resulting ids are known
                    // synchronously (the tail clip's id is minted client-side
                    // in `split_clip_at_beat`, not round-tripped from the
                    // content thread) — fire the visual now, ahead of the
                    // command actually executing.
                    self.ui_root
                        .viewport
                        .fire_split_flick(id.clone(), cmd.tail_clip_id().clone());
                    commands.push(Box::new(cmd));
                }
            }
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
    }

    fn extend_clips(&mut self, clip_ids: &[ClipId], grid_step: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let commands = EditingService::extend_clips_by_grid(
                project,
                clip_ids,
                manifold_core::Beats::from_f32(grid_step),
            );
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
    }

    fn shrink_clips(&mut self, clip_ids: &[ClipId], grid_step: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let commands = EditingService::shrink_clips_by_grid(
                project,
                clip_ids,
                manifold_core::Beats::from_f32(grid_step),
            );
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
    }

    fn nudge_clips(&mut self, clip_ids: &[ClipId], beat_delta: f32) {
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm.0;
            let commands = EditingService::nudge_clips(
                project,
                clip_ids,
                manifold_core::Beats::from_f32(beat_delta),
                spb,
            );
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
        *self.needs_structural_sync = true;
    }

    fn move_selection_across_layers(&mut self, clip_ids: &[ClipId], layer_delta: i32) {
        if let Some(project) = Some(&mut *self.project) {
            let spb = 60.0 / project.settings.bpm.0;
            let commands =
                EditingService::move_clips_across_layers(project, clip_ids, layer_delta, spb);
            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
        *self.needs_structural_sync = true;
    }

    fn toggle_mute_clips(&mut self, clip_ids: &[ClipId]) {
        // Unity EditingService.ToggleMuteSelectedClips (line 418-448):
        // Group-mute semantics: if ANY unmuted → mute ALL, else unmute ALL.
        // Records undo via MuteClipCommand. Marks compositor dirty.
        if let Some(project) = Some(&mut *self.project) {
            // First pass: collect current mute state for each clip
            let mut clip_states: Vec<(ClipId, bool)> = Vec::new();
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip_ids.contains(&clip.id) {
                        clip_states.push((clip.id.clone(), clip.is_muted));
                    }
                }
            }

            // Determine target: if ANY unmuted → mute all, else unmute all
            let any_unmuted = clip_states.iter().any(|(_, muted)| !muted);
            let new_muted = any_unmuted;

            // Build commands for clips that need to change
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for (id, old_muted) in &clip_states {
                if *old_muted != new_muted {
                    commands.push(Box::new(MuteClipCommand::new(
                        id.clone(),
                        *old_muted,
                        new_muted,
                    )));
                }
            }

            if !commands.is_empty() {
                let _label = if new_muted {
                    "Mute clips"
                } else {
                    "Unmute clips"
                };
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn group_selected_layers(&mut self) {
        // Port of Unity EditingService.GroupSelectedLayers.
        // Requires >= 2 selected, none nested or already groups.
        if self.selection.layer_selection_count() < 2 {
            return;
        }

        let selected_ids: Vec<LayerId> =
            self.selection.selected_layer_ids.iter().cloned().collect();

        if let Some(project) = Some(&mut *self.project) {
            // Validate: none are nested (have parent) or group layers
            let mut layers_to_group = Vec::new();
            for layer in &project.timeline.layers {
                if selected_ids.contains(&layer.layer_id) {
                    if layer.parent_layer_id.is_some() || layer.is_group() {
                        return; // Validation failure
                    }
                    layers_to_group.push(layer.layer_id.clone());
                }
            }
            if layers_to_group.len() < 2 {
                return;
            }

            // Snapshot current order for undo
            let original_order = project.timeline.layers.clone();
            let cmd = manifold_editing::commands::layer::GroupLayersCommand::new(
                layers_to_group,
                original_order,
            );
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            boxed.execute(project);
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Execute(boxed),
            );
        }

        self.selection.clear_selection();
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn ungroup_selected_layers(&mut self) {
        // If exactly one layer is selected and it's a group, dissolve it.
        if self.selection.layer_selection_count() != 1 {
            return;
        }
        let selected_id = self.selection.selected_layer_ids.iter().next().cloned();
        if let Some(id) = selected_id
            && let Some(project) = Some(&mut *self.project)
        {
            let layer = match project.timeline.layers.iter().find(|l| l.layer_id == id) {
                Some(l) => l,
                None => return,
            };
            if !layer.is_group() {
                return;
            }
            let group_layer = layer.clone();
            let group_id = group_layer.layer_id.clone();
            let child_ids: Vec<LayerId> = project
                .timeline
                .layers
                .iter()
                .filter(|l| l.parent_layer_id.as_ref() == Some(&group_id))
                .map(|l| l.layer_id.clone())
                .collect();
            let original_order = project.timeline.layers.clone();
            let cmd = manifold_editing::commands::layer::UngroupLayersCommand::new(
                group_layer,
                child_ids,
                original_order,
            );
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            boxed.execute(project);
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::Execute(boxed),
            );
        }

        self.selection.clear_selection();
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn delete_selected_layers(&mut self) {
        // Port of Unity EditingService.DeleteSelectedLayers.
        // Deletes selected layers in reverse index order, preserves at least 1 layer.
        if self.selection.layer_selection_count() == 0 {
            return;
        }

        let selected_ids: Vec<LayerId> =
            self.selection.selected_layer_ids.iter().cloned().collect();

        if let Some(project) = Some(&mut *self.project) {
            // Find indices to delete (in reverse order for safe removal)
            let mut indices: Vec<usize> = Vec::new();
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                if selected_ids.contains(&layer.layer_id) {
                    indices.push(i);
                }
            }

            // Don't delete all layers — keep at least one
            if indices.len() >= project.timeline.layers.len() {
                indices.pop();
            }

            if indices.is_empty() {
                return;
            }

            // Delete in reverse order (highest index first) for correct indexing
            indices.sort_unstable();
            indices.reverse();

            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for &idx in &indices {
                if idx < project.timeline.layers.len() {
                    let layer_clone = project.timeline.layers[idx].clone();
                    let cmd =
                        manifold_editing::commands::layer::DeleteLayerCommand::new(layer_clone);
                    commands.push(Box::new(cmd));
                }
            }

            if !commands.is_empty() {
                ContentCommand::send(
                    self.content_tx,
                    crate::content_command::ContentCommand::ExecuteBatch(commands, String::new()),
                );
            }
        }

        self.selection.clear_selection();
        ContentCommand::send(
            self.content_tx,
            crate::content_command::ContentCommand::MarkCompositorDirty,
        );
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn duplicate_selected_layers(&mut self) {
        if self.selection.layer_selection_count() == 0 {
            return;
        }
        let selected_ids: Vec<manifold_core::LayerId> =
            self.selection.selected_layer_ids.iter().cloned().collect();
        if let Some(cmd) = EditingService::duplicate_layers(self.project, &selected_ids) {
            let mut boxed: Box<dyn manifold_editing::command::Command> = cmd;
            boxed.execute(self.project);
            ContentCommand::send(
                self.content_tx,
                crate::content_command::ContentCommand::ExecuteBatch(
                    vec![boxed],
                    "Duplicate Layers".to_string(),
                ),
            );
        }
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    fn layer_count(&self) -> usize {
        Some(&*self.project)
            .map(|p| p.timeline.layers.len())
            .unwrap_or(0)
    }

    fn project_beats_per_bar(&self) -> u32 {
        Some(&*self.project)
            .map(|p| p.settings.time_signature_numerator.max(1) as u32)
            .unwrap_or(4)
    }

    fn set_export_in_at_playhead(&mut self) {
        let bpb = self.project.settings.time_signature_numerator.max(1) as u32;
        let snapped = self
            .ui_root
            .viewport
            .mapper()
            .snap_beat_to_grid(self.content_state.current_beat, bpb);
        self.project.timeline.export_in_beat = snapped;
        self.project.timeline.export_range_enabled = true;
        // Push to viewport immediately so build() sees it this frame
        self.ui_root.viewport.set_export_range(
            self.project.timeline.export_in_beat,
            self.project.timeline.export_out_beat,
            true,
        );
        *self.needs_rebuild = true;
        ContentCommand::send(
            self.content_tx,
            ContentCommand::MutateProject(Box::new(move |p| {
                p.timeline.export_in_beat = snapped;
                p.timeline.export_range_enabled = true;
            })),
        );
    }

    fn set_export_out_at_playhead(&mut self) {
        let bpb = self.project.settings.time_signature_numerator.max(1) as u32;
        let snapped = self
            .ui_root
            .viewport
            .mapper()
            .snap_beat_to_grid(self.content_state.current_beat, bpb);
        self.project.timeline.export_out_beat = snapped;
        self.project.timeline.export_range_enabled = true;
        self.ui_root.viewport.set_export_range(
            self.project.timeline.export_in_beat,
            self.project.timeline.export_out_beat,
            true,
        );
        *self.needs_rebuild = true;
        ContentCommand::send(
            self.content_tx,
            ContentCommand::MutateProject(Box::new(move |p| {
                p.timeline.export_out_beat = snapped;
                p.timeline.export_range_enabled = true;
            })),
        );
    }

    fn clear_export_in(&mut self) {
        let has_out = self.project.timeline.export_out_beat > self.project.timeline.export_in_beat;
        if !has_out {
            self.project.timeline.export_in_beat = manifold_core::Beats::ZERO;
            self.project.timeline.export_out_beat = manifold_core::Beats::ZERO;
            self.project.timeline.export_range_enabled = false;
            ContentCommand::send(
                self.content_tx,
                ContentCommand::MutateProject(Box::new(|p| {
                    p.timeline.export_in_beat = manifold_core::Beats::ZERO;
                    p.timeline.export_out_beat = manifold_core::Beats::ZERO;
                    p.timeline.export_range_enabled = false;
                })),
            );
        } else {
            self.project.timeline.export_in_beat = manifold_core::Beats::ZERO;
            ContentCommand::send(
                self.content_tx,
                ContentCommand::MutateProject(Box::new(|p| {
                    p.timeline.export_in_beat = manifold_core::Beats::ZERO;
                })),
            );
        }
        self.ui_root.viewport.set_export_range(
            self.project.timeline.export_in_beat,
            self.project.timeline.export_out_beat,
            self.project.timeline.export_range_enabled,
        );
        *self.needs_rebuild = true;
    }

    fn clear_export_out(&mut self) {
        if !self.project.timeline.export_range_enabled {
            return;
        }
        self.project.timeline.export_in_beat = manifold_core::Beats::ZERO;
        self.project.timeline.export_out_beat = manifold_core::Beats::ZERO;
        self.project.timeline.export_range_enabled = false;
        self.ui_root.viewport.set_export_range(
            manifold_core::Beats::ZERO,
            manifold_core::Beats::ZERO,
            false,
        );
        *self.needs_rebuild = true;
        ContentCommand::send(
            self.content_tx,
            ContentCommand::MutateProject(Box::new(|p| {
                p.timeline.export_in_beat = manifold_core::Beats::ZERO;
                p.timeline.export_out_beat = manifold_core::Beats::ZERO;
                p.timeline.export_range_enabled = false;
            })),
        );
    }

    fn start_export(&mut self) {
        // Defer to Application::start_export() which opens the file dialog.
        *self.pending_export = true;
    }

    fn dismiss_top_overlay(&mut self) -> bool {
        self.ui_root.escape_overlays()
    }

    fn grid_step(&self) -> f32 {
        self.ui_root.viewport.grid_step()
    }

    fn navigate_cursor(&mut self, direction: u8, is_fine: bool, grid_step: f32) {
        // Unity InputHandler.NavigateInsertCursor (lines 523-595)
        let dir = match direction {
            0 => cursor_nav::Direction::Left,
            1 => cursor_nav::Direction::Right,
            2 => cursor_nav::Direction::Up,
            3 => cursor_nav::Direction::Down,
            _ => return,
        };

        let mapper = self.ui_root.viewport.mapper();
        let layer_count = mapper.layer_count();
        let mut layers = Vec::with_capacity(layer_count);
        let mut clips = Vec::new();

        if let Some(project) = Some(&*self.project) {
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                layers.push(cursor_nav::NavLayerInfo {
                    index: i,
                    height: mapper.get_layer_height(i),
                });
                for clip in &layer.clips {
                    clips.push(cursor_nav::NavClipInfo {
                        clip_id: clip.id.clone(),
                        layer_index: i,
                        start_beat: clip.start_beat.as_f32(),
                        end_beat: (clip.start_beat + clip.duration_beats).as_f32(),
                    });
                }
            }
        }

        // Step 4f: read cursor position from UIState (not viewport scroll)
        let current_beat = self
            .selection
            .insert_cursor_beat
            .unwrap_or(self.content_state.current_beat)
            .as_f32();
        let active_idx = self
            .active_layer
            .as_ref()
            .and_then(|id| self.project.timeline.find_layer_index_by_id(id));
        let insert_cursor_idx = self
            .selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| self.project.timeline.find_layer_index_by_id(id));
        let current_layer = insert_cursor_idx.or(active_idx).unwrap_or(0);

        let result = cursor_nav::navigate_cursor(
            dir,
            current_beat,
            current_layer,
            grid_step,
            is_fine,
            &layers,
            &clips,
        );
        match result {
            cursor_nav::NavResult::SetCursor { beat, layer } => {
                let lid = self
                    .project
                    .timeline
                    .layers
                    .get(layer)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                self.selection
                    .set_insert_cursor(manifold_core::Beats::from_f32(beat), lid);
                *self.active_layer = self
                    .project
                    .timeline
                    .layers
                    .get(layer)
                    .map(|l| l.layer_id.clone());
            }
            cursor_nav::NavResult::SelectClip(clip_id) => {
                // Find the clip's layer for proper selection
                let li =
                    Some(&*self.project)
                        .and_then(|p| {
                            p.timeline.layers.iter().enumerate().find_map(|(i, l)| {
                                l.clips.iter().any(|c| c.id == clip_id).then_some(i)
                            })
                        })
                        .unwrap_or(0);
                let lid = self
                    .project
                    .timeline
                    .layers
                    .get(li)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                self.selection.select_clip(clip_id, lid);
                *self.active_layer = self
                    .project
                    .timeline
                    .layers
                    .get(li)
                    .map(|l| l.layer_id.clone());
            }
            cursor_nav::NavResult::NoChange => {}
        }

        *self.needs_rebuild = true;
        self.scroll_dirty.visual = true;
    }

    // ── UIState delegation ──────────────────────────────────────

    fn get_selected_clip_ids(&self) -> Vec<ClipId> {
        if let Some(r) = self.selection.current_region() {
            let region = crate::ui_translate::selection_region_to_core(r);
            EditingService::get_clips_in_region(self.project, &region)
                .into_iter()
                .map(|(_, id)| id)
                .collect()
        } else {
            self.selection.get_selected_clip_ids()
        }
    }

    fn selection_count(&self) -> usize {
        self.selection.selection_count()
    }

    fn layer_selection_count(&self) -> usize {
        self.selection.layer_selection_count()
    }

    fn has_region(&self) -> bool {
        self.selection.has_region()
    }

    fn insert_cursor_beat(&self) -> Option<f32> {
        self.selection.insert_cursor_beat.map(|beat| beat.as_f32())
    }

    fn insert_cursor_layer_index(&self) -> Option<usize> {
        self.selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| self.project.timeline.find_layer_index_by_id(id))
    }

    fn clear_selection(&mut self) {
        self.selection.clear_selection();
    }

    fn zoom_to_fit(&mut self) {
        // Unity InputHandler.ZoomToFit (lines 906-957):
        // Arbitrary ppb, center scroll, no-clips fallback.
        let viewport_width = self.ui_root.viewport.tracks_rect().width;
        if viewport_width <= 0.0 {
            return;
        }

        let project = match Some(&*self.project) {
            Some(p) => p,
            None => return,
        };

        let mut min_beat = f32::MAX;
        let mut max_beat = f32::MIN;
        let mut clip_count = 0;
        for layer in &project.timeline.layers {
            for clip in &layer.clips {
                let sb = clip.start_beat.as_f32();
                if sb < min_beat {
                    min_beat = sb;
                }
                let end = (clip.start_beat + clip.duration_beats).as_f32();
                if end > max_beat {
                    max_beat = end;
                }
                clip_count += 1;
            }
        }

        if clip_count == 0 {
            // No clips — reset to default zoom, scroll to start
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let default_idx = levels.len() / 2; // middle of zoom range
            self.ui_root.viewport.set_zoom(levels[default_idx]);
            self.ui_root.viewport.set_scroll(0.0, 0.0);
            self.scroll_dirty.zoom = true;
            return;
        }

        let extent_beats = max_beat - min_beat;
        // 10% padding on each side (min 1 beat)
        let padding = (extent_beats * 0.1).max(1.0);
        let fit_beats = extent_beats + padding * 2.0;

        // Calculate ideal ppb — arbitrary float, NOT nearest preset
        let max_ppb = *manifold_ui::color::ZOOM_LEVELS.last().unwrap_or(&200.0);
        let ideal_ppb = (viewport_width / fit_beats).clamp(1.0, max_ppb);

        self.ui_root.viewport.set_zoom(ideal_ppb);

        // Center-scroll on clip extent
        let center_beat = min_beat + extent_beats * 0.5;
        let center_pixel = center_beat * ideal_ppb;
        let scroll_beat = ((center_pixel - viewport_width * 0.5) / ideal_ppb).max(0.0);
        self.ui_root.viewport.set_scroll(scroll_beat, 0.0);

        self.scroll_dirty.zoom = true;
    }

    /// B14 `Z` — frame the current selection (`Clips` or `TimeRange`) with
    /// margin. Same fit math as `zoom_to_fit`, bounds narrowed to the
    /// selection. No-op with nothing selected.
    fn zoom_to_selection(&mut self) {
        let viewport_width = self.ui_root.viewport.tracks_rect().width;
        if viewport_width <= 0.0 {
            return;
        }

        let bounds: Option<(f32, f32)> = if let Some(region) = self.selection.current_region() {
            Some((region.start_beat.as_f32(), region.end_beat.as_f32()))
        } else {
            let ids = self.selection.get_selected_clip_ids();
            if ids.is_empty() {
                None
            } else {
                let mut min_beat = f32::MAX;
                let mut max_beat = f32::MIN;
                for layer in &self.project.timeline.layers {
                    for clip in &layer.clips {
                        if !ids.contains(&clip.id) {
                            continue;
                        }
                        let sb = clip.start_beat.as_f32();
                        if sb < min_beat {
                            min_beat = sb;
                        }
                        let end = (clip.start_beat + clip.duration_beats).as_f32();
                        if end > max_beat {
                            max_beat = end;
                        }
                    }
                }
                if max_beat > min_beat {
                    Some((min_beat, max_beat))
                } else {
                    None
                }
            }
        };

        let Some((min_beat, max_beat)) = bounds else {
            return;
        };

        // Capture the pre-zoom view — B14 `Shift+Z` restores it.
        self.ui_root.viewport.store_zoom_back();

        let max_ppb = *manifold_ui::color::ZOOM_LEVELS.last().unwrap_or(&200.0);
        let (ideal_ppb, scroll_beat) =
            selection_fit_zoom(min_beat, max_beat, viewport_width, max_ppb);
        self.ui_root.viewport.set_zoom(ideal_ppb);
        self.ui_root.viewport.set_scroll(scroll_beat, 0.0);

        self.scroll_dirty.zoom = true;
    }

    /// B14 `Shift+Z` — restore the view captured by the last
    /// `zoom_to_selection`. No-op if nothing was captured.
    fn zoom_back(&mut self) {
        if let Some((ppb, scroll_x, scroll_y)) = self.ui_root.viewport.recall_zoom_back() {
            self.ui_root.viewport.set_zoom(ppb);
            self.ui_root.viewport.set_scroll(scroll_x.as_f32(), scroll_y);
            self.scroll_dirty.zoom = true;
        }
    }

    // ── Timeline markers ─────────────────────────────────────────

    fn add_marker_at_playhead(&mut self) {
        self.add_marker_at_playhead_impl();
    }

    fn delete_selected_markers(&mut self) {
        use manifold_editing::commands::marker::DeleteMarkerCommand;

        let ids: Vec<manifold_core::MarkerId> =
            self.selection.selected_marker_ids.iter().cloned().collect();
        if ids.is_empty() {
            return;
        }

        let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
        for id in &ids {
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(DeleteMarkerCommand::new(id.clone()));
            boxed.execute(self.project);
            commands.push(boxed);
        }
        ContentCommand::send(
            self.content_tx,
            ContentCommand::ExecuteBatch(commands, "Delete Markers".into()),
        );

        self.selection.selected_marker_ids.clear();
        *self.needs_rebuild = true;
    }

    fn has_selected_markers(&self) -> bool {
        !self.selection.selected_marker_ids.is_empty()
    }

    // ── Automation lane editing ──────────────────────────────────

    fn has_selected_automation_point(&self) -> bool {
        self.selection.selected_automation_point.is_some()
    }

    fn delete_selected_automation_point(&mut self) {
        use manifold_editing::commands::automation::RemoveAutomationPointCommand;

        let Some(point_ref) = self.selection.selected_automation_point.clone() else {
            return;
        };
        let target = crate::editing_host::to_graph_target(&point_ref.target);
        let param_id_str = point_ref.param_id.as_ref();
        let index = self.project.preset_instance(&target).and_then(|inst| {
            inst.automation_lanes.as_ref().and_then(|lanes| {
                lanes
                    .iter()
                    .find(|l| l.param_id.as_ref() == param_id_str)
                    .and_then(|lane| {
                        let param = inst.params.get(param_id_str)?;
                        lane.points.iter().position(|p| {
                            let range = (param.spec.max - param.spec.min).abs().max(f32::EPSILON);
                            let norm = ((p.value - param.spec.min) / range).clamp(0.0, 1.0);
                            p.beat == point_ref.beat && norm == point_ref.value_norm
                        })
                    })
            })
        });
        self.selection.selected_automation_point = None;
        let Some(index) = index else {
            return;
        };
        let mut cmd = RemoveAutomationPointCommand::new(target, param_id_str, index);
        cmd.execute(self.project);
        ContentCommand::send(
            self.content_tx,
            ContentCommand::Execute(Box::new(cmd)),
        );
        *self.needs_rebuild = true;
    }

    // ── Automation lane editing — marquee + draw mode (P4 Unit B) ─────

    fn has_selected_automation_points(&self) -> bool {
        !self.selection.selected_automation_points.is_empty()
            || self.selection.automation_time_selection.is_some()
    }

    fn has_automation_selection(&self) -> bool {
        self.selection.selected_automation_point.is_some()
            || !self.selection.selected_automation_points.is_empty()
            || self.selection.automation_time_selection.is_some()
    }

    fn select_all_automation(&mut self) -> bool {
        if !self.selection.automation_mode_visible { return false; }
        let Some((target, param_id)) = self.selection.automation_paste_context.clone() else { return false; };
        automation::select_all_in_lane(self.project, self.selection, &target, &param_id);
        *self.needs_rebuild = true;
        true
    }

    fn copy_selected_automation(&mut self) {
        automation::copy_selected(self.project, self.selection);
    }

    fn cut_selected_automation(&mut self) {
        automation::cut_selected(
            self.project,
            self.selection,
            self.content_tx,
            self.needs_rebuild,
        );
    }

    fn has_automation_paste_target(&self) -> bool {
        self.selection.automation_clipboard.is_some()
            && (self.selection.selected_automation_point.is_some()
                || !self.selection.selected_automation_points.is_empty()
                || self.selection.automation_time_selection.is_some()
                || self.selection.automation_paste_context.is_some())
    }

    fn paste_automation(&mut self, target_beat: f32) {
        let target_beat = self.selection.automation_time_selection.as_ref()
            .map(|range| range.start)
            .or(self.selection.automation_insert_beat)
            .unwrap_or_else(|| Beats::from_f32(target_beat));
        automation::paste(
            self.project,
            self.selection,
            self.content_tx,
            target_beat,
            self.needs_rebuild,
        );
    }

    fn duplicate_selected_automation(&mut self) {
        automation::duplicate_selected(
            self.project,
            self.selection,
            self.content_tx,
            self.ui_root.viewport.grid_step(),
            self.needs_rebuild,
        );
    }

    fn delete_selected_automation_points(&mut self) {
        automation::delete_selected(
            self.project,
            self.selection,
            self.content_tx,
            self.needs_rebuild,
        );
    }

    fn toggle_automation_draw_mode(&mut self) {
        self.selection.automation_draw_mode = !self.selection.automation_draw_mode;
        self.selection.automation_mode_visible = true;
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
    }

    fn automation_mode_visible(&self) -> bool {
        self.selection.automation_mode_visible
    }

    fn toggle_automation_mode_visible(&mut self) {
        // Mirrors `PanelAction::ToggleAutomationMode` (ui_bridge/transport.rs)
        // exactly — same view-state flip, same `DispatchResult::structural()`
        // semantics: it changes the Y-layout (lane strips appear/disappear),
        // so both dirty flags are needed, not just `needs_rebuild`.
        self.selection.automation_mode_visible = !self.selection.automation_mode_visible;
        self.selection.clear_automation_selection();
        *self.needs_rebuild = true;
        *self.needs_structural_sync = true;
    }

    // ── D4/D5 Finder-paste arbitration (docs/TIMELINE_INGEST_DESIGN.md section 2) ──

    fn pasteboard_file_urls(&self) -> Vec<std::path::PathBuf> {
        #[cfg(target_os = "macos")]
        {
            crate::macos_pasteboard::file_urls_on_general_pasteboard()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn pasteboard_change_count(&self) -> i64 {
        #[cfg(target_os = "macos")]
        {
            crate::macos_pasteboard::general_change_count()
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    fn internal_clipboard_snapshot(&self) -> Option<i64> {
        #[cfg(target_os = "macos")]
        {
            *self.internal_clipboard_change_count
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn paste_pasteboard_files(&mut self, file_paths: &[std::path::PathBuf], target_beat: f32) {
        // D5: same target resolution as a Finder drag-drop onto the active
        // lane (app.rs's DroppedFile arm) — the active layer if it exists,
        // joined only when it's audio; never auto-creates a typed lane
        // beyond what `process_dropped_files` already does for drops.
        let drop_layer_index = self
            .active_layer
            .as_ref()
            .and_then(|id| self.project.timeline.find_layer_index_by_id(id))
            .unwrap_or(0) as i32;
        let join_audio_layer = self.active_layer.as_ref().and_then(|id| {
            self.project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == *id && l.is_audio())
                .map(|l| l.layer_id.clone())
        });
        let spb = 60.0 / self.project.settings.bpm.0.max(1.0);
        let action = self.project_io.process_dropped_files(
            file_paths,
            target_beat,
            drop_layer_index,
            join_audio_layer,
            self.project,
            spb,
        );
        if action.needs_clip_sync {
            *self.needs_rebuild = true;
        }
        if !action.record_commands.is_empty() {
            if action.record_commands.len() == 1 {
                let cmd = action.record_commands.into_iter().next().unwrap();
                ContentCommand::send(self.content_tx, ContentCommand::Execute(cmd));
            } else {
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::ExecuteBatch(action.record_commands, "Paste files".into()),
                );
            }
        }
    }
}

impl AppInputHost<'_> {
    /// D4: record the general pasteboard's current changeCount as the
    /// baseline for "how recent is the internal clipboard". Called after
    /// every internal copy/cut — never after paste, which doesn't change
    /// what the internal clipboard holds.
    fn snapshot_pasteboard_change_count(&mut self) {
        #[cfg(target_os = "macos")]
        {
            *self.internal_clipboard_change_count =
                Some(crate::macos_pasteboard::general_change_count());
        }
    }

    /// Snap the playhead to the grid, mint a marker, and execute an
    /// `AddMarkerCommand`.
    fn add_marker_at_playhead_impl(&mut self) {
        use manifold_core::marker::TimelineMarker;
        use manifold_editing::commands::marker::AddMarkerCommand;

        let bpb = self.project.settings.time_signature_numerator.max(1) as u32;
        let snapped = self
            .ui_root
            .viewport
            .mapper()
            .snap_beat_to_grid(self.content_state.current_beat, bpb);
        let marker = TimelineMarker::new(snapped);

        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
            Box::new(AddMarkerCommand::new(marker));
        boxed.execute(self.project);
        ContentCommand::send(self.content_tx, ContentCommand::Execute(boxed));
        *self.needs_rebuild = true;
    }
}

#[allow(dead_code, unreachable_patterns)]
fn from_core_segment_shape(shape: SegmentShape) -> UiSegmentShape {
    match shape {
        SegmentShape::Linear => UiSegmentShape::Linear,
        SegmentShape::Hold => UiSegmentShape::Hold,
        SegmentShape::Curved(bend) => UiSegmentShape::Curved(bend),
        SegmentShape::CurvedRange { bend, start, end } => {
            UiSegmentShape::CurvedRange { bend, start, end }
        }
        _ => UiSegmentShape::Linear,
    }
}

#[allow(dead_code, unreachable_patterns)]
fn to_core_segment_shape(shape: UiSegmentShape) -> SegmentShape {
    match shape {
        UiSegmentShape::Linear => SegmentShape::Linear,
        UiSegmentShape::Hold => SegmentShape::Hold,
        UiSegmentShape::Curved(bend) => SegmentShape::Curved(bend),
        UiSegmentShape::CurvedRange { bend, start, end } => {
            SegmentShape::CurvedRange { bend, start, end }
        }
        _ => SegmentShape::Linear,
    }
}

// ── Effect resolution helpers (mirrors ui_bridge resolve_effects) ──

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;

fn resolve_effects_ref<'a>(
    tab: InspectorTab,
    project: &'a Project,
    active_layer: &Option<LayerId>,
    selection: &UIState,
) -> Option<&'a [PresetInstance]> {
    match tab {
        InspectorTab::Master => Some(&project.settings.master_effects),
        InspectorTab::Layer | InspectorTab::Group => active_layer
            .as_ref()
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .and_then(|idx| project.timeline.layers.get(idx))
            .and_then(|l| l.effects.as_deref()),
        InspectorTab::Clip => selection.primary_selected_clip_id.as_ref().and_then(|cid| {
            project
                .timeline
                .layers
                .iter()
                .flat_map(|l| l.clips.iter())
                .find(|c| c.id == *cid)
                .map(|c| c.effects.as_slice())
        }),
    }
}

fn resolve_effect_target(
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    _selection: &UIState,
) -> EffectTarget {
    match tab {
        InspectorTab::Master => EffectTarget::Master,
        InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => {
            let layer_id = active_layer.clone().unwrap_or_default();
            EffectTarget::Layer { layer_id }
        }
    }
}

/// B14 `Z` fit math — pure function so it's testable without an `AppInputHost`
/// (which borrows `Application` fields it can't stand up in a unit test).
/// Same shape as `zoom_to_fit`'s inline math (10% padding each side, min 1
/// beat; center-scroll on the extent), narrowed to `[min_beat, max_beat)`.
/// Returns `(pixels_per_beat, scroll_x_beats)`.
fn selection_fit_zoom(min_beat: f32, max_beat: f32, viewport_width: f32, max_ppb: f32) -> (f32, f32) {
    let extent_beats = (max_beat - min_beat).max(0.0);
    let padding = (extent_beats * 0.1).max(1.0);
    let fit_beats = extent_beats + padding * 2.0;

    let ideal_ppb = (viewport_width / fit_beats).clamp(1.0, max_ppb);

    let center_beat = min_beat + extent_beats * 0.5;
    let center_pixel = center_beat * ideal_ppb;
    let scroll_beat = ((center_pixel - viewport_width * 0.5) / ideal_ppb).max(0.0);
    (ideal_ppb, scroll_beat)
}

#[cfg(test)]
mod zoom_to_selection_tests {
    use super::selection_fit_zoom;

    /// B14 gate: zoom-to-selection frames the selection with margin — assert
    /// the resulting visible beat range `[scroll, scroll + width/ppb)`
    /// contains `[min_beat, max_beat)` plus positive margin on both sides.
    #[test]
    fn frames_selection_with_margin() {
        let (min_beat, max_beat) = (10.0f32, 18.0f32);
        let viewport_width = 800.0f32;
        let max_ppb = 200.0f32;

        let (ppb, scroll_beat) = selection_fit_zoom(min_beat, max_beat, viewport_width, max_ppb);
        assert!(ppb > 0.0);

        let visible_start = scroll_beat;
        let visible_end = scroll_beat + viewport_width / ppb;

        assert!(
            visible_start < min_beat,
            "visible range must start before the selection (margin): {visible_start} vs {min_beat}"
        );
        assert!(
            visible_end > max_beat,
            "visible range must end after the selection (margin): {visible_end} vs {max_beat}"
        );
    }

    /// A single-instant selection (min == max, e.g. a zero-width edge case)
    /// still produces a sane positive-extent fit via the 1-beat padding floor.
    #[test]
    fn degenerate_zero_width_selection_still_fits() {
        let (ppb, scroll_beat) = selection_fit_zoom(4.0, 4.0, 800.0, 200.0);
        assert!(ppb > 0.0);
        assert!(scroll_beat >= 0.0);
    }

    /// Very large selections clamp to the minimum zoom (`max_ppb` floor isn't
    /// exceeded on the low end either — `ideal_ppb` never goes below the
    /// clamp's lower bound of 1.0).
    #[test]
    fn wide_selection_clamps_ppb_within_bounds() {
        let (ppb, _) = selection_fit_zoom(0.0, 100_000.0, 800.0, 200.0);
        assert!((1.0..=200.0).contains(&ppb));
    }
}

#[cfg(test)]
mod automation_clipboard_host_tests {
    use super::*;
    use crossbeam_channel::Receiver;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::{AutomationLane, AutomationPoint, PresetInstance, SegmentShape};
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::{EffectId, GraphTarget, LayerId, PresetTypeId};
    use manifold_core::layer::Layer;
    use manifold_editing::service::EditingService;
    use manifold_ui::ui_state::AutomationTimeSelection;
    use manifold_ui::view::{UiAutomationPointRef, UiGraphTarget, UiSegmentShape};

    struct Harness {
        project: manifold_core::project::Project,
        tx: crossbeam_channel::Sender<ContentCommand>,
        rx: Receiver<ContentCommand>,
        content_state: crate::content_state::ContentState,
        ui_root: UIRoot,
        selection: UIState,
        active_layer: Option<LayerId>,
        needs_rebuild: bool,
        needs_structural_sync: bool,
        scroll_dirty: crate::ui_root::ScrollDirty,
        current_project_path: Option<std::path::PathBuf>,
        pending_close_output: bool,
        pending_export: bool,
        project_io: crate::project_io::ProjectIOService,
        #[cfg(target_os = "macos")]
        internal_clipboard_change_count: Option<i64>,
    }

    impl Harness {
        fn new() -> Self {
            let (tx, rx) = crossbeam_channel::unbounded();
            let mut project = manifold_core::project::Project::default();
            let mut effect = PresetInstance::new(PresetTypeId::new("ClipboardTest"));
            let p1 = ParamSpecDef {
                id: "amount".to_string(), name: "Amount".to_string(), min: 0.0,
                max: 1.0, default_value: 0.5, ..Default::default()
            };
            let p2 = ParamSpecDef {
                id: "steps".to_string(), name: "Steps".to_string(), min: 0.0,
                max: 10.0, default_value: 5.0, whole_numbers: true, ..Default::default()
            };
            effect.params = ParamManifest::from_params(vec![Param::bundled(p1), Param::bundled(p2)]);
            effect.automation_lanes = Some(vec![
                AutomationLane {
                    param_id: manifold_core::effects::ParamId::from("amount"), enabled: true,
                    points: vec![
                        AutomationPoint { beat: Beats(2.0), value: 0.2, shape: SegmentShape::Linear },
                        AutomationPoint { beat: Beats(6.0), value: 0.6, shape: SegmentShape::CurvedRange { bend: 0.4, start: 0.1, end: 0.9 } },
                        AutomationPoint { beat: Beats(10.0), value: 0.8, shape: SegmentShape::Hold },
                    ],
                },
                AutomationLane {
                    param_id: manifold_core::effects::ParamId::from("steps"), enabled: true,
                    points: vec![
                        AutomationPoint { beat: Beats(4.0), value: 2.0, shape: SegmentShape::Hold },
                        AutomationPoint { beat: Beats(8.0), value: 8.0, shape: SegmentShape::Linear },
                    ],
                },
            ]);
            project.settings.master_effects.push(effect);
            Self {
                project, tx, rx, content_state: Default::default(), ui_root: UIRoot::new(),
                selection: UIState::new(), active_layer: None, needs_rebuild: false,
                needs_structural_sync: false, scroll_dirty: Default::default(),
                current_project_path: None, pending_close_output: false, pending_export: false,
                project_io: crate::project_io::ProjectIOService::new(&crate::user_prefs::UserPrefs::in_memory()),
                #[cfg(target_os = "macos")]
                internal_clipboard_change_count: None,
            }
        }

        fn effect_id(&self) -> EffectId { self.project.settings.master_effects[0].id.clone() }

        fn host(&mut self) -> AppInputHost<'_> {
            AppInputHost {
                project: &mut self.project, content_tx: &self.tx,
                content_state: &self.content_state, ui_root: &mut self.ui_root,
                selection: &mut self.selection, active_layer: &mut self.active_layer,
                needs_rebuild: &mut self.needs_rebuild,
                needs_structural_sync: &mut self.needs_structural_sync,
                scroll_dirty: &mut self.scroll_dirty,
                current_project_path: &self.current_project_path, has_output_window: false,
                pending_close_output: &mut self.pending_close_output,
                pending_export: &mut self.pending_export,
                project_io: &mut self.project_io,
                #[cfg(target_os = "macos")]
                internal_clipboard_change_count: &mut self.internal_clipboard_change_count,
            }
        }

        fn select(&mut self, points: &[(&str, f64)]) {
            let target = UiGraphTarget::Effect(self.effect_id());
            self.selection.selected_automation_points = points.iter().map(|(param, beat)| {
                let effect = &self.project.settings.master_effects[0];
                let param_spec = effect.params.get(param).expect("clipboard fixture parameter").spec.clone();
                let lane = effect.automation_lanes.as_ref().expect("clipboard fixture lanes")
                    .iter().find(|lane| lane.param_id.as_ref() == *param)
                    .expect("clipboard fixture lane");
                let point = lane.points.iter().find(|point| point.beat == Beats(*beat))
                    .expect("clipboard fixture point");
                UiAutomationPointRef {
                    target: target.clone(),
                    param_id: manifold_core::effects::ParamId::from((*param).to_string()),
                    beat: Beats(*beat),
                    value_norm: manifold_ui::slider::BitmapSlider::value_to_normalized(
                        point.value, param_spec.min, param_spec.max,
                    ),
                }
            }).collect();
            self.selection.selected_automation_point = None;
        }
    }

    fn drain_batch(h: &Harness, authoritative: &mut manifold_core::project::Project, service: &mut EditingService) {
        match h.rx.try_recv().expect("host must emit one command") {
            ContentCommand::ExecuteBatch(commands, description) => service.execute_batch(commands, description, authoritative),
            _ => panic!("expected an automation ExecuteBatch"),
        }
    }

    fn points(project: &manifold_core::project::Project, param: &str) -> Vec<(f64, f32)> {
        project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|l| l.param_id.as_ref() == param).unwrap().points.iter()
            .map(|p| (p.beat.0, p.value)).collect()
    }

    #[test]
    fn cmd_g_wraps_one_or_multiple_effects_in_undoable_modifier_group() {
        for count in [1, 2] {
            let mut h = Harness::new();
            let mut layer = Layer::new("Effects".into(), manifold_core::types::LayerType::Video, 0);
            let layer_id = layer.layer_id.clone();
            let effects = (0..count).map(|_| PresetInstance::new(PresetTypeId::new("Mirror"))).collect::<Vec<_>>();
            let surfaces = effects.iter().enumerate().map(|(index, effect)| {
                manifold_ui::param_surface::ParamSurface {
                    kind: manifold_ui::panels::param_card::ParamCardKind::Effect,
                    title: "Mirror".into(), collapsed: false, enabled: true,
                    effect_index: index, effect_id: effect.id.clone(), supports_envelopes: true,
                    has_graph_mod: false, layer_id: Some(layer_id.clone()), modifier: None,
                    rows: Vec::new(), string_params: Vec::new(), audio_sends: Vec::new(),
                    relight: Default::default(),
                }
            }).collect::<Vec<_>>();
            layer.effects = Some(effects);
            h.project.timeline.layers.push(layer);
            h.active_layer = Some(layer_id.clone());
            h.ui_root.inspector.configure_layer_effects(&surfaces, Some(&layer_id));
            assert!(h.ui_root.inspector.select_all_effects());
            let mut input = crate::input_handler::InputHandler::new();
            input.inspector_has_focus = true;
            assert!(input.handle_keyboard_input(
                &winit::keyboard::Key::Character("g".into()),
                manifold_ui::input::Modifiers { command: true, ..Default::default() },
                &mut h.host(),
            ));
            assert!(h.project.timeline.layers[0].effect_groups.is_none(), "UI must not mutate the model");
            let ContentCommand::ExecuteOnContent(mut command) = h.rx.try_recv().expect("group queued") else {
                panic!("group must execute on content thread");
            };
            command.execute(&mut h.project);
            let layer = &h.project.timeline.layers[0];
            let groups = layer.effect_groups.as_ref().unwrap();
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].name, "Modifier Group");
            assert!(groups[0].mask_effect_id.is_none());
            assert!(layer.effects.as_ref().unwrap().iter().all(|effect| effect.group_id.as_ref() == Some(&groups[0].id)));
            command.undo(&mut h.project);
            assert!(h.project.timeline.layers[0].effects.as_ref().unwrap().iter().all(|effect| effect.group_id.is_none()));
        }
    }

    #[test]
    fn modifier_paste_dispatches_from_clipboard_into_empty_modifier_stack() {
        let mut h = Harness::new();
        let mut layer = Layer::new_generator("WaveGrid".into(), PresetTypeId::new("WaveGrid"), 0);
        let layer_id = LayerId::new("modifier-shortcut-layer");
        layer.layer_id = layer_id.clone();
        let graph = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("WaveGrid"))
            .expect("WaveGrid fixture")
            .clone();
        layer.gen_params_or_init().graph = Some(graph);
        layer.gen_params_or_init().refresh_manifest_from_graph();
        h.project.timeline.layers.push(layer);
        let mut add = crate::scene_modifier_edit::build_action(
            &h.project,
            crate::scene_modifier_edit::SceneModifierAction::Add(
                layer_id.clone(),
                "SceneFog".into(),
            ),
        )
        .expect("scene modifier add");
        add.execute(&mut h.project);
        let mut destination = Layer::new_generator(
            "WaveGrid Destination".into(),
            PresetTypeId::new("WaveGrid"),
            0,
        );
        let destination_id = LayerId::new("modifier-shortcut-destination");
        destination.layer_id = destination_id.clone();
        let destination_graph = manifold_renderer::node_graph::bundled_preset_def(
            &PresetTypeId::new("WaveGrid"),
        )
        .expect("WaveGrid destination fixture")
        .clone();
        destination.gen_params_or_init().graph = Some(destination_graph);
        destination.gen_params_or_init().refresh_manifest_from_graph();
        h.project.timeline.layers.push(destination);
        let source_graph = h
            .project
            .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
            .and_then(|owner| owner.graph.as_ref())
            .expect("source graph");
        let modifier_id = source_graph.scene_modifiers[0].id.clone();
        h.ui_root.set_scene_modifier_clipboard(Some(
            crate::scene_modifier_transfer::ModifierClipboard::capture(
                &h.project,
                &layer_id,
                std::slice::from_ref(&modifier_id),
            )
            .expect("modifier clipboard capture"),
        ));
        h.ui_root
            .inspector
            .configure_modifier_cards(&[], Some(&destination_id), true, Vec::new());
        let before = serde_json::to_vec(&h.project).expect("project serializes");
        {
            let mut host = h.host();
            assert!(host.handle_effect_paste());
        }
        let command = h.rx.try_recv().expect("paste must reach content thread");
        match command {
            ContentCommand::SceneModifier(
                crate::scene_modifier_edit::SceneModifierAction::Paste(destination, clipboard),
            ) => {
                assert_eq!(destination, destination_id);
                assert_eq!(clipboard.count(), 1);
            }
            other => panic!("expected scene modifier paste, got {:?}", std::mem::discriminant(&other)),
        }
        assert_eq!(serde_json::to_vec(&h.project).expect("project serializes"), before);

        // Copying an ordinary effect afterwards must supersede this modifier
        // clipboard, including when the destination has no selected cards.
        let effect = h.project.settings.master_effects[0].clone();
        let surface = manifold_ui::param_surface::ParamSurface {
            kind: manifold_ui::panels::param_card::ParamCardKind::Effect,
            title: "ClipboardTest".into(), collapsed: false, enabled: true,
            effect_index: 0, effect_id: effect.id.clone(), supports_envelopes: true,
            has_graph_mod: false, layer_id: None, modifier: None,
            rows: Vec::new(), string_params: Vec::new(), audio_sends: Vec::new(),
            relight: Default::default(),
        };
        h.project.timeline.find_layer_by_id_mut(&destination_id).unwrap().1.effects = Some(vec![effect]);
        h.active_layer = Some(destination_id.clone());
        h.ui_root.inspector.configure_layer_effects(&[surface], Some(&destination_id));
        assert!(h.ui_root.inspector.select_all_effects());
        assert!(h.host().handle_effect_copy());
        assert!(h.ui_root.scene_modifier_clipboard.is_none());
        h.ui_root.inspector.clear_effect_selection(&mut h.ui_root.tree);
        assert!(h.host().handle_effect_paste());
        assert!(matches!(h.rx.try_recv().unwrap(), ContentCommand::Execute(_)));
        assert_eq!(h.project.timeline.find_layer_by_id(&destination_id).unwrap().1.effects.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn modifier_copy_supersedes_effect_clipboard() {
        // Latest copy wins across both clipboards: a modifier copy made after
        // an effect copy must clear the effect clipboard, or a later paste
        // resurrects the stale effect.
        let mut h = Harness::new();
        let effect = h.project.settings.master_effects[0].clone();
        let surface = manifold_ui::param_surface::ParamSurface {
            kind: manifold_ui::panels::param_card::ParamCardKind::Effect,
            title: "ClipboardTest".into(), collapsed: false, enabled: true,
            effect_index: 0, effect_id: effect.id.clone(), supports_envelopes: true,
            has_graph_mod: false, layer_id: None, modifier: None,
            rows: Vec::new(), string_params: Vec::new(), audio_sends: Vec::new(),
            relight: Default::default(),
        };
        let mut layer = Layer::new("Effects".into(), manifold_core::types::LayerType::Video, 0);
        let layer_id = layer.layer_id.clone();
        layer.effects = Some(vec![effect]);
        h.project.timeline.layers.push(layer);
        h.active_layer = Some(layer_id.clone());
        h.ui_root.inspector.configure_layer_effects(&[surface], Some(&layer_id));
        assert!(h.ui_root.inspector.select_all_effects());
        assert!(h.host().handle_effect_copy());
        assert!(h.ui_root.effect_clipboard.has_content());

        // Scene-modifier scope on a generator layer, one SceneFog applied.
        let mut gen_layer = Layer::new_generator(
            "WaveGrid".into(),
            PresetTypeId::new("WaveGrid"),
            0,
        );
        let gen_layer_id = LayerId::new("supersede-gen");
        gen_layer.layer_id = gen_layer_id.clone();
        let graph = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("WaveGrid"))
            .expect("WaveGrid fixture")
            .clone();
        gen_layer.gen_params_or_init().graph = Some(graph);
        gen_layer.gen_params_or_init().refresh_manifest_from_graph();
        h.project.timeline.layers.push(gen_layer);
        let mut add = crate::scene_modifier_edit::build_action(
            &h.project,
            crate::scene_modifier_edit::SceneModifierAction::Add(gen_layer_id.clone(), "SceneFog".into()),
        )
        .expect("scene modifier add");
        add.execute(&mut h.project);
        let modifier_id = h
            .project
            .graph_target_owner(&GraphTarget::Generator(gen_layer_id.clone()))
            .and_then(|owner| owner.graph.as_ref())
            .expect("source graph")
            .scene_modifiers[0]
            .id
            .clone();
        let mod_surface = manifold_ui::param_surface::ParamSurface {
            kind: manifold_ui::panels::param_card::ParamCardKind::Effect,
            title: "SceneFog".into(),
            rows: Vec::new(),
            string_params: Vec::new(),
            audio_sends: Vec::new(),
            modifier: Some(manifold_ui::param_surface::ModifierCardInfo {
                instance_id: modifier_id.clone(),
                layer_id: gen_layer_id.clone(),
                enabled_label: "Enabled".into(),
                stack_index: 0,
                stack_len: 1,
                targets_all: true,
                objects: Vec::new(),
            }),
            effect_index: 0,
            effect_id: manifold_core::EffectId::new(format!("scene_modifier:{}", modifier_id)),
            enabled: true,
            collapsed: false,
            supports_envelopes: true,
            has_graph_mod: false,
            layer_id: None,
            relight: Default::default(),
        };
        h.ui_root
            .inspector
            .configure_modifier_cards(&[mod_surface], Some(&gen_layer_id), true, Vec::new());
        h.ui_root.inspector.clear_effect_selection(&mut h.ui_root.tree);
        assert!(h.ui_root.inspector.select_all_modifiers());
        assert!(h.host().handle_effect_copy());
        assert!(h.ui_root.scene_modifier_clipboard.is_some());
        assert!(
            !h.ui_root.effect_clipboard.has_content(),
            "modifier copy must clear the effect clipboard"
        );
    }

    #[test]
    fn paste_multilane_preserves_relative_timing_and_undoes_collision_exactly() {
        let mut h = Harness::new();
        h.select(&[("amount", 2.0), ("amount", 6.0), ("steps", 4.0), ("steps", 8.0)]);
        let before = h.project.clone();
        { let mut host = h.host(); host.copy_selected_automation(); host.paste_automation(10.0); }
        let amount = h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|lane| lane.param_id.as_ref() == "amount").unwrap();
        let steps = h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|lane| lane.param_id.as_ref() == "steps").unwrap();
        assert_eq!(amount.value_at(Beats(10.0)), 0.2);
        assert_eq!(amount.value_at(Beats(14.0)), 0.6);
        assert_eq!(steps.value_at(Beats(12.0)), 2.0);
        assert_eq!(steps.value_at(Beats(16.0)), 8.0);
        let mut authoritative = before.clone();
        let mut service = EditingService::new();
        drain_batch(&h, &mut authoritative, &mut service);
        assert_eq!(points(&authoritative, "amount"), points(&h.project, "amount"));
        assert!(service.undo(&mut authoritative));
        assert_eq!(serde_json::to_value(&authoritative.settings.master_effects[0].automation_lanes).unwrap(), serde_json::to_value(&before.settings.master_effects[0].automation_lanes).unwrap());
        assert!(service.redo(&mut authoritative));
        assert_eq!(serde_json::to_value(&authoritative.settings.master_effects[0].automation_lanes).unwrap(), serde_json::to_value(&h.project.settings.master_effects[0].automation_lanes).unwrap());
    }

    #[test]
    fn single_lane_paste_remaps_range_and_rounds_integer_destination() {
        let mut h = Harness::new();
        let target = UiGraphTarget::Effect(h.effect_id());
        h.select(&[("amount", 2.0), ("amount", 6.0)]);
        { let mut host = h.host(); host.copy_selected_automation(); }
        h.selection.selected_automation_point = Some(UiAutomationPointRef {
            target,
            param_id: "steps".into(),
            beat: Beats(4.0),
            value_norm: manifold_ui::slider::BitmapSlider::value_to_normalized(2.0, 0.0, 10.0),
        });
        h.selection.selected_automation_points.clear();
        { let mut host = h.host(); host.paste_automation(20.0); }
        assert!(points(&h.project, "steps").contains(&(20.0, 2.0)));
        assert_eq!(h.selection.selected_automation_points.len(), 2);
        let lane = h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|lane| lane.param_id.as_ref() == "steps").unwrap();
        assert_eq!(lane.value_at(Beats(23.5)), 2.0, "integer destinations hold between pasted points");
        assert_eq!(lane.value_at(Beats(24.0)), 6.0);
    }

    #[test]
    fn time_range_copy_carries_empty_interior_and_curved_boundaries() {
        let mut h = Harness::new();
        h.project.settings.master_effects[0]
            .automation_lanes.as_mut().unwrap()[0].points[0].shape = SegmentShape::Curved(0.6);
        let target = UiGraphTarget::Effect(h.effect_id());
        h.selection.automation_time_selection = Some(AutomationTimeSelection {
            start: Beats(3.0),
            end: Beats(5.0),
            lanes: vec![(target.clone(), "amount".into())],
        });
        { let mut host = h.host(); host.copy_selected_automation(); }
        let clipboard = h.selection.automation_clipboard.as_ref().unwrap();
        assert_eq!(clipboard.span, Beats(2.0));
        assert_eq!(clipboard.points.len(), 2, "no authored point lies inside the range");
        assert_eq!(clipboard.points[0].beat_offset, Beats::ZERO);
        assert_eq!(clipboard.points[1].beat_offset, Beats(2.0));
        assert_eq!(
            clipboard.points[0].shape,
            UiSegmentShape::CurvedRange { bend: 0.6, start: 0.25, end: 0.75 }
        );
        let copied_values = (clipboard.points[0].value, clipboard.points[1].value);
        h.selection.clear_automation_selection();
        h.selection.automation_time_selection = Some(AutomationTimeSelection {
            start: Beats(12.0), end: Beats(14.0), lanes: vec![(target, "amount".into())],
        });
        { let mut host = h.host(); host.paste_automation(100.0); }
        let lane = &h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()[0];
        assert_eq!(lane.value_at(Beats(12.0)), copied_values.0);
        assert_eq!(lane.value_at(Beats(14.0)), copied_values.1);
        assert!(lane.points.iter().all(|point| point.beat < Beats(100.0)),
            "an empty selected range supplies both the lane and insertion beat");
    }

    #[test]
    fn time_range_cut_flattens_and_undo_restores_exact_curve() {
        let mut h = Harness::new();
        let target = UiGraphTarget::Effect(h.effect_id());
        h.selection.automation_time_selection = Some(AutomationTimeSelection {
            start: Beats(3.0),
            end: Beats(5.0),
            lanes: vec![(target, "amount".into())],
        });
        let before = h.project.clone();
        { let mut host = h.host(); host.cut_selected_automation(); }
        let lane = h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|lane| lane.param_id.as_ref() == "amount").unwrap();
        assert_eq!(lane.value_at(Beats(4.0)), lane.value_at(Beats(3.0)));
        let mut authoritative = before.clone();
        let mut service = EditingService::new();
        drain_batch(&h, &mut authoritative, &mut service);
        assert!(service.undo(&mut authoritative));
        assert_eq!(
            serde_json::to_value(&authoritative.settings.master_effects[0].automation_lanes).unwrap(),
            serde_json::to_value(&before.settings.master_effects[0].automation_lanes).unwrap()
        );
    }

    #[test]
    fn duplicate_preserves_originals_and_empty_clipboard_emits_nothing() {
        let mut h = Harness::new();
        h.select(&[("amount", 2.0), ("amount", 6.0)]);
        let before = h.project.clone();
        { let mut host = h.host(); host.duplicate_selected_automation(); }
        let amount = h.project.settings.master_effects[0].automation_lanes.as_ref().unwrap()
            .iter().find(|lane| lane.param_id.as_ref() == "amount").unwrap();
        assert_eq!(amount.value_at(Beats(6.0)), 0.2);
        assert_eq!(amount.value_at(Beats(10.0)), 0.6);
        let mut authoritative = before;
        let mut service = EditingService::new();
        drain_batch(&h, &mut authoritative, &mut service);
        assert!(service.undo(&mut authoritative));
        assert_eq!(points(&authoritative, "amount"), vec![(2.0, 0.2), (6.0, 0.6), (10.0, 0.8)]);
        let mut empty = Harness::new();
        empty.select(&[("amount", 2.0)]);
        { let mut host = empty.host(); host.paste_automation(12.0); }
        assert!(empty.rx.try_recv().is_err());
    }

    #[test]
    fn missing_source_target_after_project_switch_is_a_safe_noop() {
        let mut h = Harness::new();
        h.select(&[("amount", 2.0)]);
        { let mut host = h.host(); host.copy_selected_automation(); }
        h.project = manifold_core::project::Project::default();
        h.selection.selected_automation_point = None;
        h.selection.selected_automation_points.clear();
        { let mut host = h.host(); host.paste_automation(12.0); }
        assert!(h.rx.try_recv().is_err());
    }

    #[test]
    fn shape_creates_first_lane_and_undo_restores_absence() {
        let mut h = Harness::new();
        h.project.settings.master_effects[0].automation_lanes = None;
        h.project.settings.time_signature_numerator = 3;
        let target = UiGraphTarget::Effect(h.effect_id());
        let mut authoritative = h.project.clone();
        automation::insert_shape(&mut h.project, &mut h.selection, &h.tx, &target,
            &"amount".into(), Beats(5.0), manifold_ui::panels::actions::AutomationShape::Triangle,
            &mut h.needs_rebuild);
        assert_eq!(points(&h.project, "amount"), vec![(5.0, 0.0), (6.5, 1.0), (8.0, 0.0)]);
        let ContentCommand::Execute(command) = h.rx.try_recv().unwrap() else { panic!("one undoable shape"); };
        let mut service = EditingService::new();
        service.execute(command, &mut authoritative);
        assert_eq!(points(&authoritative, "amount"), points(&h.project, "amount"));
        assert!(service.undo(&mut authoritative));
        assert!(authoritative.settings.master_effects[0].automation_lanes.is_none());
    }

    #[test]
    fn cut_keeps_automation_paste_context_and_each_batch_undoes_as_one_step() {
        let mut h = Harness::new();
        h.select(&[("amount", 2.0)]);
        let before = h.project.clone();
        {
            let mut host = h.host();
            host.cut_selected_automation();
        }
        assert_eq!(points(&h.project, "amount"), vec![(6.0, 0.6), (10.0, 0.8)]);
        {
            let mut host = h.host();
            assert!(host.has_automation_paste_target());
            host.paste_automation(12.0);
        }
        assert_eq!(points(&h.project, "amount"), vec![(6.0, 0.6), (10.0, 0.8), (12.0, 0.2)],
            "a single-point paste creates a breakpoint, without zero-duration punch guards");
        let mut authoritative = before;
        let mut service = EditingService::new();
        drain_batch(&h, &mut authoritative, &mut service);
        drain_batch(&h, &mut authoritative, &mut service);
        assert!(service.undo(&mut authoritative));
        assert_eq!(points(&authoritative, "amount"), vec![(6.0, 0.6), (10.0, 0.8)]);
        assert!(service.undo(&mut authoritative));
        assert_eq!(points(&authoritative, "amount"), vec![(2.0, 0.2), (6.0, 0.6), (10.0, 0.8)]);
    }
}
