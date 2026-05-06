//! Rendering methods for Application — extracted from app.rs.
//!
//! Contains `tick_and_render()`, `present_all_windows()`, and the text input
//! overlay rendering helper. All methods are `impl Application` blocks that
//! operate on the struct defined in app.rs.

use manifold_renderer::ui_renderer::UIRenderer;

use manifold_ui::node::FontWeight;
use manifold_ui::panels::PanelAction;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;

impl Application {
    pub(crate) fn tick_and_render(&mut self) {
        let _dt = self.frame_timer.consume_tick();
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

        // 1. Drain state from content thread
        // Deferred audio load request — collected inside the rx borrow, executed after.
        let mut deferred_audio_load: Option<(String, f32)> = None;
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
                        let current_audio_path = self
                            .local_project
                            .percussion_import
                            .as_ref()
                            .and_then(|p| p.audio_path.clone())
                            .filter(|s| !s.is_empty());
                        if let Some(ref path) = current_audio_path {
                            if !self.ws.ui_root.layout.waveform_lane_visible {
                                self.ws.ui_root.layout.waveform_lane_visible = true;
                                self.needs_rebuild = true;
                            }
                            let already_loaded =
                                self.loaded_audio_path.as_ref().is_some_and(|lp| lp == path);
                            if !already_loaded && self.pending_audio_load.is_none() {
                                let start_beat = self
                                    .local_project
                                    .percussion_import
                                    .as_ref()
                                    .map_or(0.0, |p| p.audio_start_beat.as_f32());
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
                    // Restore actively-dragged inspector field so modulation
                    // doesn't overwrite the value the user is manipulating.
                    if let Some(ref drag) = self.active_inspector_drag {
                        drag.apply(&mut self.local_project);
                    }
                }
                self.content_state = ContentState {
                    project_snapshot: None,    // consumed above
                    modulation_snapshot: None, // consumed above
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

        // 1c. Push the latest graph snapshot into the editor canvas
        // (read-only viewer of the running NodeGraphTestFX).
        if let (Some(canvas), Some(snap)) = (
            self.graph_canvas.as_mut(),
            self.content_state.active_graph_snapshot.as_ref(),
        ) {
            canvas.set_snapshot(snap);
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
                &mut self.ws.ui_root.tree,
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

        // 1e2. Sync live recording state to layer header record button.
        self.ws.ui_root.layer_headers.set_recording_active(
            &mut self.ws.ui_root.tree,
            self.content_state.is_live_recording,
        );

        // 1f. Sync stem mute/solo state from content thread to UI panels.
        // Port of Unity: WorkspaceController.OnStemMuteToggled/OnStemSoloToggled refreshing button visuals.
        {
            for i in 0..manifold_playback::stem_audio::STEM_COUNT {
                self.ws.ui_root
                    .stem_lanes
                    .set_mute_state(i, self.content_state.stem_muted[i]);
                self.ws.ui_root
                    .stem_lanes
                    .set_solo_state(i, self.content_state.stem_soloed[i]);
            }
            // 1g. Sync stem availability — drives Expand button visibility on waveform lane.
            // Port of Unity: WorkspaceController sets SetStemsAvailable when stem PATHS exist.
            // Check project state (file paths resolved), not content_state.stem_available
            // (which tracks loaded audio — only true AFTER expansion).
            let any_stem_available = self
                .local_project
                .percussion_import
                .as_ref()
                .and_then(|p| p.stem_paths.as_ref())
                .is_some_and(|paths| !paths.is_empty());
            self.ws.ui_root
                .waveform_lane
                .set_stems_available(any_stem_available);

            // 1h. Push visibility/text state to UITree nodes (buttons, labels).
            self.ws.ui_root.update_waveform_stem_nodes();
        }

        // 2. Process UI events and dispatch actions
        let mut actions = self.ws.ui_root.process_events();

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
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { .. } => {
                            self.overlay.on_end_drag(
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
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
        for action in &actions {
            // Intercept actions that need Application-level access
            match action {
                PanelAction::CopyOscAddress(addr) => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(addr.clone());
                    }
                    continue;
                }
                PanelAction::ToggleLiveRecording => {
                    if self.content_state.is_live_recording {
                        self.send_content_cmd(ContentCommand::StopLiveRecording);
                    } else {
                        let mut config =
                            manifold_recording::LiveRecordingConfig::default_to_desktop();
                        config.audio_device =
                            self.ws.ui_root.selected_audio_input_device.clone();
                        self.send_content_cmd(
                            ContentCommand::StartLiveRecording(Box::new(config)),
                        );
                    }
                    continue;
                }
                PanelAction::SetAudioInputDevice(name) => {
                    let display = if name.is_empty() {
                        self.ws.ui_root.selected_audio_input_device = None;
                        "No audio input".to_string()
                    } else {
                        self.ws.ui_root.selected_audio_input_device = Some(name.clone());
                        name.clone()
                    };
                    self.ws.ui_root.layer_headers.set_audio_device_name(
                        &mut self.ws.ui_root.tree,
                        &display,
                    );
                    continue;
                }
                PanelAction::ToggleMonitor => {
                    self.pending_toggle_output = true;
                    continue;
                }
                PanelAction::OpenGraphEditor(ei) => {
                    // Resolve `ei` (effect index in the active inspector
                    // tab) to the effect's type id, then ask the content
                    // thread to start snapshotting that specific graph.
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    let type_id = match tab {
                        manifold_ui::InspectorTab::Master => self
                            .local_project
                            .settings
                            .master_effects
                            .get(*ei)
                            .map(|e| e.effect_type().clone()),
                        manifold_ui::InspectorTab::Layer => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .and_then(|effects| effects.get(*ei))
                            .map(|e| e.effect_type().clone()),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .and_then(|c| c.effects.get(*ei))
                            .map(|e| e.effect_type().clone()),
                    };
                    if let Some(tid) = type_id {
                        self.send_content_cmd(ContentCommand::WatchEffectGraph(Some(tid)));
                    }
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::EnterPerformMode => {
                    self.perform.pending_enter = true;
                    continue;
                }
                PanelAction::SaveProject => {
                    self.save_project();
                    continue;
                }
                PanelAction::SaveProjectAs => {
                    self.save_project_as();
                    continue;
                }
                PanelAction::ExportVideo => {
                    self.start_export();
                    continue;
                }
                PanelAction::OpenProject => {
                    self.open_project();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::OpenRecent => {
                    self.open_recent_project();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::PasteEffects => {
                    // Browser popup paste button → route through same logic as Cmd+V
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    let target = match tab {
                        manifold_ui::InspectorTab::Master => {
                            manifold_editing::commands::effect_target::EffectTarget::Master
                        }
                        manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Clip => {
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
                        manifold_ui::InspectorTab::Layer => self
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
                PanelAction::BrowserSearchClicked => {
                    let r = self
                        .ws
                        .ui_root
                        .browser_popup
                        .search_bar_rect(&self.ws.ui_root.tree);
                    self.text_input.begin(
                        crate::text_input::TextInputField::SearchFilter,
                        &self.ws.ui_root.browser_popup.current_filter,
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::BpmFieldClicked => {
                    let bpm = Some(&self.local_project).map_or(120.0, |p| p.settings.bpm.0);
                    let r = self
                        .ws
                        .ui_root
                        .tree
                        .get_bounds(self.ws.ui_root.transport.bpm_field_id() as u32);
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
                    let r = self
                        .ws
                        .ui_root
                        .tree
                        .get_bounds(self.ws.ui_root.footer.fps_field_id() as u32);
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
                            let nid = self.ws.ui_root.layer_headers.name_node_id(*idx);
                            let r = if nid >= 0 {
                                self.ws.ui_root.tree.get_bounds(nid as u32)
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
                PanelAction::MarkerDoubleClicked(marker_id_str) => {
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
                PanelAction::ClipBpmClicked => {
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
                PanelAction::GenStringParamClicked(sp_idx) => {
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
                PanelAction::GenStringParamDropdownClicked(sp_idx) => {
                    // Open a dropdown for a string param (e.g. font selector).
                    if let Some(gp) = self.ws.ui_root.inspector.gen_params()
                        && let Some(sp) = gp.string_param(*sp_idx)
                    {
                        let key = sp.key.clone();
                        if let Some(r) = gp.string_param_rect(&self.ws.ui_root.tree, *sp_idx) {
                            let items: Vec<manifold_ui::panels::dropdown::DropdownItem> =
                                if key == "fontFamily" {
                                    manifold_renderer::text_rasterizer::TextRasterizer::available_font_families()
                                        .into_iter()
                                        .map(|name| manifold_ui::panels::dropdown::DropdownItem::new(&name))
                                        .collect()
                                } else {
                                    vec![]
                                };
                            if !items.is_empty() {
                                let trigger = manifold_ui::node::Rect::new(
                                    r.x, r.y, r.width, r.height,
                                );
                                self.ws.ui_root.open_dropdown_at(
                                    crate::ui_root::DropdownContext::GenStringParamDropdown(*sp_idx),
                                    items,
                                    trigger,
                                );
                            }
                        }
                    }
                    continue;
                }
                PanelAction::MacroLabelRename(idx) => {
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
                PanelAction::NewProject => {
                    let action = self.project_io.new_project();
                    self.apply_project_io_action(action);
                    needs_structural_sync = true;
                    continue;
                }
                // Transport controller actions — intercept here for Application-level access
                // CycleClockAuthority removed — authority is auto-determined from enabled sources
                PanelAction::ToggleLink => {
                    self.send_content_cmd(ContentCommand::ToggleLink);
                    continue;
                }
                PanelAction::ToggleMidiClock => {
                    self.send_content_cmd(ContentCommand::ToggleMidiClock);
                    continue;
                }
                PanelAction::ToggleSyncOutput => {
                    self.send_content_cmd(ContentCommand::ToggleOscSyncMode);
                    continue;
                }
                PanelAction::SetMidiClockDevice(index) => {
                    self.send_content_cmd(ContentCommand::SetMidiClockDevice(*index));
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
                &mut self.ws.ui_root,
                &mut self.selection,
                &mut self.active_layer_id,
                &mut self.slider_snapshot,
                &mut self.trim_snapshot,
                &mut self.adsr_snapshot,
                &mut self.target_snapshot,
                &mut self.range_snapshot,
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
            );
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
                    &mut self.scroll_dirty,
                    &mut self.invalidate_layers,
                    &mut self.pre_drag_commands,
                );
                self.overlay.poll_move_drag(
                    self.cursor_pos,
                    &mut host,
                    &mut self.selection,
                    &self.ws.ui_root.viewport,
                );
            }
        }
        // Legacy drag polling removed — overlay.poll_move_drag() handles it above.

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
        let scroll_dirty = self.scroll_dirty;
        self.scroll_dirty.clear();

        // 3. Rebuild if needed
        // Full rebuild: structural changes, data mutations, or explicit needs_rebuild.
        // Partial rebuild: only scroll/zoom changed — rebuild viewport + layer_headers,
        // preserve transport, header, footer, inspector nodes.
        // Horizontal-only scroll skips layer header rebuild entirely.
        //
        // GUARD: If the inspector has an active drag (slider being dragged), defer
        // the rebuild to prevent node destruction mid-drag which causes snap-back.
        let inspector_dragging = self.ws.ui_root.inspector.is_dragging();
        let layer_dragging = self.ws.ui_root.layer_headers.is_dragging();
        if self.needs_rebuild || needs_structural_sync {
            if inspector_dragging {
                // Defer — keep needs_rebuild set so it fires after drag ends
                // But still rebuild scroll panels if needed (they're separate from inspector)
                if scroll_dirty.any() {
                    self.ws.ui_root.rebuild_scroll_panels(scroll_dirty);
                    if let Some(cm) = &mut self.ui_cache_manager {
                        cm.invalidate_scroll_panels();
                    }
                }
            } else if layer_dragging {
                // Defer — rebuilding scroll panels while a layer drag is active would
                // destroy the node IDs that handle_drag / handle_drag_end depend on.
            } else {
                self.needs_rebuild = false;
                self.ws.ui_root.build();
                // Re-apply effect card selection visuals after rebuild —
                // structural changes recreate cards with is_selected=false.
                self.ws.ui_root
                    .inspector
                    .apply_selection_visuals(&mut self.ws.ui_root.tree);
                if let Some(cm) = &mut self.ui_cache_manager {
                    cm.invalidate_all();
                }
            }
        } else if scroll_dirty.any() && !layer_dragging {
            self.ws.ui_root.rebuild_scroll_panels(scroll_dirty);
            if let Some(cm) = &mut self.ui_cache_manager {
                cm.invalidate_scroll_panels();
            }
        }

        #[cfg(target_os = "macos")]
        self.sync_workspace_preview_size();

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
            crate::ui_bridge::sync_clip_positions(&mut self.ws.ui_root, &self.local_project);
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
            self.ws.ui_root
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

        // 6a. Update waveform lane overlay (position for dirty-checking)
        {
            let scroll_x = self.ws.ui_root.viewport.scroll_x_beats().as_f32()
                * self.ws.ui_root.viewport.pixels_per_beat();
            let wf = &mut self.ws.ui_root.waveform_lane;
            if wf.is_ready() {
                // Get start beat and duration from project percussion import state
                let (start_beat, duration_beats) = if let Some(proj) = Some(&self.local_project) {
                    if let Some(ref perc) = proj.percussion_import {
                        let dur_sec = wf.clip_duration_seconds();
                        let bpm = proj.settings.bpm.0.max(1.0);
                        let dur_beats = dur_sec * bpm / 60.0;
                        (perc.audio_start_beat.as_f32(), dur_beats)
                    } else {
                        (0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0)
                };
                let mapper = self.ws.ui_root.viewport.mapper();
                wf.update_overlay(
                    start_beat,
                    duration_beats,
                    scroll_x,
                    self.ws.ui_root.viewport.tracks_rect().width,
                    mapper,
                );
            }

            // 6a-ii. Update stem lane overlay (same position/scroll as master).
            if self.ws.ui_root.stem_lanes.is_expanded() {
                let start_beat = self
                    .local_project
                    .percussion_import
                    .as_ref()
                    .map_or(0.0, |perc| perc.audio_start_beat.as_f32());
                let bpm = self.local_project.settings.bpm.0.max(1.0);
                let mapper = self.ws.ui_root.viewport.mapper();
                self.ws.ui_root
                    .stem_lanes
                    .update_overlay(start_beat, scroll_x, bpm, mapper);
            }
        }

        // 6b. Repaint dirty layer bitmaps (CPU pixel painting).
        // Build BitmapRepaintState from current selection/hover.
        {
            let hovered = self
                .ws
                .ui_root
                .viewport
                .hovered_clip_id()
                .map(|s| s.to_string());
            let sel_region = self.ws.ui_root.viewport.selection_region_ref().cloned();
            let has_region = sel_region.is_some();
            let insert_cursor_beat = self.ws.ui_root.viewport.insert_cursor_beat().as_f32();
            let insert_layer = self
                .selection
                .insert_cursor_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
            let has_insert = self.selection.has_insert_cursor();
            let ppb = self.ws.ui_root.viewport.pixels_per_beat();
            let sel_ver = self.selection.selection_version;

            let marker_lines = self.ws.ui_root.viewport.marker_line_data();
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
                markers: &marker_lines,
            };
            self.ws.ui_root.viewport.repaint_dirty_layers(&state);
        }

        // 6c. Upload dirty layer textures to GPU
        if let (Some(gpu), Some(bitmap_gpu)) = (&self.gpu, &mut self.layer_bitmap_gpu) {
            for (layer_idx, pixels, tw, th) in self.ws.ui_root.viewport.dirty_layer_iter() {
                bitmap_gpu.upload_layer(&gpu.device, layer_idx, pixels, tw as u32, th as u32);
            }

            // 6d. Repaint + upload waveform lane if dirty
            let wf_rect = self.ws.ui_root.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                let wf = &mut self.ws.ui_root.waveform_lane;
                // Force dirty on resize
                if wf.buffer_width != wf_rect.width as usize {
                    wf.dirty = true;
                }
                if wf.dirty {
                    wf.repaint(wf_rect.width as usize);
                    // Upload after repaint
                    if wf.buffer_width > 0 && wf.buffer_height > 0 && !wf.pixel_buffer.is_empty() {
                        bitmap_gpu.upload_layer(
                            &gpu.device,
                            1000,
                            &wf.pixel_buffer,
                            wf.buffer_width as u32,
                            wf.buffer_height as u32,
                        );
                    }
                }
            }

            // 6e. Repaint + upload stem lanes if dirty
            if self.ws.ui_root.stem_lanes.is_expanded() {
                let sl_rect = self.ws.ui_root.viewport.stem_lanes_rect();
                if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                    let sl = &mut self.ws.ui_root.stem_lanes;
                    let mapper = self.ws.ui_root.viewport.mapper();
                    if sl.buffer_width != sl_rect.width as usize {
                        sl.dirty = true;
                    }
                    if sl.dirty {
                        sl.repaint(sl_rect.width as usize, mapper);
                        if sl.buffer_width > 0
                            && sl.buffer_height > 0
                            && !sl.pixel_buffer.is_empty()
                        {
                            bitmap_gpu.upload_layer(
                                &gpu.device,
                                1001,
                                &sl.pixel_buffer,
                                sl.buffer_width as u32,
                                sl.buffer_height as u32,
                            );
                        }
                    }
                }
            }

            // 6f. Repaint + upload overview strip bitmap
            self.ws.ui_root.viewport.repaint_overview();
            if let Some((pixels, tw, th)) = self.ws.ui_root.viewport.overview_bitmap() {
                bitmap_gpu.upload_layer(&gpu.device, 1002, pixels, tw as u32, th as u32);
            }

            // 6g. Repaint + upload collapsed group bitmaps
            self.ws.ui_root.viewport.repaint_collapsed_groups();
            for (track_idx, pixels, tw, th) in
                self.ws.ui_root.viewport.dirty_collapsed_group_iter()
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
            let panel_end = self.ws.ui_root.perf_hud.first_node();
            if self.ws.ui_root.tree.has_dirty_in_range(0, panel_end) {
                self.ws.offscreen_dirty = true;
            }
            self.present_all_windows(front);
            self.present_graph_editor_window();
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.present_all_windows(0);
            self.present_graph_editor_window();
        }

        self.frame_count += 1;
    }

    /// Render and present one frame to the graph editor window.
    ///
    /// Renders to the editor's offscreen via `UIRenderer` (clear + a
    /// centered "Graph Editor" placeholder label) and blits the result
    /// to the drawable. Phase 4 replaces the placeholder with a real
    /// `GraphCanvasPanel`.
    ///
    /// Gated on the editor's own CVDisplayLink: when it hasn't fired,
    /// we skip the present to avoid wasting a drawable slot.
    fn present_graph_editor_window(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let Some(wid) = self.graph_editor_window_id else {
            return;
        };
        let Some(ws) = self.graph_editor.as_mut() else {
            return;
        };

        // Consume editor vsync signal — skip when no pulse fired.
        // (Falls through to render when there's no display link, e.g.
        // non-macOS.)
        #[cfg(target_os = "macos")]
        {
            let pulse = ws
                .ui_display_link
                .as_ref()
                .is_none_or(|dl| dl.vsync_ready());
            if !pulse {
                return;
            }
        }

        let Some(win_state) = self.window_registry.get(&wid) else {
            return;
        };
        let Some(surface) = win_state.surface.as_ref() else {
            return;
        };
        let scale = win_state.window.scale_factor();
        let (surface_w, surface_h) = (surface.width, surface.height);

        let Some(offscreen) = ws.ui_offscreen.as_ref() else {
            return;
        };
        // Surface/offscreen size mismatch: a resize is in flight. Skip
        // until the matching `resize_graph_editor_offscreen()` lands.
        if offscreen.width != surface_w || offscreen.height != surface_h {
            return;
        }

        let logical_w = (surface_w as f64 / scale).max(1.0) as u32;
        let logical_h = (surface_h as f64 / scale).max(1.0) as u32;

        // ── Build frame: clear, then draw the canvas ──
        let mut encoder = gpu.device.create_encoder("Graph Editor Frame");
        encoder.clear_texture(offscreen, 0.10, 0.10, 0.12, 1.0);

        if let (Some(ui), Some(canvas)) = (&mut self.ui_renderer, self.graph_canvas.as_ref()) {
            ui.begin_frame();
            canvas.render(
                ui,
                crate::graph_canvas::Rect::new(0.0, 0.0, logical_w as f32, logical_h as f32),
            );
            if ui.prepare(&gpu.device, logical_w, logical_h, scale) {
                ui.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
            }
        }

        encoder.commit();
        ws.offscreen_dirty = false;

        // Skip drawable acquisition on the resize frame — drawable pool
        // may still be reconfiguring.
        if ws.surface_resized_this_frame {
            ws.surface_resized_this_frame = false;
            return;
        }

        // ── Late drawable acquisition + blit ──
        let Some(drawable) = surface.next_drawable() else {
            return;
        };
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        let (Some(blit_p), Some(blit_s)) = (&self.blit_pipeline, &self.blit_sampler) else {
            return;
        };

        let mut present_enc = gpu.device.create_encoder("Graph Editor Present");
        present_enc.draw_fullscreen(
            blit_p,
            &drawable_tex,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: offscreen,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler: blit_s,
                },
            ],
            false,
            true,
            "Editor Offscreen → Drawable",
        );
        present_enc.present_drawable(&drawable);
        present_enc.commit();
    }

    fn present_all_windows(&mut self, front_index: usize) {
        let Some(gpu) = &self.gpu else { return };

        // ── Panel cache update ──
        let scale = self.scale_factor;
        let panel_infos = self.ws.ui_root.panel_cache_info();
        if let (Some(cm), Some(ui)) = (&mut self.ui_cache_manager, &mut self.ui_renderer) {
            // Compute logical surface dimensions
            let (surface_w, surface_h) = self
                .primary_window_id
                .and_then(|id| self.window_registry.get(&id))
                .and_then(|ws| ws.surface.as_ref())
                .map(|s| (s.width, s.height))
                .unwrap_or((1, 1));
            let logical_w = (surface_w as f64 / scale) as u32;
            let logical_h = (surface_h as f64 / scale) as u32;
            cm.set_scale_factor(scale);
            cm.ensure_atlas(&gpu.device, logical_w, logical_h);
            let (_, rendered_ranges) =
                cm.render_dirty_panels(&gpu.device, ui, &self.ws.ui_root.tree, &panel_infos);
            // Clear dirty flags only for ranges that were actually rendered.
            // Deferred panels keep their dirty flags for the next frame.
            for (start, end) in &rendered_ranges {
                self.ws.ui_root.tree.clear_dirty_range(*start, *end);
            }
        }

        // ── Render target: offscreen texture ──
        // All passes render to an offscreen texture. The drawable is acquired
        // late (just before present) to minimize time blocking on WindowServer
        // IPC during Direct Display synchronization on external monitors.
        let Some(window_id) = self.primary_window_id else {
            return;
        };
        let surface_dims = self
            .window_registry
            .get(&window_id)
            .and_then(|ws| ws.surface.as_ref())
            .map(|s| (s.width, s.height))
            .unwrap_or((1, 1));
        let (surface_w, surface_h) = surface_dims;

        let Some(offscreen) = &self.ws.ui_offscreen else {
            return;
        };
        // Ensure offscreen matches surface (may be stale after resize race).
        if offscreen.width != surface_w || offscreen.height != surface_h {
            return;
        }

        let logical_w = (surface_w as f64 / scale) as u32;
        let logical_h = (surface_h as f64 / scale) as u32;
        let sf = scale as f32;

        // ── Fast path: nothing visual changed — re-blit cached offscreen.
        // Must still present every callback to maintain consistent cadence.
        // ProMotion adapts refresh rate based on observed frame delivery;
        // skipping presents causes it to drop from 120Hz to 60Hz, producing
        // an 8/16ms nextDrawable bounce when it oscillates back.
        if !self.ws.offscreen_dirty {
            if self.ws.surface_resized_this_frame {
                self.ws.surface_resized_this_frame = false;
                return;
            }
            let drawable = {
                let ws = match self.window_registry.get_mut(&window_id) {
                    Some(ws) => ws,
                    None => return,
                };
                let surface = match ws.surface.as_ref() {
                    Some(s) => s,
                    None => return,
                };
                match surface.next_drawable() {
                    Some(d) => d,
                    None => return,
                }
            };
            let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
            if let (Some(blit_p), Some(blit_s)) = (&self.blit_pipeline, &self.blit_sampler) {
                let mut enc = gpu.device.create_encoder("Re-present");
                enc.draw_fullscreen(
                    blit_p,
                    &drawable_tex,
                    &[
                        manifold_gpu::GpuBinding::Texture {
                            binding: 0,
                            texture: offscreen,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 1,
                            sampler: blit_s,
                        },
                    ],
                    false,
                    true,
                    "Offscreen → Drawable",
                );
                enc.present_drawable(&drawable);
                enc.commit();
            }
            return;
        }
        self.ws.offscreen_dirty = false;

        // Reset overlay TextRenderer pool index
        if let Some(ui) = &mut self.ui_renderer {
            ui.begin_frame();
        }

        // ── Build the frame ──
        let mut encoder = gpu.device.create_encoder("Frame");

        // Pass 1: Clear to black
        encoder.clear_texture(offscreen, 0.0, 0.0, 0.0, 1.0);

        // Pass 2: Atlas blit fullscreen (UI panels onto black)
        let atlas_opt = self
            .ui_cache_manager
            .as_ref()
            .and_then(|cm| cm.atlas_texture());
        if let (Some(atlas_pipeline), Some(atlas_sampler), Some(atlas)) = (
            &self.atlas_pipeline,
            &self.atlas_sampler,
            atlas_opt.as_ref(),
        ) {
            encoder.draw_fullscreen(
                atlas_pipeline,
                offscreen,
                &[
                    manifold_gpu::GpuBinding::Texture {
                        binding: 0,
                        texture: atlas,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 1,
                        sampler: atlas_sampler,
                    },
                ],
                false,
                true,
                "Atlas Blit",
            );
        }

        // Pass 3: Blit compositor output ON TOP of atlas in video area (aspect-fit)
        // Compositor replaces whatever is in the video rect (opaque, no blend).
        #[cfg(target_os = "macos")]
        if let (Some(compositor_tex), Some(blit_pipeline), Some(blit_sampler)) = (
            self.ui_preview_textures[front_index].as_ref(),
            &self.blit_pipeline,
            &self.blit_sampler,
        ) {
            let (comp_w, comp_h) = self
                .content_pipeline_output
                .as_ref()
                .map(|p| p.get_dimensions())
                .unwrap_or((1920, 1080));
            let source_aspect = comp_w as f32 / comp_h as f32;
            let video_rect = self.ws.ui_root.layout.video_area();
            let rect_x = video_rect.x * sf;
            let rect_y = video_rect.y * sf;
            let rect_w = video_rect.width * sf;
            let rect_h = video_rect.height * sf;

            if rect_w > 0.0 && rect_h > 0.0 && source_aspect > 0.0 {
                let rect_aspect = rect_w / rect_h;
                let (fit_w, fit_h) = if source_aspect > rect_aspect {
                    (rect_w, rect_w / source_aspect)
                } else {
                    (rect_h * source_aspect, rect_h)
                };
                let fit_x = rect_x + (rect_w - fit_w) * 0.5;
                let fit_y = rect_y + (rect_h - fit_h) * 0.5;

                encoder.draw_fullscreen_viewport(
                    blit_pipeline,
                    offscreen,
                    &[
                        manifold_gpu::GpuBinding::Texture {
                            binding: 0,
                            texture: compositor_tex,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 1,
                            sampler: blit_sampler,
                        },
                    ],
                    (fit_x, fit_y, fit_w, fit_h),
                    manifold_gpu::GpuLoadAction::Load,
                    "Blit Compositor",
                );
            }
        }

        // Pass 4: Layer bitmaps directly to drawable
        if let Some(bitmap_gpu) = &mut self.layer_bitmap_gpu {
            let mut rects = self.ws.ui_root.viewport.layer_bitmap_rects();

            let wf_rect = self.ws.ui_root.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                rects.push((1000, wf_rect));
            }

            if self.ws.ui_root.stem_lanes.is_expanded() {
                let sl_rect = self.ws.ui_root.viewport.stem_lanes_rect();
                if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                    rects.push((1001, sl_rect));
                }
            }

            let ov_rect = self.ws.ui_root.viewport.overview_rect();
            if ov_rect.width > 0.0 && ov_rect.height > 0.0 {
                rects.push((1002, ov_rect));
            }

            // Collapsed group bitmaps
            rects.extend(self.ws.ui_root.viewport.collapsed_group_rects());

            if !rects.is_empty() {
                bitmap_gpu.render_layers(
                    &gpu.device,
                    &mut encoder,
                    offscreen,
                    logical_w,
                    logical_h,
                    &rects,
                );
            }
        }

        // Pass 5: Overlay UI (playhead, HUD, dropdowns, text)
        if let Some(ui) = &mut self.ui_renderer {
            // Waveform/stem lane buttons
            let wf_first = self.ws.ui_root.waveform_lane.first_node();
            let sl_first = self.ws.ui_root.stem_lanes.first_node();
            let overlay_end = self.ws.ui_root.perf_hud.first_node();
            let overlay_start = if wf_first != usize::MAX {
                Some(wf_first)
            } else if sl_first != usize::MAX {
                Some(sl_first)
            } else {
                None
            };
            if let Some(start) = overlay_start {
                ui.render_overlay_range(&self.ws.ui_root.tree, start, overlay_end);
            }

            // Playhead line
            if let Some(px) = self.ws.ui_root.viewport.playhead_pixel() {
                let ruler = self.ws.ui_root.viewport.ruler_rect();
                let tr = self.ws.ui_root.viewport.get_tracks_rect();
                let top = ruler.y;
                let height = (tr.y + tr.height) - top;
                ui.draw_rect(
                    px - 1.0,
                    top,
                    manifold_ui::color::PLAYHEAD_WIDTH,
                    height,
                    manifold_ui::color::PLAYHEAD_RED.to_f32(),
                );
            }

            // Perf HUD
            if self.ws.ui_root.perf_hud.is_visible() {
                let hud_start = self.ws.ui_root.perf_hud.first_node();
                let hud_end = if self.ws.ui_root.dropdown.is_open() {
                    self.ws.ui_root.dropdown.first_node()
                } else if self.ws.ui_root.browser_popup.is_open() {
                    self.ws.ui_root.browser_popup.first_node()
                } else if self.ws.ui_root.ableton_picker.is_open() {
                    self.ws.ui_root.ableton_picker.first_node()
                } else {
                    usize::MAX
                };
                ui.render_overlay_range(&self.ws.ui_root.tree, hud_start, hud_end);
            }

            // Popups
            if self.ws.ui_root.dropdown.is_open() {
                let start = self.ws.ui_root.dropdown.first_node();
                ui.render_overlay(&self.ws.ui_root.tree, start);
            } else if self.ws.ui_root.browser_popup.is_open() {
                let start = self.ws.ui_root.browser_popup.first_node();
                ui.render_overlay(&self.ws.ui_root.tree, start);
            } else if self.ws.ui_root.ableton_picker.is_open() {
                let start = self.ws.ui_root.ableton_picker.first_node();
                ui.render_overlay(&self.ws.ui_root.tree, start);
            }

            // Effect card drag ghost
            if let Some(start) = self.ws.ui_root.inspector.card_drag_first_node() {
                ui.render_overlay(&self.ws.ui_root.tree, start);
            }

            // Text input overlay
            if self.text_input.active {
                render_text_input_overlay(&self.text_input, &self.frame_timer, ui);
            }

            // Flush all overlay commands
            if ui.prepare(&gpu.device, logical_w, logical_h, scale) {
                ui.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
            }
        }

        // ── Commit offscreen render ──
        encoder.commit();

        // Clear ALL remaining dirty flags. render_dirty_panels only clears
        // panel cache ranges — overlay nodes (HUD, playhead, popups) live
        // outside those ranges and their DIRTY flags were never cleared,
        // keeping has_dirty permanently true and defeating the fast path.
        self.ws.ui_root.tree.clear_dirty();

        // ── Late drawable acquisition ──
        // Acquire the drawable as late as possible to minimize time blocking on
        // WindowServer IPC. All GPU work is already committed to the offscreen
        // texture above — this is just a single fullscreen blit.
        //
        // Skip entirely on resize frames: set_drawable_size reconfigures the
        // drawable pool, and nextDrawable can block up to 1s during the
        // reconfiguration. The offscreen render is still committed above —
        // it just won't be blitted to screen this frame.
        if self.ws.surface_resized_this_frame {
            self.ws.surface_resized_this_frame = false;
            return;
        }
        let drawable = {
            let ws = match self.window_registry.get_mut(&window_id) {
                Some(ws) => ws,
                None => return,
            };
            let surface = match ws.surface.as_ref() {
                Some(s) => s,
                None => return,
            };
            match surface.next_drawable() {
                Some(d) => d,
                None => {
                    log::warn!("No drawable available — skipping frame");
                    return;
                }
            }
        };

        // ── Blit offscreen → drawable + present ──
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        let blit_pipeline = match &self.blit_pipeline {
            Some(p) => p,
            None => return,
        };
        let blit_sampler = match &self.blit_sampler {
            Some(s) => s,
            None => return,
        };

        let mut present_enc = gpu.device.create_encoder("Present");
        present_enc.draw_fullscreen(
            blit_pipeline,
            &drawable_tex,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: offscreen,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler: blit_sampler,
                },
            ],
            false,
            true, // store: must write to drawable for present
            "Offscreen → Drawable",
        );
        present_enc.present_drawable(&drawable);
        present_enc.commit();
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
    let line_h = fs + 3.0; // line height with leading

    let bg_x = a.x;
    let bg_y = a.y;
    let bg_w = a.width.max(40.0);

    // For multiline fields, compute height from line count (minimum 3 lines).
    let line_count = if ti.multiline {
        ti.text.split('\n').count().max(3)
    } else {
        1
    };
    let bg_h = (line_count as f32 * line_h + pad_v * 2.0).max(a.height.max(fs + pad_v * 2.0));

    ui.draw_bordered_rect(
        bg_x,
        bg_y,
        bg_w,
        bg_h,
        TEXT_INPUT_BG,
        3.0,
        1.0,
        [0.35, 0.45, 0.7, 0.8],
    );

    // Selection highlight (when select_all)
    if ti.select_all && !ti.text.is_empty() {
        let text_w = ui
            .measure_text_cached(&ti.text, fs as u16, FontWeight::Medium)
            .x;
        ui.draw_rect(
            bg_x + pad_h,
            bg_y + pad_v,
            text_w.min(bg_w - pad_h * 2.0),
            line_h,
            TEXT_INPUT_SELECT_BG,
        );
    }

    let text_x = bg_x + pad_h;

    if ti.multiline {
        // Draw each line separately.
        for (i, line) in ti.text.split('\n').enumerate() {
            let ly = bg_y + pad_v + i as f32 * line_h;
            ui.draw_text(text_x, ly, line, fs, TEXT_INPUT_FG);
        }

        // Blinking cursor — find which line the cursor is on.
        if !ti.select_all {
            let elapsed = timer.realtime_since_start();
            let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
            if blink_on {
                let before = &ti.text[..ti.cursor];
                let cursor_line = before.matches('\n').count();
                let line_start = before.rfind('\n').map_or(0, |p| p + 1);
                let before_on_line = &before[line_start..];
                let cursor_x = text_x
                    + ui.measure_text_cached(before_on_line, fs as u16, FontWeight::Medium)
                        .x;
                let cursor_y = bg_y + pad_v + cursor_line as f32 * line_h;
                ui.draw_rect(
                    cursor_x,
                    cursor_y,
                    TEXT_INPUT_CURSOR_W,
                    line_h,
                    TEXT_INPUT_CURSOR,
                );
            }
        }
    } else {
        // Single-line rendering (original path).
        let text_y = bg_y + pad_v;
        ui.draw_text(text_x, text_y, &ti.text, fs, TEXT_INPUT_FG);

        if !ti.select_all {
            let elapsed = timer.realtime_since_start();
            let blink_on = ((elapsed / TEXT_INPUT_BLINK_PERIOD) as u64).is_multiple_of(2);
            if blink_on {
                let before = &ti.text[..ti.cursor];
                let cursor_x = text_x
                    + ui.measure_text_cached(before, fs as u16, FontWeight::Medium)
                        .x;
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
