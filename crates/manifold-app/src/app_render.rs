//! Rendering methods for Application — extracted from app.rs.
//!
//! Contains `tick_and_render()`, `present_all_windows()`, and the text input
//! overlay rendering helper. All methods are `impl Application` blocks that
//! operate on the struct defined in app.rs.

use winit::window::WindowId;

use manifold_renderer::ui_renderer::{TextMode, UIRenderer};

use manifold_ui::panels::PanelAction;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;

impl Application {
    pub(crate) fn tick_and_render(&mut self) {
        let _dt = self.frame_timer.consume_tick();
        let realtime = self.frame_timer.realtime_since_start();
        self.time_since_start = realtime as f32;

        // Content rendering now runs on dedicated thread — no cadence check needed here.

        // 1. Drain state from content thread
        // Deferred audio load request — collected inside the rx borrow, executed after.
        let mut deferred_audio_load: Option<(String, f32)> = None;
        if let Some(ref rx) = self.state_rx {
            // Drain all pending states, keep the latest
            while let Ok(state) = rx.try_recv() {
                // Accept project snapshot if data_version changed (unless drag in progress)
                if let Some(snapshot) = state.project_snapshot {
                    let drag_active = self.overlay.drag_mode() != manifold_ui::interaction_overlay::DragMode::None;
                    // Suppress snapshots until content thread catches up after a local project load.
                    // Safety net: timeout after 120 frames (~2s) to prevent indefinite suppression.
                    const MAX_SUPPRESS_FRAMES: u64 = 120;
                    let suppress_timed_out = self.suppress_snapshot_until > 0
                        && self.frame_count.saturating_sub(self.suppress_snapshot_set_at) >= MAX_SUPPRESS_FRAMES;
                    if suppress_timed_out {
                        log::warn!("[UI] Snapshot suppression timed out — accepting snapshot");
                        self.suppress_snapshot_until = 0;
                    }
                    let suppressed = state.data_version < self.suppress_snapshot_until;

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
                        self.local_project = *snapshot;
                        // Restore actively-dragged inspector field so snapshot
                        // doesn't overwrite the value the user is manipulating.
                        if let Some(ref drag) = self.active_inspector_drag {
                            drag.apply(&mut self.local_project);
                        }
                        // Clear suppression once we've accepted a post-load snapshot
                        self.suppress_snapshot_until = 0;

                        // Sync waveform lane visibility and detect new audio path
                        // from content thread (fresh percussion import or audio-only
                        // import). Actual loading is deferred to after the rx borrow.
                        let current_audio_path = self.local_project.percussion_import
                            .as_ref()
                            .and_then(|p| p.audio_path.clone())
                            .filter(|s| !s.is_empty());
                        if let Some(ref path) = current_audio_path {
                            if !self.ui_root.layout.waveform_lane_visible {
                                self.ui_root.layout.waveform_lane_visible = true;
                                self.needs_rebuild = true;
                            }
                            let already_loaded = self.loaded_audio_path.as_ref()
                                .is_some_and(|lp| lp == path);
                            if !already_loaded && self.pending_audio_load.is_none() {
                                let start_beat = self.local_project.percussion_import
                                    .as_ref()
                                    .map_or(0.0, |p| p.audio_start_beat);
                                deferred_audio_load = Some((path.clone(), start_beat));
                            }
                        } else if self.loaded_audio_path.is_some() {
                            // Audio path was removed (undo, reset) — clear tracking
                            // so a future re-import will trigger loading again.
                            self.loaded_audio_path = None;
                        }

                        // Only trigger structural sync when data_version changed
                        // (editing commands, undo/redo). Modulation-only snapshots
                        // just update param_values — push_state() syncs sliders
                        // every frame without needing a structural rebuild.
                        if version_changed {
                            // Prune selection references to deleted clips/layers
                            let valid_clips: std::collections::HashSet<manifold_core::ClipId> =
                                self.local_project.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                                    .collect();
                            let valid_layers: std::collections::HashSet<manifold_core::LayerId> =
                                self.local_project.timeline.layers.iter()
                                    .map(|l| l.layer_id.clone())
                                    .collect();
                            self.selection.prune_stale_references(&valid_clips, &valid_layers);

                            // Validate active_layer_id
                            if let Some(ref id) = self.active_layer_id
                                && !valid_layers.contains(id)
                            {
                                self.active_layer_id = self.local_project.timeline.layers
                                    .last().map(|l| l.layer_id.clone());
                            }

                            self.needs_structural_sync = true;
                            self.needs_rebuild = true;
                        }
                    }
                }
                self.content_state = ContentState {
                    project_snapshot: None, // consumed above
                    ..state
                };
            }
        }

        // 1a. Trigger deferred audio load (new audio_path from content thread).
        if let Some((path, start_beat)) = deferred_audio_load {
            self.loaded_audio_path = Some(path.clone());
            self.spawn_background_audio_load(path, start_beat);
            log::info!(
                "[Audio] Detected new audio path from content thread, \
                starting background load"
            );
        }

        // 1b. Poll for completed background audio load (waveform stays on UI thread)
        self.poll_pending_audio_load();

        // 1d. Percussion import runs on content thread — read status from content_state.
        let was_importing = false; // previous frame state not tracked here
        let is_importing = self.content_state.percussion_importing;

        // 1e. Sync percussion pipeline status to header panel
        // Port of Unity WorkspaceController.RefreshPercussionImportStatusLabel
        {
            let msg = self.content_state.percussion_status_message.clone();
            let progress = self.content_state.percussion_progress;
            let show = self.content_state.percussion_show_progress && !msg.is_empty();
            self.ui_root.header.set_import_status(
                &mut self.ui_root.tree,
                &msg,
                if progress < 0.0 { 0.0 } else { progress.clamp(0.0, 1.0) },
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

        // 1f. Sync stem mute/solo state from content thread to UI panels.
        // Port of Unity: WorkspaceController.OnStemMuteToggled/OnStemSoloToggled refreshing button visuals.
        {
            for i in 0..manifold_playback::stem_audio::STEM_COUNT {
                self.ui_root.stem_lanes.set_mute_state(i, self.content_state.stem_muted[i]);
                self.ui_root.stem_lanes.set_solo_state(i, self.content_state.stem_soloed[i]);
            }
            // 1g. Sync stem availability — drives Expand button visibility on waveform lane.
            // Port of Unity: WorkspaceController sets SetStemsAvailable when stem PATHS exist.
            // Check project state (file paths resolved), not content_state.stem_available
            // (which tracks loaded audio — only true AFTER expansion).
            let any_stem_available = self.local_project.percussion_import
                .as_ref()
                .and_then(|p| p.stem_paths.as_ref())
                .is_some_and(|paths| !paths.is_empty());
            self.ui_root.waveform_lane.set_stems_available(any_stem_available);

            // 1h. Push visibility/text state to UITree nodes (buttons, labels).
            self.ui_root.update_waveform_stem_nodes();
        }

        // 2. Process UI events and dispatch actions
        let mut actions = self.ui_root.process_events();

        // 2a. Route viewport tracks-area events through InteractionOverlay.
        // These events were stashed by process_events() because the overlay
        // needs &mut TimelineEditingHost which UIRoot can't provide.
        {
            let viewport_events = self.ui_root.drain_viewport_events();
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
                    &mut self.needs_scroll_rebuild,
                    &mut self.pre_drag_commands,
                );
                for event in &viewport_events {
                    use manifold_ui::input::UIEvent;
                    match event {
                        UIEvent::Click { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, modifiers.shift, modifiers.ctrl || modifiers.command,
                                1, false,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DoubleClick { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, modifiers.shift, modifiers.ctrl || modifiers.command,
                                2, false,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::RightClick { pos, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, false, false,
                                1, true,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DragBegin { origin, .. } => {
                            self.overlay.on_begin_drag(
                                *origin, &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::Drag { pos, .. } => {
                            self.overlay.on_drag(
                                *pos, &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { .. } => {
                            self.overlay.on_end_drag(
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
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
        self.ui_root.intercept_overlay_actions(&mut actions);

        // Update effect clipboard count for browser popup
        self.ui_root.effect_clipboard_count = self.effect_clipboard.count();

        // Consume deferred structural sync flag (set by keyboard shortcuts)
        let mut needs_structural_sync = self.needs_structural_sync;
        self.needs_structural_sync = false;
        let mut needs_resolution_resize = false;
        let prev_active_layer = self.active_layer_id.clone();
        let prev_sel_version = self.selection.selection_version;
        for action in &actions {
            // Intercept actions that need Application-level access
            match action {
                PanelAction::ToggleMonitor => { self.pending_toggle_output = true; continue; }
                PanelAction::SaveProject => { self.save_project(); continue; }
                PanelAction::SaveProjectAs => { self.save_project_as(); continue; }
                PanelAction::OpenProject => { self.open_project(); needs_structural_sync = true; continue; }
                PanelAction::OpenRecent => { self.open_recent_project(); needs_structural_sync = true; continue; }
                PanelAction::PasteEffects => {
                    // Browser popup paste button → route through same logic as Cmd+V
                    let tab = self.ui_root.inspector.last_effect_tab();
                    let target = match tab {
                        manifold_ui::InspectorTab::Master => manifold_editing::commands::effect_target::EffectTarget::Master,
                        manifold_ui::InspectorTab::Layer => {
                            let layer_id = self.active_layer_id.clone().unwrap_or_default();
                            manifold_editing::commands::effect_target::EffectTarget::Layer { layer_id }
                        },
                        manifold_ui::InspectorTab::Clip => {
                            if let Some(cid) = self.selection.primary_selected_clip_id.clone() {
                                manifold_editing::commands::effect_target::EffectTarget::Clip { clip_id: cid }
                            } else {
                                let layer_id = self.active_layer_id.clone().unwrap_or_default();
                                manifold_editing::commands::effect_target::EffectTarget::Layer { layer_id }
                            }
                        }
                    };
                    let effects_len = match tab {
                        manifold_ui::InspectorTab::Master => self.local_project.settings.master_effects.len(),
                        manifold_ui::InspectorTab::Layer => self.active_layer_id.as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .map(|e| e.len()).unwrap_or(0),
                        manifold_ui::InspectorTab::Clip => self.selection.primary_selected_clip_id.as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .map(|c| c.effects.len()).unwrap_or(0),
                    };
                    let clones = self.effect_clipboard.get_paste_clones();
                    for (offset, fx) in clones.into_iter().enumerate() {
                        let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                            target.clone(), fx, effects_len + offset,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::BrowserSearchClicked => {
                    let r = self.ui_root.browser_popup.search_bar_rect(&self.ui_root.tree);
                    self.text_input.begin(
                        crate::text_input::TextInputField::SearchFilter,
                        &self.ui_root.browser_popup.current_filter,
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::BpmFieldClicked => {
                    let bpm = Some(&self.local_project).map_or(120.0, |p| p.settings.bpm);
                    let r = self.ui_root.tree.get_bounds(
                        self.ui_root.transport.bpm_field_id() as u32,
                    );
                    self.text_input.begin(
                        crate::text_input::TextInputField::Bpm,
                        &format!("{:.1}", bpm),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        14.0,
                    );
                    continue;
                }
                PanelAction::FpsFieldClicked => {
                    let fps = Some(&self.local_project).map_or(60.0, |p| p.settings.frame_rate);
                    let r = self.ui_root.tree.get_bounds(
                        self.ui_root.footer.fps_field_id() as u32,
                    );
                    self.text_input.begin(
                        crate::text_input::TextInputField::Fps,
                        &format!("{:.0}", fps),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::LayerDoubleClicked(idx) => {
                    // Open text input for layer rename
                    {
                        let project = &self.local_project;
                        if let Some(layer) = project.timeline.layers.get(*idx) {
                            let nid = self.ui_root.layer_headers.name_node_id(*idx);
                            let r = if nid >= 0 {
                                self.ui_root.tree.get_bounds(nid as u32)
                            } else {
                                manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0)
                            };
                            self.text_input.begin(
                                crate::text_input::TextInputField::LayerName(*idx),
                                &layer.name,
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                        }
                    }
                    continue;
                }
                PanelAction::ClipBpmClicked => {
                    // Open text input for clip recorded BPM editing.
                    // Unity: ClipInspector.OnBitmapBpmClicked → BitmapTextInput.BeginEdit
                    if let Some(clip_id) = &self.selection.primary_selected_clip_id {
                        let bpm_text = Some(&self.local_project)
                            .and_then(|p| {
                                p.timeline.layers.iter()
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
                        let r = self.ui_root.inspector.clip_chrome_mut()
                            .bpm_button_rect(&self.ui_root.tree);
                        self.text_input.begin(
                            crate::text_input::TextInputField::ClipBpm,
                            &bpm_text,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            10.0,
                        );
                    }
                    continue;
                }
                PanelAction::NewProject => {
                    let project = Self::create_default_project();
                    self.local_project = project.clone();
                    self.suppress_snapshot_until = self.content_state.data_version + 1;
                    self.suppress_snapshot_set_at = self.frame_count;
                    self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));
                    self.send_content_cmd(ContentCommand::SetProject);
                    self.selection.clear_selection();
                    self.active_layer_id = self.local_project.timeline.layers.first().map(|l| l.layer_id.clone());
                    self.current_project_path = None;
                    needs_structural_sync = true;
                    continue;
                }
                // Transport controller actions — intercept here for Application-level access
                PanelAction::CycleClockAuthority => {
                    self.send_content_cmd(ContentCommand::CycleClockAuthority);
                    continue;
                }
                PanelAction::ToggleLink => {
                    self.send_content_cmd(ContentCommand::ToggleLink);
                    continue;
                }
                PanelAction::ToggleMidiClock => {
                    self.send_content_cmd(ContentCommand::ToggleMidiClock);
                    continue;
                }
                PanelAction::ToggleSyncOutput => {
                    self.send_content_cmd(ContentCommand::ToggleSyncOutput);
                    continue;
                }
                PanelAction::ResetBpm => {
                    self.send_content_cmd(ContentCommand::ResetBpm);
                    self.needs_rebuild = true;
                    continue;
                }
                _ => {}
            }
            let content_tx = self.content_tx.as_ref().unwrap();
            let result = crate::ui_bridge::dispatch(
                action,
                &mut self.local_project,
                content_tx,
                &self.content_state,
                &mut self.ui_root,
                &mut self.selection,
                &mut self.active_layer_id,
                &mut self.slider_snapshot,
                &mut self.trim_snapshot,
                &mut self.adsr_snapshot,
                &mut self.target_snapshot,
                &mut self.user_prefs,
                &mut self.active_inspector_drag,
            );
            if result.structural_change {
                needs_structural_sync = true;
            }
            if result.resolution_changed {
                needs_resolution_resize = true;
            }
        }

        // Resize compositor + generator when resolution preset changes
        if needs_resolution_resize {
            let dims = Some(&self.local_project).map(|p| {
                (p.settings.output_width.max(1) as u32, p.settings.output_height.max(1) as u32)
            });
            if let Some((w, h)) = dims {
                self.send_content_cmd(ContentCommand::ResizeContent(w, h));
                log::info!("Resolution changed to {}x{}", w, h);
            }
        }

        // Selection version change → sync inspector so it shows the newly selected clip
        if self.selection.selection_version != prev_sel_version && !needs_structural_sync {
            let active_idx = self.active_layer_id.as_ref().and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, active_idx, &self.selection);
            needs_structural_sync = true;
        }

        if needs_structural_sync {
            let active_idx = self.active_layer_id.as_ref().and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.local_project, active_idx, &self.selection);
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, active_idx, &self.selection);
        } else if self.active_layer_id != prev_active_layer {
            let active_idx = self.active_layer_id.as_ref().and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, active_idx, &self.selection);
            needs_structural_sync = true; // Inspector content changed — needs rebuild
        }
        // 2a. Per-frame drag polling with auto-scroll.
        // InteractionOverlay.PollMoveDrag — continues edge auto-scroll when mouse is stationary.
        {
            use manifold_ui::interaction_overlay::DragMode;
            if self.overlay.drag_mode() == DragMode::Move {
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.needs_scroll_rebuild,
                    &mut self.pre_drag_commands,
                );
                self.overlay.poll_move_drag(
                    self.cursor_pos, &mut host, &mut self.selection, &self.ui_root.viewport,
                );
            }
        }
        // Legacy drag polling removed — overlay.poll_move_drag() handles it above.

        // 2b. Auto-scroll check for playback (BEFORE build so rebuild includes new scroll)
        let auto_scroll_changed = crate::ui_bridge::check_auto_scroll(&mut self.ui_root, &self.content_state, &self.local_project);
        let overlay_changed = self.ui_root.overlay_dirty;
        self.ui_root.overlay_dirty = false;
        let scroll_changed = auto_scroll_changed || self.needs_scroll_rebuild || overlay_changed;
        self.needs_scroll_rebuild = false;

        // 3. Rebuild if needed
        // Full rebuild: structural changes, data mutations, or explicit needs_rebuild.
        // Partial rebuild: only scroll/zoom changed — rebuild viewport + layer_headers,
        // preserve transport, header, footer, inspector nodes.
        // From Unity: CheckScrollAndInvalidate only repaints affected layers.
        //
        // GUARD: If the inspector has an active drag (slider being dragged), defer
        // the rebuild to prevent node destruction mid-drag which causes snap-back.
        // Unity avoids this because rebuilds only happen on structural changes and
        // SyncValues() dirty-checks against the data model without rebuilding.
        let inspector_dragging = self.ui_root.inspector.is_dragging();
        let layer_dragging = self.ui_root.layer_headers.is_dragging();
        if self.needs_rebuild || needs_structural_sync {
            if inspector_dragging {
                // Defer — keep needs_rebuild set so it fires after drag ends
                // But still rebuild scroll panels if needed (they're separate from inspector)
                if scroll_changed {
                    self.ui_root.rebuild_scroll_panels();
                }
            } else if layer_dragging {
                // Defer — rebuilding scroll panels while a layer drag is active would
                // destroy the node IDs that handle_drag / handle_drag_end depend on.
            } else {
                self.needs_rebuild = false;
                self.ui_root.build();
                // Re-apply effect card selection visuals after rebuild —
                // structural changes recreate cards with is_selected=false.
                self.ui_root.inspector.apply_selection_visuals(&mut self.ui_root.tree);
            }
        } else if scroll_changed && !layer_dragging {
            self.ui_root.rebuild_scroll_panels();
        }

        // 4. Push engine state to UI panels (AFTER build so new nodes get state)
        let active_idx = self.active_layer_id.as_ref().and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        crate::ui_bridge::push_state(
            &mut self.ui_root,
            &self.local_project,
            &self.content_state,
            active_idx,
            &self.selection,
            self.content_state.editing_is_dirty,
            self.current_project_path.as_deref(),
        );

        // 4b. Sync clip positions from live project model every frame.
        // During drag, the InteractionOverlay mutates clip data directly in the
        // project model, but the viewport's clips_by_layer cache is only refreshed
        // via sync_project_data() (structural sync). This per-frame sync ensures
        // bitmap renderers see mutated clip positions and repaint during drag.
        // Cost: iterates layers+clips, but the bitmap fingerprint skips repaint
        // when nothing changed (cheap no-op outside of drag).
        crate::ui_bridge::sync_clip_positions(&mut self.ui_root, &self.local_project);

        // 5. Push performance metrics to HUD
        if self.ui_root.perf_hud.is_visible() {
            let bpm = Some(&self.local_project).map(|p| p.settings.bpm).unwrap_or(120.0);
            let clock_source = Some(&self.local_project)
                .map(|p| p.settings.clock_authority.display_name().to_string())
                .unwrap_or_else(|| "Internal".to_string());
            self.ui_root.perf_hud.set_metrics(manifold_ui::panels::perf_hud::PerfMetrics {
                ui_fps: self.frame_timer.current_fps() as f32,
                ui_frame_time_ms: (self.frame_timer.last_dt() * 1000.0) as f32,
                render_fps: self.content_state.content_fps,
                render_frame_time_ms: self.content_state.content_frame_time_ms,
                active_clips: self.content_state.active_clips,
                preparing_clips: 0,
                current_beat: self.content_state.current_beat,
                current_time_secs: self.content_state.current_time,
                bpm,
                clock_source,
                is_playing: self.content_state.is_playing,
                data_version: self.content_state.data_version,
                profiling_active: self.content_state.profiling_active,
                profiling_frame_count: self.content_state.profiling_frame_count,
            });
        }

        // 6. Lightweight update (playhead, insert cursor, layer selection, HUD values)
        self.ui_root.update();

        // 6a. Update waveform lane overlay (position for dirty-checking)
        {
            let scroll_x = self.ui_root.viewport.scroll_x_beats() * self.ui_root.viewport.pixels_per_beat();
            let wf = &mut self.ui_root.waveform_lane;
            if wf.is_ready() {
                // Get start beat and duration from project percussion import state
                let (start_beat, duration_beats) = if let Some(proj) = Some(&self.local_project) {
                    if let Some(ref perc) = proj.percussion_import {
                        let dur_sec = wf.clip_duration_seconds();
                        let bpm = proj.settings.bpm.max(1.0);
                        let dur_beats = dur_sec * bpm / 60.0;
                        (perc.audio_start_beat, dur_beats)
                    } else {
                        (0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0)
                };
                let mapper = self.ui_root.viewport.mapper();
                wf.update_overlay(
                    start_beat,
                    duration_beats,
                    scroll_x,
                    self.ui_root.viewport.tracks_rect().width,
                    mapper,
                );
            }

            // 6a-ii. Update stem lane overlay (same position/scroll as master).
            if self.ui_root.stem_lanes.is_expanded() {
                let start_beat = self.local_project.percussion_import
                    .as_ref()
                    .map_or(0.0, |perc| perc.audio_start_beat);
                let bpm = self.local_project.settings.bpm.max(1.0);
                let mapper = self.ui_root.viewport.mapper();
                self.ui_root.stem_lanes.update_overlay(
                    start_beat,
                    scroll_x,
                    bpm,
                    mapper,
                );
            }
        }

        // 6b. Repaint dirty layer bitmaps (CPU pixel painting).
        // Build BitmapRepaintState from current selection/hover.
        {
            let hovered = self.ui_root.viewport.hovered_clip_id().map(|s| s.to_string());
            let sel_region = self.ui_root.viewport.selection_region_ref().cloned();
            let has_region = sel_region.is_some();
            let insert_cursor_beat = self.ui_root.viewport.insert_cursor_beat();
            let insert_layer = self.selection.insert_cursor_layer_id.as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            let has_insert = self.selection.has_insert_cursor();
            let ppb = self.ui_root.viewport.pixels_per_beat();
            let sel_ver = self.selection.selection_version;

            let state = manifold_ui::BitmapRepaintState {
                selection_version: sel_ver,
                is_selected: &|id: &str| self.selection.is_selected(id),
                hovered_clip_id: hovered.as_deref(),
                has_region,
                region: sel_region.as_ref(),
                has_insert_cursor: has_insert,
                insert_cursor_beat,
                insert_cursor_layer: insert_layer,
                pixels_per_beat: ppb,
            };
            self.ui_root.viewport.repaint_dirty_layers(&state);
        }

        // 6c. Upload dirty layer textures to GPU
        if let (Some(gpu), Some(bitmap_gpu)) = (&self.gpu, &mut self.layer_bitmap_gpu) {
            for (layer_idx, pixels, tw, th) in self.ui_root.viewport.dirty_layer_iter() {
                bitmap_gpu.upload_layer(
                    &gpu.device, &gpu.queue,
                    layer_idx, pixels, tw as u32, th as u32,
                );
            }

            // 6d. Repaint + upload waveform lane if dirty
            let wf_rect = self.ui_root.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                let wf = &mut self.ui_root.waveform_lane;
                // Force dirty on resize
                if wf.buffer_width != wf_rect.width as usize {
                    wf.dirty = true;
                }
                if wf.dirty {
                    wf.repaint(wf_rect.width as usize);
                    // Upload after repaint
                    if wf.buffer_width > 0 && wf.buffer_height > 0 && !wf.pixel_buffer.is_empty() {
                        bitmap_gpu.upload_layer(
                            &gpu.device, &gpu.queue,
                            1000, &wf.pixel_buffer,
                            wf.buffer_width as u32, wf.buffer_height as u32,
                        );
                    }
                }
            }

            // 6e. Repaint + upload stem lanes if dirty
            if self.ui_root.stem_lanes.is_expanded() {
                let sl_rect = self.ui_root.viewport.stem_lanes_rect();
                if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                    let sl = &mut self.ui_root.stem_lanes;
                    let mapper = self.ui_root.viewport.mapper();
                    if sl.buffer_width != sl_rect.width as usize {
                        sl.dirty = true;
                    }
                    if sl.dirty {
                        sl.repaint(sl_rect.width as usize, mapper);
                        if sl.buffer_width > 0 && sl.buffer_height > 0 && !sl.pixel_buffer.is_empty() {
                            bitmap_gpu.upload_layer(
                                &gpu.device, &gpu.queue,
                                1001, &sl.pixel_buffer,
                                sl.buffer_width as u32, sl.buffer_height as u32,
                            );
                        }
                    }
                }
            }
        }

        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        // Present using IOSurface shared texture (dual device, zero GPU copy).
        // The content thread writes to the IOSurface-backed texture on its device;
        // the UI device reads the same GPU memory via its own imported texture.
        #[cfg(target_os = "macos")]
        {
            // Detect bridge resize (generation changed) and re-import UI texture.
            if let Some(ref bridge) = self.shared_texture_bridge {
                let bridge_gen = bridge.generation();
                if bridge_gen != self.last_bridge_generation {
                    self.last_bridge_generation = bridge_gen;
                    let ui_tex = unsafe { bridge.import_texture(&gpu.device) };
                    self.ui_shared_view = Some(ui_tex.create_view(&wgpu::TextureViewDescriptor::default()));
                    self.ui_shared_texture = Some(ui_tex);
                    log::info!("[UI] re-imported IOSurface texture after resize (gen={})", bridge_gen);
                }
            }
            let view = self.ui_shared_view.clone();
            if let Some(ref v) = view {
                self.present_all_windows(v);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Fallback: single-device SharedOutputView (non-macOS)
            let compositor_view = self.content_pipeline_output.as_ref()
                .and_then(|shared| shared.get_view());
            if let Some(ref view) = compositor_view {
                self.present_all_windows(view);
            }
        }

        self.frame_count += 1;
    }

    fn present_all_windows(&mut self, compositor_output: &wgpu::TextureView) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };
        let blit = match &self.blit_pipeline {
            Some(b) => b,
            None => return,
        };
        // Compositor aspect ratio for aspect-correct blitting (FitInParent)
        let (comp_w, comp_h) = self.content_pipeline_output.as_ref()
            .map(|p| p.get_dimensions())
            .unwrap_or((1920, 1080));
        let source_aspect = comp_w as f32 / comp_h as f32;

        let window_ids: Vec<WindowId> = self.window_registry.iter().map(|(id, _)| *id).collect();

        for window_id in window_ids {
            let is_workspace = Some(window_id) == self.primary_window_id;

            let ws = match self.window_registry.get_mut(&window_id) {
                Some(ws) => ws,
                None => continue,
            };

            let surface_texture = match ws.surface.get_current_texture() {
                Ok(t) => t,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    ws.surface.resize(
                        &gpu.device,
                        ws.surface.width,
                        ws.surface.height,
                        ws.surface.scale_factor,
                    );
                    continue;
                }
                Err(e) => {
                    log::error!("Surface error: {e}");
                    continue;
                }
            };

            let surface_view = surface_texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let surface_w = ws.surface.width;
            let surface_h = ws.surface.height;
            let scale = ws.surface.scale_factor;

            let mut encoder =
                gpu.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Blit Encoder"),
                    });

            if is_workspace {
                // Blit compositor output into the video preview area only (not fullscreen)
                let video_rect = self.ui_root.layout.video_area();
                let sf = scale as f32;
                // Clear surface first (black background for areas outside video)
                {
                    let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Clear Surface"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &surface_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                blit.blit_to_rect_fit(
                    &gpu.device, &mut encoder, compositor_output, &surface_view,
                    video_rect.x * sf, video_rect.y * sf,
                    video_rect.width * sf, video_rect.height * sf,
                    source_aspect,
                );
            } else {
                // Output windows: project resolution centered with letterbox/pillarbox.
                // Clear to black first (bars around content when window != project aspect).
                let output_blit = self.output_blit_pipeline.as_ref().unwrap_or(blit);
                {
                    let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Clear Output"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &surface_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                output_blit.blit_to_rect_fit(
                    &gpu.device, &mut encoder, compositor_output, &surface_view,
                    0.0, 0.0, surface_w as f32, surface_h as f32,
                    source_aspect,
                );
            }

            // Draw UI overlay on workspace window using the UITree
            // Pass logical pixel dimensions — the tree is built in logical coords
            if is_workspace {
                let logical_w = (surface_w as f64 / scale) as u32;
                let logical_h = (surface_h as f64 / scale) as u32;

                // Reset overlay TextRenderer pool index so each overlay pass
                // this frame gets its own TextRenderer (prevents vertex buffer
                // destruction before encoder submission).
                if let Some(ui) = &mut self.ui_renderer {
                    ui.begin_frame();
                }

                // Pass 1: UITree rects + text (track backgrounds, ruler, chrome panels).
                // Skip overlay nodes that render after bitmap textures: waveform/stem
                // lane buttons (Pass 2b), perf HUD (Pass 3b), popups (Pass 4).
                // Uses TextMode::Main so base UI text goes to the main TextRenderer's
                // own vertex buffer, isolated from the overlay TextRenderer.
                if let Some(ui) = &mut self.ui_renderer {
                    // Earliest overlay node — skip everything from here onwards in Pass 1.
                    let wf_first = self.ui_root.waveform_lane.first_node();
                    let sl_first = self.ui_root.stem_lanes.first_node();
                    let mut skip_from: Option<usize> = None;
                    if wf_first != usize::MAX {
                        skip_from = Some(wf_first);
                    } else if sl_first != usize::MAX {
                        skip_from = Some(sl_first);
                    }
                    if skip_from.is_none() {
                        if self.ui_root.perf_hud.is_visible() {
                            skip_from = Some(self.ui_root.perf_hud.first_node());
                        } else if self.ui_root.dropdown.is_open() {
                            skip_from = Some(self.ui_root.dropdown.first_node());
                        } else if self.ui_root.browser_popup.is_open() {
                            skip_from = Some(self.ui_root.browser_popup.first_node());
                        }
                    }
                    ui.render_tree(&self.ui_root.tree, skip_from);
                    ui.render(
                        &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                        logical_w, logical_h, scale, TextMode::Main,
                    );
                }

                // Pass 2: Layer bitmap textures + waveform lane (alpha-blend over track BGs)
                if let Some(bitmap_gpu) = &mut self.layer_bitmap_gpu {
                    let mut rects = self.ui_root.viewport.layer_bitmap_rects();

                    // Add waveform lane rect (texture at reserved index 1000)
                    let wf_rect = self.ui_root.viewport.waveform_lane_rect();
                    if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                        rects.push((1000, wf_rect));
                    }

                    // Add stem lanes rect (texture at reserved index 1001)
                    if self.ui_root.stem_lanes.is_expanded() {
                        let sl_rect = self.ui_root.viewport.stem_lanes_rect();
                        if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                            rects.push((1001, sl_rect));
                        }
                    }

                    if !rects.is_empty() {
                        bitmap_gpu.render_layers(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, &rects,
                        );
                    }
                }

                // Pass 2b: Waveform/stem lane buttons — render ON TOP of bitmap textures.
                // These nodes were skipped in Pass 1 so the bitmap wouldn't cover them.
                {
                    let wf_first = self.ui_root.waveform_lane.first_node();
                    let sl_first = self.ui_root.stem_lanes.first_node();
                    let overlay_end = self.ui_root.perf_hud.first_node();

                    let overlay_start = if wf_first != usize::MAX {
                        Some(wf_first)
                    } else if sl_first != usize::MAX {
                        Some(sl_first)
                    } else {
                        None
                    };

                    if let (Some(start), Some(ui)) =
                        (overlay_start, self.ui_renderer.as_mut())
                    {
                        ui.render_overlay_range(
                            &self.ui_root.tree,
                            start,
                            overlay_end,
                        );
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
                }

                // Pass 3: Unified playhead line spanning ruler → waveform → stems → tracks.
                // Single overlay quad eliminates per-bitmap integer-truncation drift.
                // TextMode::Skip — no text, no glyphon prepare(), no buffer mutation.
                if let Some(ui) = &mut self.ui_renderer
                    && let Some(px) = self.ui_root.viewport.playhead_pixel() {
                        let ruler = self.ui_root.viewport.ruler_rect();
                        let tr = self.ui_root.viewport.get_tracks_rect();
                        let top = ruler.y;
                        let height = (tr.y + tr.height) - top;
                        ui.draw_rect(
                            px - 1.0, top,
                            manifold_ui::color::PLAYHEAD_WIDTH, height,
                            manifold_ui::color::PLAYHEAD_RED.to_f32(),
                        );
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Skip,
                        );
                    }

                // Pass 3b: Perf HUD — renders on top of bitmaps and playhead.
                // Uses its own overlay pass so it's not covered by layer textures.
                if self.ui_root.perf_hud.is_visible()
                    && let Some(ui) = &mut self.ui_renderer {
                        // Render only perf HUD nodes (from first_node up to dropdown/browser start)
                        let hud_start = self.ui_root.perf_hud.first_node();
                        let hud_end = if self.ui_root.dropdown.is_open() {
                            self.ui_root.dropdown.first_node()
                        } else if self.ui_root.browser_popup.is_open() {
                            self.ui_root.browser_popup.first_node()
                        } else {
                            usize::MAX
                        };
                        ui.render_overlay_range(&self.ui_root.tree, hud_start, hud_end);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }

                // Pass 4: Overlay popups — render ON TOP of layer bitmaps and playhead.
                // Uses TextMode::Overlay so popup text goes to a separate TextRenderer
                // with its own vertex buffer, preventing corruption of Pass 1's text.
                if self.ui_root.dropdown.is_open() {
                    if let Some(ui) = &mut self.ui_renderer {
                        let start = self.ui_root.dropdown.first_node();
                        ui.render_overlay(&self.ui_root.tree, start);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
                } else if self.ui_root.browser_popup.is_open()
                    && let Some(ui) = &mut self.ui_renderer {
                        let start = self.ui_root.browser_popup.first_node();
                        ui.render_overlay(&self.ui_root.tree, start);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }

                // Pass 4b: Effect card drag ghost/indicator overlay.
                if let Some(start) = self.ui_root.inspector.card_drag_first_node()
                    && let Some(ui) = &mut self.ui_renderer {
                        ui.render_overlay(&self.ui_root.tree, start);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }

                // Pass 5: Text input overlay — renders on top of everything.
                // Uses immediate-mode draw_rect + draw_text (no UITree nodes needed).
                if self.text_input.active
                    && let Some(ui) = &mut self.ui_renderer {
                        render_text_input_overlay(&self.text_input, &self.frame_timer, ui);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
            }

            gpu.queue.submit(std::iter::once(encoder.finish()));
            surface_texture.present();
        }
    }
}

// ── Text input overlay rendering (free function to avoid borrow conflicts) ──

/// Render the text input overlay using immediate-mode draw calls.
fn render_text_input_overlay(
    ti: &crate::text_input::TextInputState,
    timer: &crate::frame_timer::FrameTimer,
    ui: &mut UIRenderer,
) {
    use crate::text_input::*;

    let a = &ti.anchor;
    let fs = ti.font_size;
    let pad_h = TEXT_INPUT_PAD_H;
    let pad_v = TEXT_INPUT_PAD_V;

    let bg_x = a.x;
    let bg_y = a.y;
    let bg_w = a.width.max(40.0);
    let bg_h = a.height.max(fs + pad_v * 2.0);

    ui.draw_bordered_rect(
        bg_x, bg_y, bg_w, bg_h,
        TEXT_INPUT_BG,
        3.0,
        1.0,
        [0.35, 0.45, 0.7, 0.8],
    );

    // Selection highlight (when select_all)
    if ti.select_all && !ti.text.is_empty() {
        let text_w = ti.text.len() as f32 * fs * 0.6;
        ui.draw_rect(
            bg_x + pad_h, bg_y + pad_v,
            text_w.min(bg_w - pad_h * 2.0), bg_h - pad_v * 2.0,
            TEXT_INPUT_SELECT_BG,
        );
    }

    // Text
    let text_x = bg_x + pad_h;
    let text_y = bg_y + pad_v;
    ui.draw_text(text_x, text_y, &ti.text, fs, TEXT_INPUT_FG);

    // Blinking cursor
    if !ti.select_all {
        let elapsed = timer.realtime_since_start();
        let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
        if blink_on {
            let chars_before = ti.text[..ti.cursor].chars().count();
            let cursor_x = text_x + chars_before as f32 * fs * 0.6;
            ui.draw_rect(
                cursor_x, bg_y + pad_v,
                TEXT_INPUT_CURSOR_W, bg_h - pad_v * 2.0,
                TEXT_INPUT_CURSOR,
            );
        }
    }
}
