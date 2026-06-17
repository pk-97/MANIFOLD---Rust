//! Rendering methods for Application — extracted from app.rs.
//!
//! Contains `tick_and_render()`, `present_all_windows()`, and the text input
//! overlay rendering helper. All methods are `impl Application` blocks that
//! operate on the struct defined in app.rs.

use manifold_renderer::ui_renderer::{Depth, UIRenderer};

use manifold_ui::node::FontWeight;
use manifold_ui::panels::PanelAction;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use manifold_editing::commands::effects::BindingMappingEdit;

/// Build the reshape-edit command for the watched graph target — one
/// [`manifold_editing::commands::effects::EditParamMappingCommand`] for
/// both effects and generators. The reshape lives in the preset's
/// authoring surface (`ParamSpecDef` + `BindingDef`); the command edits the
/// instance's per-instance graph override, materializing it from `seed_def`
/// (the catalog graph, resolved renderer-side) when the instance is still on
/// the catalog default. Addresses the param by stable id.
fn build_mapping_command(
    target: &manifold_core::GraphTarget,
    param_id: &str,
    edit: manifold_editing::commands::effects::BindingMappingEdit,
    seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
) -> Box<dyn manifold_editing::command::Command + Send> {
    Box::new(
        manifold_editing::commands::effects::EditParamMappingCommand::new(
            target.clone(),
            param_id.to_string(),
            edit,
            seed_def,
        ),
    )
}

/// Drag-commit variant: the command carries the EXPLICIT pre-drag reverse
/// (captured at drag start) so undo restores the true pre-drag values, not
/// the preview-mutated ones — mirroring `ChangeEffectParamCommand`'s
/// explicit `old_value`.
fn build_mapping_command_with_reverse(
    target: &manifold_core::GraphTarget,
    param_id: &str,
    new: manifold_editing::commands::effects::BindingMappingEdit,
    reverse: manifold_editing::commands::effects::BindingMappingEdit,
    seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
) -> Box<dyn manifold_editing::command::Command + Send> {
    Box::new(
        manifold_editing::commands::effects::EditParamMappingCommand::new_with_reverse(
            target.clone(),
            param_id.to_string(),
            new,
            reverse,
            seed_def,
        ),
    )
}

/// Immutable descend into a graph def at a scope path of group ids — the def
/// analog of the snapshot's `resolve_level`. `None` if the path doesn't resolve
/// (a group id is missing or isn't a group). Empty scope is the document root.
fn descend_def_level<'a>(
    def: &'a manifold_core::effect_graph_def::EffectGraphDef,
    scope: &[u32],
) -> Option<(
    &'a [manifold_core::effect_graph_def::EffectGraphNode],
    &'a [manifold_core::effect_graph_def::EffectGraphWire],
)> {
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    for &gid in scope {
        let group = nodes.iter().find(|n| n.id == gid)?.group.as_deref()?;
        nodes = &group.nodes;
        wires = &group.wires;
    }
    Some((nodes, wires))
}

/// The current reshape for `param_id` read out of a preset graph def:
/// `(label, min, max, invert, curve, scale, offset)`. Range/curve/invert/label
/// live on the param's [`ParamSpecDef`]; scale/offset on its [`BindingDef`]
/// (identity `1.0`/`0.0` when the param has no binding). `None` when the def
/// carries no metadata or the param id isn't found.
fn full_reshape_from_def(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    param_id: &str,
) -> Option<(
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
)> {
    let meta = def.preset_metadata.as_ref()?;
    let spec = meta.params.iter().find(|p| p.id == param_id)?;
    let (scale, offset) = meta
        .bindings
        .iter()
        .find(|b| b.id == param_id)
        .map(|b| (b.scale, b.offset))
        .unwrap_or((1.0, 0.0));
    Some((
        spec.name.clone(),
        spec.min,
        spec.max,
        spec.invert,
        spec.curve,
        scale,
        offset,
    ))
}

impl Application {
    /// The mapping drawer's store target for the editor's watched graph —
    /// the [`manifold_core::GraphTarget`] the command then resolves to a
    /// `GraphHost`.
    fn mapping_target(&self) -> Option<manifold_core::GraphTarget> {
        self.watched_graph_target.clone()
    }

    /// Read the watched param's CURRENT reshape `(min, max, scale, offset)`
    /// for the drawer seed + drag change-detection. Reads the preset's
    /// authoring surface (`ParamSpecDef` range + `BindingDef` scale/offset)
    /// from whichever graph def is live: the instance's per-instance override
    /// if it has diverged, else the catalog graph (effect) / bundled def
    /// (generator). `None` if the param doesn't resolve. This is the single
    /// reshape source after `ParamMapping` was deleted.
    fn watched_reshape(&self, param_id: &str) -> Option<(f32, f32, f32, f32)> {
        let (_, min, max, _, _, scale, offset) = self.watched_full_reshape(param_id)?;
        Some((min, max, scale, offset))
    }

    /// The watched param's full reshape `(label, min, max, invert, curve,
    /// scale, offset)` — the drawer's complete seed. Reads the instance's
    /// per-instance graph override first, then falls back to the catalog
    /// (effect) / bundled (generator) graph def.
    fn watched_full_reshape(
        &self,
        param_id: &str,
    ) -> Option<(
        String,
        f32,
        f32,
        bool,
        manifold_core::macro_bank::MacroCurve,
        f32,
        f32,
    )> {
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if let Some(def) = fx.graph.as_ref()
                    && let Some(r) = full_reshape_from_def(def, param_id)
                {
                    return Some(r);
                }
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())?;
                full_reshape_from_def(view.canonical_def, param_id)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if let Some(def) = layer.generator_graph()
                    && let Some(r) = full_reshape_from_def(def, param_id)
                {
                    return Some(r);
                }
                let gp = layer.gen_params()?;
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())?;
                full_reshape_from_def(view.canonical_def, param_id)
            }
        }
    }

    /// The catalog graph def to seed the instance's per-instance graph
    /// override when a reshape edit hits an instance still on the catalog
    /// default. Effects start on the catalog default (`graph: None`), so they
    /// need the seed; a generator layer always carries its authoring
    /// `generator_graph`, so the seed is only a safety net. `None` when the
    /// instance has already diverged (no seed needed) or can't be resolved.
    fn seed_def_for(
        &self,
        target: &manifold_core::GraphTarget,
    ) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        match target {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if fx.graph.is_some() {
                    return None;
                }
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())?;
                Some(view.canonical_def.clone())
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if layer.generator_graph().is_some() {
                    return None;
                }
                let gp = layer.gen_params()?;
                manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())
                    .map(|v| v.canonical_def.clone())
            }
        }
    }

    /// The graph def the editor currently shows for the watched target — the
    /// per-instance override if it has diverged, else the catalog / bundled
    /// default. Cloned (copy is a rare authoring action). `None` when nothing is
    /// watched.
    fn watched_def_cloned(&self) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if let Some(d) = fx.graph.as_ref() {
                    Some(d.clone())
                } else {
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())
                        .map(|v| v.canonical_def.clone())
                }
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if let Some(d) = layer.generator_graph() {
                    Some(d.clone())
                } else {
                    manifold_renderer::node_graph::bundled_preset_def(
                        layer.gen_params()?.generator_type(),
                    )
                    .cloned()
                }
            }
        }
    }

    /// Resolve the canvas's current selection into copy-ready data: the selected
    /// def nodes plus the wires whose BOTH endpoints are selected (internal
    /// connectivity only). `None` when nothing is watched or selected. Backs
    /// Cmd+C (store to clipboard) and Cmd+D (duplicate immediately).
    pub(crate) fn copy_selected_graph_nodes(
        &self,
    ) -> Option<(
        Vec<manifold_core::effect_graph_def::EffectGraphNode>,
        Vec<manifold_core::effect_graph_def::EffectGraphWire>,
    )> {
        let canvas = self.graph_canvas.as_ref()?;
        let ids: std::collections::HashSet<u32> = canvas.selected_ids().into_iter().collect();
        if ids.is_empty() {
            return None;
        }
        let scope = canvas.scope_path().to_vec();
        let def = self.watched_def_cloned()?;
        let (nodes, wires) = descend_def_level(&def, &scope)?;
        let sel_nodes: Vec<_> = nodes.iter().filter(|n| ids.contains(&n.id)).cloned().collect();
        if sel_nodes.is_empty() {
            return None;
        }
        let sel_wires: Vec<_> = wires
            .iter()
            .filter(|w| ids.contains(&w.from_node) && ids.contains(&w.to_node))
            .cloned()
            .collect();
        Some((sel_nodes, sel_wires))
    }

    /// Modal Yes/No confirm shown before deleting a graph node that backs card
    /// sliders. Lists the controls the delete would remove so the choice is
    /// informed. Returns true only on Yes. Blocking native dialog — fine for an
    /// authoring-time action (same as the preset import/export dialogs); never
    /// reached during performance.
    fn confirm_remove_node_orphans(labels: &[String]) -> bool {
        let n = labels.len();
        let list = labels
            .iter()
            .map(|l| format!("  •  {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let msg = format!(
            "Deleting this node will also remove {n} card control{}:\n\n{list}\n\nThis can be undone.",
            if n == 1 { "" } else { "s" },
        );
        rfd::MessageDialog::new()
            .set_title("Remove node")
            .set_description(msg)
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show()
            == rfd::MessageDialogResult::Yes
    }

    /// Read the watched param's CURRENT (post-modulation) value — the number
    /// shown on the card slider — for the mapping popover's live dot. Reads the
    /// same `param_values` slot drivers / Ableton / envelopes write each frame,
    /// so the dot tracks live motion. `None` if the param doesn't resolve.
    fn watched_value(&self, param_id: &str) -> Option<f32> {
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                let idx = fx.param_id_to_value_index(param_id)?;
                fx.param_values.get(idx).map(|p| p.value)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let gp = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?
                    .gen_params()?;
                let def =
                    manifold_core::preset_definition_registry::try_get(gp.generator_type())?;
                let idx = *def.id_to_index.get(param_id)?;
                gp.param_values.get(idx).map(|p| p.value)
            }
        }
    }

    /// Live-drag preview: apply the partial edit to the watched param's
    /// reshape store on BOTH the local project (immediate card UI) and the
    /// content thread (smooth canvas + survives the next snapshot sync),
    /// WITHOUT recording undo — the commit records the single undo entry.
    /// Reuses the edit command's own apply logic (it picks user-binding vs
    /// note + seeds copy-on-write), so the preview can never diverge from
    /// the commit.
    fn preview_mapping(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        edit: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        build_mapping_command(target, param_id, edit.clone(), seed_def.clone())
            .execute(&mut self.local_project);
        let target = target.clone();
        let pid = param_id.to_string();
        self.send_content_cmd(ContentCommand::MutateProject(Box::new(move |p| {
            build_mapping_command(&target, &pid, edit, seed_def).execute(p);
        })));
    }

    /// Commit / single-shot: send the reshape edit as one undoable command.
    /// Self-captures the reverse (correct for single-shot — nothing mutated
    /// the store first).
    fn commit_mapping(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        edit: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        self.send_content_cmd(ContentCommand::Execute(build_mapping_command(
            target, param_id, edit, seed_def,
        )));
    }

    /// Drag commit: one undoable command carrying the explicit pre-drag
    /// reverse, so undo restores the pre-drag values rather than the
    /// preview-mutated ones.
    fn commit_mapping_with_reverse(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        self.send_content_cmd(ContentCommand::Execute(build_mapping_command_with_reverse(
            target, param_id, new, reverse, seed_def,
        )));
    }

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
            let has_mod = self
                .watched_graph_target
                .as_ref()
                .is_some_and(|target| match target {
                    manifold_core::GraphTarget::Effect(eid) => self
                        .local_project
                        .find_effect_by_id(eid)
                        .is_some_and(|fx| fx.graph.is_some()),
                    manifold_core::GraphTarget::Generator(lid) => self
                        .local_project
                        .timeline
                        .find_layer_by_id(lid)
                        .is_some_and(|(_, l)| l.generator_graph().is_some()),
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
                self.ws
                    .ui_root
                    .stem_lanes
                    .set_mute_state(i, self.content_state.stem_muted[i]);
                self.ws
                    .ui_root
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
            self.ws
                .ui_root
                .waveform_lane
                .set_stems_available(any_stem_available);

            // 1h. Push visibility/text state to UITree nodes (buttons, labels).
            self.ws.ui_root.update_waveform_stem_nodes();
        }

        // 2. Process UI events and dispatch actions
        // Keep the Add-picker's embedded-preset list current (fingerprint-gated;
        // rebuilds only when a fork/import/remove changed the set).
        self.ws
            .ui_root
            .sync_embedded_presets(&self.local_project);
        let mut actions = self.ws.ui_root.process_events();

        // Editor LEFT-LANE CARD actions are collected separately so they can be
        // dispatched against the editor's watched graph identity: they carry the
        // same PanelAction variants the inspector emits, but must resolve against
        // the edited effect/generator, not the main window's active layer.
        // Appended to `actions` after a recorded boundary so the dispatch loop
        // can tell which segment they live in.
        let mut editor_card_actions: Vec<manifold_ui::panels::PanelAction> = Vec::new();
        // The editor card's right-edge chevron requests the sideways mapping
        // drawer. It's an editor-local interaction (open a popover anchored on
        // the row), not a model edit, so it's peeled out of the dispatch stream
        // and handled below once the `graph_editor` borrow is released. Last
        // request wins within a frame.
        let mut pending_open_card_mapping: Option<manifold_core::effects::ParamId> = None;

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
                            self.text_input.begin(
                                crate::text_input::TextInputField::SearchFilter,
                                &ed.ui_root.browser_popup.current_filter,
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                            ed.offscreen_dirty = true;
                        } else if let Some(action) =
                            ed.ui_root.browser_popup.handle_click(node_id)
                        {
                            match action {
                                BrowserPopupAction::NodeSelected { type_id, graph_pos } => {
                                    // Hand off to the layer-2 spawn handler.
                                    // `graph_pos` is the palette-origin
                                    // canvas position captured at open — pass
                                    // it straight through, never recompute.
                                    actions.push(
                                        manifold_ui::panels::PanelAction::AddGraphNodeAt {
                                            type_id,
                                            graph_pos,
                                        },
                                    );
                                    self.text_input.cancel();
                                }
                                BrowserPopupAction::Dismissed => {
                                    self.text_input.cancel();
                                }
                                // Effect/Generator/Paste never arise in Node
                                // mode from the editor popup.
                                _ => {}
                            }
                            ed.offscreen_dirty = true;
                        } else if ed.ui_root.browser_popup.contains_node(node_id) {
                            // Internal click (category chip, background) —
                            // consume so it doesn't leak to the canvas.
                            ed.offscreen_dirty = true;
                        }
                    }
                }
            } else {
                use manifold_ui::input::UIEvent;
                for event in &events {
                    // Left-lane card: map editor pointer events to the card's
                    // node-id methods, exactly as the inspector composite does
                    // (Click→handle_click, PointerDown→grab, Drag→scrub,
                    // DragEnd→commit, RightClick→menu). The card ignores node_ids
                    // that aren't its own, so forwarding every event is safe.
                    // OpenGraphEditor (the card cog) is dropped here — you're
                    // already in the editor; CardContext suppresses the button in
                    // a later pass.
                    let from_card = match event {
                        UIEvent::Click { node_id, .. } => self.editor_card.handle_click(*node_id),
                        UIEvent::PointerDown { node_id, pos, .. } => {
                            self.editor_card.handle_pointer_down(*node_id, *pos)
                        }
                        UIEvent::Drag { pos, .. } => {
                            self.editor_card.handle_drag(*pos, &mut ed.ui_root.tree)
                        }
                        UIEvent::DragEnd { .. } => {
                            self.editor_card.handle_drag_end(&mut ed.ui_root.tree)
                        }
                        UIEvent::RightClick { node_id, .. } => {
                            self.editor_card.handle_right_click(*node_id)
                        }
                        _ => Vec::new(),
                    };
                    for a in from_card {
                        match a {
                            // Cog is dropped (you're already in the editor).
                            manifold_ui::panels::PanelAction::OpenGraphEditor(_) => {}
                            // Chevron → open the sideways mapping drawer; handled
                            // after this borrow scope ends.
                            manifold_ui::panels::PanelAction::OpenCardMapping(pid) => {
                                pending_open_card_mapping = Some(pid);
                            }
                            other => editor_card_actions.push(other),
                        }
                    }
                    // Forward every event into the right-sidebar inspector panel
                    // too — it wants Click (toggle/cycle) plus DragBegin/Drag/
                    // DragEnd (numeric scrub) for the per-node expose rows.
                    actions.extend(self.graph_editor_panel.handle_event(event));
                }
            }
        }
        // 2b. Drain editor-canvas actions (wire-drag completions,
        // node-drag releases, delete-key requests). Bypasses the
        // UITree event path because the canvas owns its own pointer
        // state — see `GraphCanvas::drain_actions`.
        if let Some(canvas) = self.graph_canvas.as_mut() {
            actions.extend(canvas.drain_actions());
        }

        // Editor-card chevron requested the sideways mapping drawer: resolve the
        // binding's current reshape from the edited effect OR generator, anchor
        // on the row's chevron, and open the popover. The reshape is read from
        // the instance's per-instance graph override if it has diverged, else
        // the catalog (effect) / bundled (generator) def. A `None` (e.g. a stale
        // id) just no-ops.
        if let Some(pid) = pending_open_card_mapping {
            self.open_editor_card_mapping(&pid);
        }
        // The editor mapping popover emits the same `EffectMapping*` actions the
        // canvas popover does (range / scale / offset / invert / curve), keyed by
        // binding id and dispatched against the editor's `watched_graph_target`
        // (by effect id) in the inline arms below.
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

        // Append the editor card's actions as a trailing segment, recording where
        // it starts. Actions at or past `editor_card_seg_start` were emitted by
        // the graph editor's left-lane card and dispatch against the editor's
        // watched graph identity; everything before is main-window / sidebar and
        // dispatches against the ambient inspector context.
        let editor_card_seg_start = actions.len();
        actions.extend(editor_card_actions);
        // Editor-card-segment actions dispatch against the editor's watched
        // graph identity (effect or generator), by id — not the ambient
        // inspector context. Cloned once per frame so the dispatch loop can
        // borrow it while `dispatch` mutably borrows `self`'s other fields.
        let editor_graph_target: Option<manifold_core::GraphTarget> =
            self.watched_graph_target.clone();
        // The canvas's current view depth (a path of group ids; empty = root),
        // captured once so the per-node graph edits below target the level the
        // user is actually looking at when they're inside a group.
        let canvas_scope: Vec<u32> = self
            .graph_canvas
            .as_ref()
            .map(|c| c.scope_path().to_vec())
            .unwrap_or_default();

        for (action_idx, action) in actions.iter().enumerate() {
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
                        config.audio_device = self.ws.ui_root.selected_audio_input_device.clone();
                        self.send_content_cmd(ContentCommand::StartLiveRecording(Box::new(config)));
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
                    self.ws
                        .ui_root
                        .layer_headers
                        .set_audio_device_name(&mut self.ws.ui_root.tree, &display);
                    continue;
                }
                PanelAction::ToggleMonitor => {
                    self.pending_toggle_output = true;
                    continue;
                }
                PanelAction::OpenAudioSetup => {
                    // Toggle the modal and flag a one-shot structural sync. The
                    // panel's device + send list lives behind sync_inspector_data
                    // (gated on this flag), so the sync populates it this frame,
                    // then build() rebuilds the overlay with data. While the panel
                    // stays open nothing re-fires — the overlay nodes persist and
                    // the draw pass redraws them from overlay_draw — so opening it
                    // costs one rebuild, not a per-frame one. Subsequent send /
                    // device edits each return DispatchResult::structural(), which
                    // re-runs this same path to refresh the panel.
                    self.ws.ui_root.audio_setup_panel.toggle();
                    needs_structural_sync = true;
                    continue;
                }
                PanelAction::OpenGeneratorGraphEditor => {
                    // Ask the content thread to snapshot the active
                    // layer's generator graph and set the unified
                    // watched_graph_target so every PanelAction edit
                    // handler downstream dispatches against the
                    // generator graph rather than an effect.
                    if let Some(lid) = self.active_layer_id.clone() {
                        self.send_content_cmd(ContentCommand::WatchGeneratorGraph(Some(
                            lid.clone(),
                        )));
                        self.watched_graph_target =
                            Some(manifold_core::GraphTarget::Generator(lid.clone()));
                        // Cache the catalog default — the bundled JSON
                        // for the layer's generator type — so the edit
                        // commands can lift None → default on first edit.
                        self.watched_catalog_default = self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .map(|(_, l)| l.generator_type().clone())
                            .filter(|gt| !gt.is_none())
                            .and_then(|gt| {
                                manifold_renderer::node_graph::bundled_preset_json(&gt)
                            })
                            .and_then(|json| serde_json::from_str(&json).ok());
                    }
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::OpenGraphEditor(ei) => {
                    // Resolve `ei` (effect index in the active inspector
                    // tab) to the effect's stable `EffectId`, then ask
                    // the content thread to start snapshotting that
                    // specific instance's graph. Keyed by instance id —
                    // not type id — so two cards of the same effect type
                    // can produce independent snapshots once Phase 3
                    // editing lands.
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    let effect_id = match tab {
                        manifold_ui::InspectorTab::Master => self
                            .local_project
                            .settings
                            .master_effects
                            .get(*ei)
                            .map(|e| e.id.clone()),
                        manifold_ui::InspectorTab::Layer => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .and_then(|effects| effects.get(*ei))
                            .map(|e| e.id.clone()),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .and_then(|c| c.effects.get(*ei))
                            .map(|e| e.id.clone()),
                    };
                    if let Some(eid) = effect_id.clone() {
                        self.send_content_cmd(ContentCommand::WatchEffectGraph(Some(eid)));
                    }
                    // Phase 4: capture the watched effect's id + its
                    // catalog-default graph def so the per-card editing
                    // commands (AddGraphNode, ConnectPorts, ...) can
                    // lift `instance.graph` from `None` on first edit
                    // without round-tripping through the renderer.
                    self.watched_graph_target =
                        effect_id.clone().map(manifold_core::GraphTarget::Effect);
                    self.watched_catalog_default = effect_id.as_ref().and_then(|eid| {
                        let instance = self.local_project.find_effect_by_id(eid)?;
                        manifold_renderer::node_graph::catalog_graph_def_for(
                            instance.effect_type(),
                        )
                    });
                    // `watched_graph_target` (set above to `Effect(effect_id)`)
                    // is the sole identity for the edited instance — master,
                    // layer, or clip. Every editor-card edit and the
                    // right-sidebar exposure panel resolve through it by id, so
                    // clip-scoped effects are addressed correctly with no
                    // positional fallback.
                    self.pending_open_graph_editor = true;
                    continue;
                }
                PanelAction::AddGraphNode { type_id } => {
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
                PanelAction::OpenNodePicker {
                    screen_pos,
                    graph_pos,
                } => {
                    use manifold_renderer::node_graph::{Category, descriptor_for};
                    use manifold_ui::panels::browser_popup::*;

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

                    let names: Vec<String> = self
                        .palette_atoms_cache
                        .iter()
                        .map(|a| a.label.clone())
                        .collect();
                    let type_ids: Vec<String> = self
                        .palette_atoms_cache
                        .iter()
                        .map(|a| a.type_id.clone())
                        .collect();
                    let categories: Vec<String> = self
                        .palette_atoms_cache
                        .iter()
                        .map(|a| a.category.clone())
                        .collect();
                    // Search haystack per item: the friendly label plus the
                    // descriptor's aliases (old names, plain-English, the
                    // TouchDesigner-equivalent operator). Typing "blur top"
                    // or a legacy name finds the node.
                    let search: Vec<String> = self
                        .palette_atoms_cache
                        .iter()
                        .map(|a| {
                            let aliases = descriptor_for(&a.type_id)
                                .map(|d| d.aliases.join(" "))
                                .unwrap_or_default();
                            if aliases.is_empty() {
                                a.label.clone()
                            } else {
                                format!("{} {}", a.label, aliases)
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
                            item_names: names,
                            item_categories: categories,
                            category_names: cat_names,
                            item_type_ids: type_ids,
                            item_search: Some(search),
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
                    self.text_input.begin(
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
                PanelAction::AddGraphNodeAt { type_id, graph_pos } => {
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
                PanelAction::ConnectPorts {
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
                PanelAction::RevertEffectGraph => {
                    if let Some(eid) = self.watched_graph_target.as_ref() {
                        let cmd =
                            manifold_editing::commands::graph::RevertEffectGraphCommand::new(
                                eid.clone(),
                            );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                PanelAction::DisconnectPorts { to_node, to_port } => {
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
                PanelAction::RemoveGraphNode { node_id } => {
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
                PanelAction::MoveGraphNode { node_id, new_pos } => {
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
                PanelAction::RelayoutGraph {
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
                PanelAction::SetGraphNodeParam {
                    node_id,
                    param_name,
                    new_value,
                } => {
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            eid.clone(),
                            *node_id,
                            param_name.clone(),
                            new_value.clone(),
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                PanelAction::BrowseGraphNodePath { node_id, param_name } => {
                    // Blocking native folder picker — fine for authoring (same as
                    // preset import/export). On a pick, set the param to the path
                    // through the same command SetGraphNodeParam uses.
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) && let Some(folder) = rfd::FileDialog::new().pick_folder()
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
                PanelAction::EditGraphNodeStringParam {
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
                PanelAction::EditGraphNodeWgsl {
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
                PanelAction::EditGraphNodeTableCell {
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
                PanelAction::GroupSelection {
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
                PanelAction::Ungroup {
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
                PanelAction::SetGroupTint {
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
                PanelAction::ToggleNodeParamExpose {
                    node_id,
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
                        let cmd =
                            manifold_editing::commands::graph::ToggleNodeParamExposeCommand::new(
                                target.clone(),
                                node_id.clone(),
                                node_handle.clone(),
                                inner_param.clone(),
                                *expose,
                                default.clone(),
                                label.clone(),
                                *min,
                                *max,
                                *default_value,
                                *convert,
                                *is_angle,
                                value_labels.clone(),
                            );
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                PanelAction::SetNodePreviewNormalize(on) => {
                    // Preview-only display preference — no undo, no model
                    // mutation. Update the UI mirror and tell the content
                    // thread to flip the node-preview blit.
                    self.node_preview_normalize = *on;
                    self.send_content_cmd(ContentCommand::SetNodePreviewNormalize(*on));
                    continue;
                }
                PanelAction::EffectMappingRangeSnapshot { binding_id } => {
                    // Pre-drag (min, max) so the commit can record one undo
                    // for the whole range drag. Store-aware (user binding /
                    // note / seed) and kind-aware (effect / generator).
                    self.mapping_range_snapshot =
                        self.watched_reshape(binding_id).map(|(mn, mx, _, _)| (mn, mx));
                    continue;
                }
                PanelAction::EffectMappingRangeChanged {
                    binding_id,
                    min,
                    max,
                } => {
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
                PanelAction::EffectMappingRangeCommit { binding_id } => {
                    let snap = self.mapping_range_snapshot.take();
                    if let (Some((old_min, old_max)), Some(t)) = (snap, self.mapping_target())
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
                PanelAction::EffectMappingLabel { binding_id, label } => {
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
                PanelAction::EffectMappingInvert { binding_id, invert } => {
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
                PanelAction::EffectMappingCurve { binding_id, curve } => {
                    if let Some(t) = self.mapping_target() {
                        self.commit_mapping(
                            &t,
                            binding_id,
                            BindingMappingEdit {
                                curve: Some(*curve),
                                ..Default::default()
                            },
                        );
                    }
                    continue;
                }
                PanelAction::EffectMappingAffineSnapshot { binding_id } => {
                    self.mapping_affine_snapshot =
                        self.watched_reshape(binding_id).map(|(_, _, sc, of)| (sc, of));
                    continue;
                }
                PanelAction::EffectMappingAffineChanged {
                    binding_id,
                    scale,
                    offset,
                } => {
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
                PanelAction::EffectMappingAffineCommit { binding_id } => {
                    let snap = self.mapping_affine_snapshot.take();
                    if let (Some((old_scale, old_offset)), Some(t)) = (snap, self.mapping_target())
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
                PanelAction::EffectMappingGotoNode { binding_id } => {
                    // Read-only navigation: resolve the binding's stable NodeId
                    // from the live snapshot (outer routing → node handle → id)
                    // and centre the editor canvas on it. Same path as the
                    // card-label jump-to-node, triggered from the mapping drawer.
                    if let (Some(snap), Some(canvas)) = (
                        self.content_state.active_graph_snapshot.as_deref(),
                        self.graph_canvas.as_mut(),
                    ) && let Some(node_id) =
                        crate::graph_canvas::resolve_card_param_node_id(snap, binding_id)
                    {
                        canvas.focus_node(snap, &node_id);
                    }
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
                            let items: Vec<manifold_ui::panels::dropdown::DropdownItem> = if key
                                == "fontFamily"
                            {
                                manifold_renderer::text_rasterizer::TextRasterizer::available_font_families()
                                        .into_iter()
                                        .map(|name| manifold_ui::panels::dropdown::DropdownItem::new(&name))
                                        .collect()
                            } else {
                                vec![]
                            };
                            if !items.is_empty() {
                                let trigger =
                                    manifold_ui::node::Rect::new(r.x, r.y, r.width, r.height);
                                self.ws.ui_root.open_dropdown_at(
                                    crate::ui_root::DropdownContext::GenStringParamDropdown(
                                        *sp_idx,
                                    ),
                                    items,
                                    trigger,
                                );
                            }
                        }
                    }
                    continue;
                }
                PanelAction::AudioSendLabelClicked(send_id) => {
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
            // Editor card segment → dispatch with the editor's identity;
            // main-window / sidebar actions → None (ambient, unchanged).
            let editor_target = if action_idx >= editor_card_seg_start {
                editor_graph_target.as_ref()
            } else {
                None
            };
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
                &mut self.target_snapshot,
                &mut self.decay_snapshot,
                &mut self.user_prefs,
                &mut self.active_inspector_drag,
                editor_target,
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
                self.ws
                    .ui_root
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

        // 6·audio. Live per-send level meters in the Audio Setup modal — in-place
        // node resize from the latest content-state levels, no rebuild.
        if self.ws.ui_root.audio_setup_panel.is_open() {
            let count = self.content_state.audio_send_count;
            let levels = self.content_state.audio_send_levels;
            self.ws.ui_root.update_audio_meters(&levels[..count]);
        }

        // 6·audio·scope. Push the scope's selected send to the content thread
        // (drives the worker's VQT column producer). Only on change — closing the
        // panel sends `None`, stopping column production.
        {
            let desired = if self.ws.ui_root.audio_setup_panel.is_open() {
                self.ws.ui_root.audio_setup_panel.selected_send().cloned()
            } else {
                None
            };
            if desired != self.spectrogram_send_sent {
                self.send_content_cmd(ContentCommand::SetSpectrogramSend(desired.clone()));
                self.spectrogram_send_sent = desired;
            }
        }

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
                self.ws
                    .ui_root
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

    /// Open the editor card's sideways mapping drawer for the binding named by
    /// `param_id` (its right-edge chevron was clicked). Resolves the binding's
    /// current range / scale / offset / invert / curve from the edited effect,
    /// the declared inner-param range from the live snapshot (for the trim track
    /// span), and anchors the popover on the chevron rect. Effect-only — a
    /// generator target or an unresolvable id just no-ops.
    fn open_editor_card_mapping(&mut self, param_id: &str) {
        // Resolve the param's current mapping values + the trim-track
        // range from whichever store the editor watches — an effect
        // (user-binding inline / stock note / fresh seed) or a generator
        // (note / fresh seed). The drawer edits all of them through the
        // same command. `None` → don't open.
        let resolved = match self.watched_graph_target.clone() {
            Some(manifold_core::GraphTarget::Effect(eid)) => {
                let Some(fx) = self.local_project.find_effect_by_id(&eid) else {
                    return;
                };
                if let Some(b) = fx
                    .user_param_bindings()
                    .into_iter()
                    .find(|b| b.id == param_id)
                {
                    let (node_id, inner_param) = (b.node_id.clone(), b.inner_param.clone());
                    let range = self
                        .content_state
                        .active_graph_snapshot
                        .as_deref()
                        .and_then(|snap| {
                            snap.nodes
                                .iter()
                                .find(|n| n.node_id == node_id)
                                .and_then(|n| n.parameters.iter().find(|p| p.name == inner_param))
                                .and_then(|p| p.range)
                        });
                    (
                        b.id.clone(), b.label.clone(), b.min, b.max, b.invert, b.curve, b.scale,
                        b.offset, range,
                    )
                } else {
                    // Stock / recalibrated param: read the current reshape from
                    // the instance's per-instance graph override if it has
                    // diverged, else the catalog graph. The trim track spans the
                    // preset's authored (pre-fork) range from the registry.
                    let registry_range =
                        manifold_core::preset_definition_registry::try_get(fx.effect_type())
                            .and_then(|def| {
                                def.id_to_index
                                    .get(param_id)
                                    .map(|&i| (def.param_defs[i].min, def.param_defs[i].max))
                            });
                    let from_def = fx
                        .graph
                        .as_ref()
                        .and_then(|d| full_reshape_from_def(d, param_id))
                        .or_else(|| {
                            manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())
                                .and_then(|v| full_reshape_from_def(v.canonical_def, param_id))
                        });
                    let Some((name, min, max, invert, curve, scale, offset)) = from_def else {
                        // Not a known stock param (and not a user binding).
                        return;
                    };
                    (
                        param_id.to_string(), name, min, max, invert, curve, scale, offset,
                        registry_range,
                    )
                }
            }
            Some(manifold_core::GraphTarget::Generator(lid)) => {
                let Some(layer) = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| l.layer_id == lid)
                else {
                    return;
                };
                let Some(gp) = layer.gen_params() else {
                    return;
                };
                let registry_range =
                    manifold_core::preset_definition_registry::try_get(gp.generator_type())
                        .and_then(|def| {
                            def.id_to_index
                                .get(param_id)
                                .map(|&i| (def.param_defs[i].min, def.param_defs[i].max))
                        });
                let from_def = layer
                    .generator_graph()
                    .and_then(|d| full_reshape_from_def(d, param_id))
                    .or_else(|| {
                        manifold_renderer::node_graph::bundled_preset_def(gp.generator_type())
                            .and_then(|d| full_reshape_from_def(d, param_id))
                    });
                let Some((name, min, max, invert, curve, scale, offset)) = from_def else {
                    return;
                };
                (
                    param_id.to_string(), name, min, max, invert, curve, scale, offset,
                    registry_range,
                )
            }
            None => return,
        };
        let (binding_id, label, min, max, invert, curve, scale, offset, range) = resolved;

        // Anchor on the chevron (UI-space rect → canvas-space Rect).
        let Some(ed) = self.graph_editor.as_ref() else {
            return;
        };
        let Some(anchor) = self
            .editor_card
            .mapping_chevron_rect(&ed.ui_root.tree, &binding_id)
        else {
            return;
        };
        let anchor =
            crate::graph_canvas::Rect::new(anchor.x, anchor.y, anchor.width, anchor.height);
        // Clip to the editor window's logical rect so the drawer never renders
        // past an edge.
        let clip = self
            .graph_editor_window_id
            .and_then(|wid| self.window_registry.get(&wid))
            .map(|w| {
                let s = w.window.scale_factor() as f32;
                let sz = w.window.inner_size();
                crate::graph_canvas::Rect::new(
                    0.0,
                    0.0,
                    (sz.width as f32 / s).max(1.0),
                    (sz.height as f32 / s).max(1.0),
                )
            })
            .unwrap_or_else(|| crate::graph_canvas::Rect::new(0.0, 0.0, 1280.0, 800.0));

        self.editor_mapping_popover.open(
            binding_id.to_string(), label, min, max, invert, curve, scale, offset, range, anchor,
            clip,
        );
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.offscreen_dirty = true;
        }
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
    /// The graph-editor canvas viewport rect in logical pixels — the same
    /// region `canvas.render` draws into. `None` when the editor window is
    /// closed or its surface isn't ready. Used to anchor overlays (the group
    /// rename field) in screen space from the key handler, outside the present
    /// pass where `canvas_x`/`canvas_width` are computed inline.
    pub(crate) fn editor_canvas_viewport(&self) -> Option<crate::graph_canvas::Rect> {
        let wid = self.graph_editor_window_id?;
        let win_state = self.window_registry.get(&wid)?;
        let surface = win_state.surface.as_ref()?;
        let scale = win_state.window.scale_factor();
        let logical_w = (surface.width as f64 / scale).max(1.0) as f32;
        let logical_h = (surface.height as f64 / scale).max(1.0) as f32;
        let palette_width = manifold_ui::panels::graph_editor::EDITOR_CARD_LANE_WIDTH;
        let sidebar_width = manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
        let canvas_x = palette_width;
        let canvas_width = (logical_w - palette_width - sidebar_width).max(0.0);
        Some(crate::graph_canvas::Rect::new(
            canvas_x, 0.0, canvas_width, logical_h,
        ))
    }

    fn present_graph_editor_window(&mut self) {
        // Forward the editor's single-node selection to the content thread so
        // it can capture that node's output for the preview pane. Deduplicated
        // against the last send. A closed editor (`graph_canvas == None`)
        // yields `None`, clearing the preview.
        // Resolve the single selection to a preview target. A boundary node
        // (Group Input/Output, final output) has no runtime instance of its
        // own, so resolve it against the hierarchical snapshot at the canvas
        // scope to the concrete node whose texture stands in for that boundary.
        let preview_node = match (
            self.graph_canvas.as_ref(),
            self.content_state.active_graph_snapshot.as_deref(),
        ) {
            (Some(canvas), Some(snap)) => canvas
                .selected_node_id()
                .and_then(|id| resolve_preview_target(snap, canvas.scope_path(), id)),
            _ => None,
        };
        if preview_node != self.last_preview_node {
            if let Some(tx) = self.content_tx.as_ref() {
                crate::content_command::ContentCommand::send(
                    tx,
                    crate::content_command::ContentCommand::SetGraphPreviewNode(
                        preview_node.clone(),
                    ),
                );
            }
            self.last_preview_node = preview_node;
        }

        // Send the canvas's currently-visible nodes (deduped) so the content
        // thread captures thumbnails only for what's on screen — hidden /
        // off-scope / collapsed-group nodes cost nothing. The set is the whole
        // current scope level (what the atlas shows), so it changes only on a
        // scope descend/ascend or a topology edit, not on pan/zoom.
        let visible_nodes: Vec<manifold_core::NodeId> = match (
            self.graph_canvas.as_ref(),
            self.content_state.active_graph_snapshot.as_deref(),
        ) {
            (Some(canvas), Some(snap)) => {
                let (nodes, _) =
                    crate::graph_canvas::resolve_level(snap, canvas.scope_path())
                        .unwrap_or((&snap.nodes, &snap.wires));
                nodes
                    .iter()
                    .filter_map(crate::graph_canvas::node_preview_target)
                    .collect()
            }
            _ => Vec::new(),
        };
        if visible_nodes != self.last_atlas_visible_sent {
            self.send_content_cmd(
                crate::content_command::ContentCommand::SetNodeAtlasVisible(visible_nodes.clone()),
            );
            self.last_atlas_visible_sent = visible_nodes;
        }

        let Some(gpu) = &self.gpu else { return };
        let Some(wid) = self.graph_editor_window_id else {
            return;
        };
        // Resolve the open popover's live value before borrowing the editor
        // window state mutably (`watched_value` borrows all of `self`).
        let popover_live_value = if self.editor_mapping_popover.is_open() {
            self.watched_value(self.editor_mapping_popover.binding_id())
        } else {
            None
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

        // ── Editor window layout ──────────────────────────────────────
        // Left palette (atoms) + center canvas + right sidebar (param
        // expose). Built BEFORE rendering so the tree's nodes (panels +
        // buttons + labels) are ready to draw alongside the canvas.
        // Left lane renders the real effect/generator card now (the node
        // palette moved to the spawn popup). Width comes from the single
        // EDITOR_CARD_LANE_WIDTH constant the canvas input path also reads, so
        // the canvas origin and click hit-testing stay in lockstep.
        let palette_width = manifold_ui::panels::graph_editor::EDITOR_CARD_LANE_WIDTH;
        let sidebar_width = manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
        let canvas_x = palette_width;
        let canvas_width = (logical_w as f32 - palette_width - sidebar_width).max(0.0);
        let sidebar_x = canvas_x + canvas_width;
        // When a node is being previewed, the preview pane occupies the top of
        // the sidebar; the expose/param rows start below it so they don't
        // overlap. Logical units; the present pass draws the pane to match.
        let preview_pad = 8.0_f32;
        let preview_title_h = 18.0_f32;
        let preview_w = (sidebar_width - 2.0 * preview_pad).max(1.0);
        let preview_h = preview_w * 9.0 / 16.0;
        // The right column is monitors-only now — the inner-node param list moved
        // under the left card. Two equal stacked 16:9 monitors (the selected
        // node's output on top, the master compositor output below), each a
        // titled pane (title row + body). The pair is centred vertically in the
        // column so it reads as intentional rather than top-pinned with a void
        // beneath. The node pane's body shows the node's image, or — for a
        // control / math / envelope node with no image — its value inspector text
        // in place of the image, or a placeholder when nothing is selected. The
        // master pane always shows what the live show is putting out.
        let node_preview_info = self.content_state.node_preview_info.clone();
        let preview_has_image = node_preview_info
            .as_ref()
            .map(|i| i.has_image)
            .unwrap_or(false);
        let show_image = self.last_preview_node.is_some() && preview_has_image;
        let pane_block_h = 2.0 * (preview_title_h + preview_h) + preview_pad;
        let mut pane_y = (((logical_h as f32) - pane_block_h) * 0.5).max(preview_pad);
        // Node-output monitor: title row + 16:9 body.
        let node_title_y = pane_y;
        let node_img_y = node_title_y + preview_title_h;
        pane_y = node_img_y + preview_h + preview_pad;
        // Master-out monitor: just below the node pane.
        let master_title_y = pane_y;
        let master_img_y = master_title_y + preview_title_h;
        let palette_viewport =
            manifold_ui::Rect::new(0.0, 0.0, palette_width, logical_h as f32);

        // Resolve which `PresetInstance` is being edited and build the
        // panel inputs. An open editor without a resolvable
        // `watched_graph_target` is a degenerate state — show the panel's
        // empty placeholder.
        let snap_arc = self.content_state.active_graph_snapshot.as_ref().cloned();
        let (selected_node_u32, panel_scope) = self
            .graph_canvas
            .as_ref()
            .map(|c| (c.selected_node_id(), c.scope_path().to_vec()))
            .unwrap_or((None, Vec::new()));
        let view_for_panel =
            build_graph_editor_view(selected_node_u32, snap_arc.as_deref(), &panel_scope);
        // V2 unification: the right-sidebar's top "Effect Parameters"
        // list is a read-only summary of every inner-node param
        // currently exposed on the effect card (static-block + user-
        // bindings, merged); the per-node section's checkbox is the
        // single toggle entry point.
        //
        // `static_block_targets` is computed once and reused by both
        // `build_card_exposures` (for the per-node checked state) and
        // by the panel itself (for routing the click to the right
        // command — `EffectStaticParamExpose` vs `EffectParamExpose`).
        let static_block_targets = build_static_block_targets(
            self.watched_graph_target.as_ref(),
            snap_arc.as_deref(),
            &self.local_project,
        );
        let exposed_keys = build_card_exposures(snap_arc.as_deref());
        // Outer→inner routings declared by the effect (Mirror's
        // `Amount` → `Mix.amount`, `Mode` → `Transform.mode`, etc.).
        // The panel uses this to disable inner-param rows the outer
        // card slider drives every frame.
        let outer_driven = build_outer_driven_map(snap_arc.as_deref());
        // Wire-driven set: (handle, inner_param) for every inner param
        // shadowed by an incoming wire on the same-named scalar input
        // port. The panel disables the checkbox + value cell for these
        // rows; clicks on either short-circuit to no-op.
        let wire_driven_keys = build_wire_driven_keys(snap_arc.as_deref());
        // The editor targets its effect by identity (`watched_graph_target`),
        // never by index. This panel field is vestigial — stored, never read —
        // so the positional index is gone now that targeting is id-based.
        let effect_index: Option<usize> = None;
        self.graph_editor_panel.configure(
            effect_index,
            view_for_panel.as_ref(),
            exposed_keys,
            outer_driven,
            static_block_targets,
            wire_driven_keys,
        );
        self.graph_editor_panel
            .set_node_preview_normalize(self.node_preview_normalize);
        // Value inspector for a previewed node with no image: its description
        // (from the descriptor) + the live scalar I/O captured this frame.
        let node_inspector = node_preview_info
            .as_ref()
            .filter(|i| !i.has_image)
            .map(|info| {
                let snap_node = snap_arc
                    .as_deref()
                    .and_then(|s| find_snapshot_node(&s.nodes, &info.node_id));
                let title = snap_node
                    .map(|n| n.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| info.node_id.to_string());
                let description = snap_node
                    .and_then(|n| {
                        manifold_renderer::node_graph::descriptor_for(&n.type_id)
                    })
                    .map(|d| {
                        if !d.summary.is_empty() {
                            d.summary.to_string()
                        } else {
                            // First sentence of the technical purpose keeps it short.
                            d.purpose.split(". ").next().unwrap_or(d.purpose).to_string()
                        }
                    })
                    .unwrap_or_default();
                manifold_ui::panels::graph_editor::NodeInspector {
                    title,
                    description,
                    inputs: info.inputs.clone(),
                    outputs: info.outputs.clone(),
                }
            });
        self.graph_editor_panel.set_node_inspector(node_inspector);

        // The left lane renders the REAL effect/generator card for the edited
        // target — the same `ParamCardPanel` the inspector shows, configured
        // from the same `PresetInstance` / `PresetInstance`, resolved by
        // identity from `watched_graph_target` (effect id or generator layer).
        // Resolved once per editor frame; `None` (degenerate open state with
        // no resolvable target) leaves the lane empty.
        let editor_card_data = crate::ui_bridge::editor_card_config(
            &self.local_project,
            self.watched_graph_target.as_ref(),
            &self.selection,
        );

        // Rebuild the editor's UITree from scratch each frame: tree state
        // is small, so a clear + rebuild is cheaper than dirty-tracking and
        // means stale rows can never linger after the target changes.
        ws.ui_root.tree.clear();
        // Left lane = the card, then the inner-node param list docked right under
        // it. Track the card's built height so the panel knows where to start.
        let mut card_h = 0.0_f32;
        if let Some((config, values)) = editor_card_data.as_ref() {
            // Gate `configure` on a structural/mod-state change (same discipline
            // as the inspector's gated `sync_inspector_data`): reconfiguring
            // resets the card's transient UI state — open drawers, in-progress
            // drags — so doing it every frame would make the card uninteractive.
            // Param VALUES are not in the config (they ride `values`), so a drag
            // leaves this hash unchanged. Hash the config's Debug form: cheap for
            // one card, and captures every field `configure` consumes without
            // needing PartialEq across the UI config types.
            let config_hash = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                format!("{config:?}").hash(&mut hasher);
                hasher.finish()
            };
            if self.editor_card_config_hash != Some(config_hash) {
                self.editor_card.configure(config);
                self.editor_card_config_hash = Some(config_hash);
            }
            self.editor_card.build(&mut ws.ui_root.tree, palette_viewport);
            self.editor_card.sync_values(&mut ws.ui_root.tree, values);
            card_h = self.editor_card.compute_height();
            // Close the drawer only when the row it anchored on is actually gone
            // (target changed, param unexposed) — NOT on any config change. The
            // drawer edits the note live, and the note's min/max now flow into
            // the card config (so the slider range tracks the drawer), so a
            // range-handle drag changes the config every frame. Closing on that
            // dismissed the panel mid-drag. The chevron still resolving means
            // the row is still there, so keep the drawer open.
            if self.editor_mapping_popover.is_open()
                && self
                    .editor_card
                    .mapping_chevron_rect(
                        &ws.ui_root.tree,
                        self.editor_mapping_popover.binding_id(),
                    )
                    .is_none()
            {
                self.editor_mapping_popover.close();
            }
        } else {
            self.editor_card_config_hash = None;
            self.editor_mapping_popover.close();
        }
        // Inner-node param list: docked under the card in the LEFT lane now (the
        // right column is monitors-only). A thin divider sets it off from the card.
        let panel_top = if card_h > 0.0 { card_h + 5.0 } else { 0.0 };
        if card_h > 0.0 {
            ws.ui_root.tree.add_panel(
                -1,
                0.0,
                card_h + 2.0,
                palette_width,
                1.0,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::DIVIDER_COLOR,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
        let panel_viewport = manifold_ui::Rect::new(
            0.0,
            panel_top,
            palette_width,
            (logical_h as f32 - panel_top).max(0.0),
        );
        self.graph_editor_panel
            .build(&mut ws.ui_root.tree, panel_viewport);

        // Pinned preview monitors in the right column: a backing panel, the two pane
        // titles, and — for a non-image node — the value inspector text in the
        // node pane. Added to the editor tree so they composite into the
        // offscreen; the monitor images blit onto the drawable below each title
        // in the present pass.
        {
            use manifold_ui::node::{TextAlign, UIStyle};
            let title_style = UIStyle {
                text_color: manifold_ui::color::TEXT_WHITE_C32,
                font_size: 14,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            };
            let title_x = sidebar_x + preview_pad;
            // Backing panel for the whole right column so the two centred monitors
            // sit on a clean, uniform surface rather than a black void.
            ws.ui_root.tree.add_panel(
                -1,
                sidebar_x,
                0.0,
                sidebar_width,
                logical_h as f32,
                UIStyle {
                    bg_color: manifold_ui::color::EFFECT_CARD_INNER_BG_C32,
                    ..UIStyle::default()
                },
            );
            // Node-output pane. A non-image node fills the pane with its value
            // inspector (its own title heads the pane, in place of the image);
            // otherwise the generic "Node Output" title sits above the image,
            // and a hint fills the body when nothing is selected.
            let node_region = manifold_ui::Rect::new(
                title_x,
                node_title_y,
                preview_w,
                preview_title_h + preview_h,
            );
            let inspector_drawn = self
                .graph_editor_panel
                .render_node_inspector(&mut ws.ui_root.tree, node_region);
            if !inspector_drawn {
                ws.ui_root.tree.add_label(
                    -1,
                    title_x,
                    node_title_y,
                    preview_w,
                    preview_title_h,
                    "Node Output",
                    title_style,
                );
                if !show_image {
                    // Nothing selected — the image body stays empty; hint it.
                    ws.ui_root.tree.add_label(
                        -1,
                        title_x,
                        node_img_y + preview_h * 0.5 - 8.0,
                        preview_w,
                        16.0,
                        "Select a node",
                        UIStyle {
                            text_color: manifold_ui::color::TEXT_DIMMED_C32,
                            font_size: 12,
                            text_align: TextAlign::Center,
                            ..UIStyle::default()
                        },
                    );
                }
            }
            ws.ui_root.tree.add_label(
                -1,
                title_x,
                master_title_y,
                preview_w,
                preview_title_h,
                "Master Out",
                title_style,
            );
        }

        // Node picker overlays the whole editor window when open. Keep its
        // screen size in lockstep with the editor logical size (drives the
        // backdrop extent + edge-clamp), then build it last so its nodes
        // sit on top of palette + sidebar in the additive overlay below.
        ws.ui_root
            .browser_popup
            .set_screen_size(logical_w as f32, logical_h as f32);
        if ws.ui_root.browser_popup.is_open() {
            ws.ui_root.browser_popup.build(&mut ws.ui_root.tree);
        }

        // A pending jump-to-node centres now that `set_snapshot` has laid the
        // canvas out for this frame's scope (no-op until the target is present).
        if let Some(canvas) = self.graph_canvas.as_mut() {
            let vp = crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, logical_h as f32);
            canvas.resolve_pending_focus(vp);
            // Frame the whole level on editor open / scope change (camera only).
            canvas.apply_pending_fit(vp);
        }

        // ── Build frame: clear, then draw the canvas + sidebar ──
        let mut encoder = gpu.device.create_encoder("Graph Editor Frame");
        encoder.clear_texture(offscreen, 0.10, 0.10, 0.12, 1.0);

        if let (Some(ui), Some(canvas)) = (&mut self.ui_renderer, self.graph_canvas.as_ref()) {
            ui.begin_frame();
            canvas.render(
                ui,
                crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, logical_h as f32),
            );
            // Layer the sidebar UITree on top of the canvas's immediate-mode
            // draws (the flush protocol covers them with their own batches).
            ui.render_tree_range(&ws.ui_root.tree, 0, usize::MAX);
            // The mapping drawer floats over the composited canvas + sidebar:
            // it draws inline at POPOVER depth (above the CONTENT-depth nodes),
            // unclipped.
            if self.editor_mapping_popover.is_open() {
                ui.push_depth(Depth::POPOVER);
                self.editor_mapping_popover.set_live_value(popover_live_value);
                self.editor_mapping_popover.render(ui);
                ui.pop_depth();
            }
            // Graph text-input overlay (group rename / String param / wgsl /
            // node search) — tops everything at TOOLTIP depth.
            if self.text_input.active && self.text_input.field.is_graph_field() {
                ui.push_depth(Depth::TOOLTIP);
                render_text_input_overlay(&self.text_input, &self.frame_timer, ui);
                ui.pop_depth();
            }
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

        // ── Per-node thumbnails on the canvas ──
        // Blit each visible node's atlas cell into its body, over the canvas.
        // Authoring-only (the content thread only fills the atlas while the
        // editor is open). No-op until the first atlas frame lands.
        #[cfg(target_os = "macos")]
        if let (Some(thumb_p), Some(atlas_sampler), Some(bridge)) = (
            self.node_thumb_pipeline.as_ref(),
            self.thumb_sampler.as_ref(),
            self.node_atlas_texture_bridge.as_ref(),
        ) {
            let front = bridge.front_index() as usize;
            if let Some(atlas_tex) =
                self.ui_node_atlas_textures.get(front).and_then(|t| t.as_ref())
            {
                let layout: ahash::AHashMap<&manifold_core::NodeId, u32> = self
                    .content_state
                    .node_atlas_layout
                    .iter()
                    .map(|(id, cell)| (id, *cell))
                    .collect();
                if !layout.is_empty() {
                    let vp = crate::graph_canvas::Rect::new(
                        canvas_x,
                        0.0,
                        canvas_width,
                        logical_h as f32,
                    );
                    let thumbs = self
                        .graph_canvas
                        .as_ref()
                        .map(|c| c.visible_node_thumbnails(vp))
                        .unwrap_or_default();
                    let sf = scale as f32;
                    // The atlas is a GRID×GRID grid of equal-UV cells (cells are
                    // 16:9 in pixels, but each spans 1/GRID of the atlas on both
                    // axes), so one `inv` serves u and v. A half-texel inset keeps
                    // the Linear thumb sampler's taps inside the cell instead of
                    // bleeding the neighbour at the border.
                    let inv = 1.0 / crate::content_pipeline::ATLAS_GRID as f32;
                    let half_tx = 0.5 / crate::content_pipeline::ATLAS_W as f32;
                    let half_ty = 0.5 / crate::content_pipeline::ATLAS_H as f32;
                    for (node_id, bx, by, bw, bh) in thumbs {
                        let Some(&cell) = layout.get(&node_id) else {
                            continue;
                        };
                        // Clip the strip rect to the canvas viewport so a node
                        // straddling an edge can't blit its thumbnail over the
                        // param column (left) or the preview sidebar (right).
                        // Crop the source UVs by the same fraction the rect lost
                        // on each side so the visible part stays 1:1 instead of
                        // squashing the whole image into the clipped rect.
                        if bw <= 0.0 || bh <= 0.0 {
                            continue;
                        }
                        let clip_l = (vp.x - bx).max(0.0);
                        let clip_r = (bx + bw - (vp.x + vp.w)).max(0.0);
                        let clip_t = (vp.y - by).max(0.0);
                        let clip_b = (by + bh - (vp.y + vp.h)).max(0.0);
                        let cbw = bw - clip_l - clip_r;
                        let cbh = bh - clip_t - clip_b;
                        if cbw <= 0.0 || cbh <= 0.0 {
                            continue;
                        }
                        let cbx = bx + clip_l;
                        let cby = by + clip_t;
                        let gx = (cell % crate::content_pipeline::ATLAS_GRID) as f32;
                        let gy = (cell / crate::content_pipeline::ATLAS_GRID) as f32;
                        let u0 = gx * inv + half_tx;
                        let v0 = gy * inv + half_ty;
                        let du = inv - 2.0 * half_tx;
                        let dv = inv - 2.0 * half_ty;
                        let cell_uv = [
                            u0 + du * (clip_l / bw),
                            v0 + dv * (clip_t / bh),
                            du * (cbw / bw),
                            dv * (cbh / bh),
                        ];
                        let mut bytes = [0u8; 16];
                        for (k, v) in cell_uv.iter().enumerate() {
                            bytes[k * 4..k * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                        }
                        present_enc.draw_fullscreen_viewport(
                            thumb_p,
                            &drawable_tex,
                            &[
                                manifold_gpu::GpuBinding::Texture {
                                    binding: 0,
                                    texture: atlas_tex,
                                },
                                manifold_gpu::GpuBinding::Sampler {
                                    binding: 1,
                                    sampler: atlas_sampler,
                                },
                                manifold_gpu::GpuBinding::Bytes {
                                    binding: 2,
                                    data: &bytes,
                                },
                            ],
                            (cbx * sf, cby * sf, cbw * sf, cbh * sf),
                            manifold_gpu::GpuLoadAction::Load,
                            "Node Thumbnail",
                        );
                    }
                }
            }
        }

        // ── Sidebar-top preview monitors ──
        // Composite the captured node texture (top) and the master compositor
        // output (below it) into the editor sidebar, each below its title.
        // Only when the IOSurface front buffer is available. The blit pipeline
        // targets the drawable's Bgra8Unorm format, so these draw into the
        // drawable (Load) rather than the Rgba16Float offscreen.
        #[cfg(target_os = "macos")]
        {
            let scale = scale as f32;
            let w = preview_w * scale;
            let h = preview_h * scale;
            let x = (sidebar_x + preview_pad) * scale;
            // Node-output monitor — the selected image node's captured output.
            // Only when that node has an image; a non-image node's pane is
            // filled with the value inspector text instead (drawn above).
            if show_image
                && let Some(bridge) = self.node_preview_texture_bridge.as_ref()
            {
                let front = bridge.front_index() as usize;
                if let Some(tex) = self
                    .ui_node_preview_textures
                    .get(front)
                    .and_then(|t| t.as_ref())
                {
                    present_enc.draw_fullscreen_viewport(
                        blit_p,
                        &drawable_tex,
                        &[
                            manifold_gpu::GpuBinding::Texture {
                                binding: 0,
                                texture: tex,
                            },
                            manifold_gpu::GpuBinding::Sampler {
                                binding: 1,
                                sampler: blit_s,
                            },
                        ],
                        (x, node_img_y * scale, w, h),
                        manifold_gpu::GpuLoadAction::Load,
                        "Node Preview → Sidebar",
                    );
                }
            }
            // Master-out monitor — the live compositor output, the same texture
            // the main/perform window presents. Imported into `ui_preview_textures`
            // by the workspace-preview present path that runs just before this.
            if let Some(bridge) = self.preview_texture_bridge.as_ref() {
                let front = bridge.front_index() as usize;
                if let Some(tex) = self.ui_preview_textures.get(front).and_then(|t| t.as_ref()) {
                    present_enc.draw_fullscreen_viewport(
                        blit_p,
                        &drawable_tex,
                        &[
                            manifold_gpu::GpuBinding::Texture {
                                binding: 0,
                                texture: tex,
                            },
                            manifold_gpu::GpuBinding::Sampler {
                                binding: 1,
                                sampler: blit_s,
                            },
                        ],
                        (x, master_img_y * scale, w, h),
                        manifold_gpu::GpuLoadAction::Load,
                        "Master Out → Sidebar",
                    );
                }
            }
        }

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

        // Pass 2b: Audio Setup spectrogram waterfall. Render the VQT columns into
        // a UI-device texture, then blit it into the panel's reserved scope rect
        // (on top of the atlas, like the sidebar monitors). Gated on the modal
        // being open with a live column stream.
        #[cfg(target_os = "macos")]
        if self.ws.ui_root.audio_setup_panel.is_open()
            && self.content_state.spectrogram_num_bins > 0
            && let Some(rect) = self.ws.ui_root.audio_setup_panel.scope_rect()
            && let (Some(blit_pipeline), Some(blit_sampler)) =
                (&self.blit_pipeline, &self.blit_sampler)
        {
            use manifold_spectral::{Spectrogram, SpectrogramConfig};
            let num_bins = self.content_state.spectrogram_num_bins;
            let cfg = SpectrogramConfig::default();

            // (Re)create the renderer + target pane if the layout changed.
            if self.spectrogram.is_none() || self.spectrogram_num_bins != num_bins {
                self.spectrogram = Some(Spectrogram::new(
                    &gpu.device,
                    num_bins,
                    cfg.history_len,
                    manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    cfg.db_min,
                    cfg.db_max,
                ));
                let tex = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: 512,
                    height: 256,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                    label: "Audio Scope",
                    mip_levels: 1,
                });
                self.spectrogram_pane = Some(crate::texture_pane::TexturePane::local(tex));
                self.spectrogram_num_bins = num_bins;
            }

            if let (Some(spectrogram), Some(pane)) =
                (self.spectrogram.as_mut(), self.spectrogram_pane.as_mut())
                && let Some(target) = pane.local_target().cloned()
            {
                // Feed new columns (post-gain magnitudes from the worker).
                for col in self.content_state.spectrogram_columns.chunks_exact(num_bins) {
                    spectrogram.push_column(col);
                }
                // Band dividers at 250 Hz / 2 kHz (the modulation's low/mid/high
                // splits), as normalised y on the log axis.
                let (fmin, fmax) =
                    (self.content_state.spectrogram_fmin, self.content_state.spectrogram_fmax);
                let y_of = |f: f32| {
                    if fmin > 0.0 && fmax > fmin {
                        (f / fmin).log2() / (fmax / fmin).log2()
                    } else {
                        -1.0
                    }
                };
                spectrogram.render(&mut encoder, &target, [y_of(250.0), y_of(2000.0)]);

                // Blit through the unified TexturePane path (logical rect + scale).
                crate::texture_pane::blit_texture_pane(
                    pane,
                    &gpu.device,
                    &mut encoder,
                    blit_pipeline,
                    blit_sampler,
                    offscreen,
                    (rect.x, rect.y, rect.width, rect.height),
                    sf,
                    "Audio Scope Blit",
                );
            }
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
            // Waveform/stem lane buttons (Base layer, before the overlays).
            // Bounded by overlay_region_start — the overlays live past it and
            // are drawn separately below.
            let wf_first = self.ws.ui_root.waveform_lane.first_node();
            let sl_first = self.ws.ui_root.stem_lanes.first_node();
            let overlay_end = self.ws.ui_root.overlay_region_start;
            let overlay_start = if wf_first != usize::MAX {
                Some(wf_first)
            } else if sl_first != usize::MAX {
                Some(sl_first)
            } else {
                None
            };
            if let Some(start) = overlay_start {
                ui.render_tree_range(&self.ws.ui_root.tree, start, overlay_end);
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

            // Top-level overlays (perf HUD, dropdown, modals) — built by the
            // overlay driver at the tail of the tree, drawn here in z-order.
            // One source (overlay_draw, recorded at build) for build and draw,
            // so they cannot drift. overlay_draw is in Z_ORDER (bottom→top), so
            // each overlay's depth is OVERLAY + its stack index: a later-opened
            // overlay (e.g. a dropdown over the Audio Setup modal) paints over
            // an earlier one, text included. Each range gets its own push/pop
            // for scissor isolation.
            for (i, &(start, end)) in self.ws.ui_root.overlay_draw.iter().enumerate() {
                ui.push_depth(Depth::OVERLAY.above(i as i32));
                ui.render_tree_range(&self.ws.ui_root.tree, start, end);
                ui.pop_depth();
            }

            // Effect card drag ghost + text input — TOOLTIP depth, above every
            // overlay.
            ui.push_depth(Depth::TOOLTIP);
            if let Some(start) = self.ws.ui_root.inspector.card_drag_first_node() {
                ui.render_tree_range(&self.ws.ui_root.tree, start, usize::MAX);
            }
            // Text input overlay — last, so it tops everything.
            if self.text_input.active {
                render_text_input_overlay(&self.text_input, &self.frame_timer, ui);
            }
            ui.pop_depth();

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

/// Convert a renderer-side [`manifold_renderer::node_graph::NodeSnapshot`]
/// into the UI-facing [`manifold_ui::panels::graph_editor::GraphEditorNodeView`]
/// that the right-sidebar panel consumes.
///
/// Returns `None` when:
/// - no graph snapshot is available, or
/// - the canvas's selected node is not in the snapshot.
///
/// Intentionally does NOT gate on an effect target — generator graphs
/// have no effect identity, but the snapshot carries everything the
/// per-node param view needs (handle, title, params + ranges). Gating
/// on an effect-only target would silently empty the right column for
/// every generator graph the user opens.
/// Recursively find a snapshot node by stable [`NodeId`], descending into
/// groups. Resolves a previewed node's title + type_id for the value inspector.
fn find_snapshot_node<'a>(
    nodes: &'a [manifold_renderer::node_graph::NodeSnapshot],
    id: &manifold_core::NodeId,
) -> Option<&'a manifold_renderer::node_graph::NodeSnapshot> {
    for n in nodes {
        if &n.node_id == id {
            return Some(n);
        }
        if let Some(g) = n.group.as_ref()
            && let Some(found) = find_snapshot_node(&g.nodes, id)
        {
            return Some(found);
        }
    }
    None
}

/// Seed text for the inline `Table` cell editor — compact but lossless enough
/// to round-trip: integers without a decimal point, fractionals to four places
/// with trailing zeros trimmed.
fn fmt_table_cell_seed(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1.0e7 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn build_graph_editor_view(
    selected_node: Option<u32>,
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
    scope: &[u32],
) -> Option<manifold_ui::panels::graph_editor::GraphEditorNodeView> {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    use manifold_ui::panels::graph_editor::{
        GraphEditorNodeView, GraphEditorParam, GraphEditorParamKind,
    };

    let id = selected_node?;
    let snap = snapshot?;
    // The selected id is level-local: when the canvas has descended into a
    // group, the node lives in that group's body, not the document root. Resolve
    // the level the canvas is showing before searching, or its params come back
    // empty for every node inside a group.
    let level_nodes = crate::graph_canvas::resolve_level(snap, scope)
        .map(|(nodes, _)| nodes)
        .unwrap_or(snap.nodes.as_slice());
    let node = level_nodes.iter().find(|n| n.id == id)?;
    let parameters = node
        .parameters
        .iter()
        .map(|p| GraphEditorParam {
            name: p.name.clone(),
            label: p.label.clone(),
            kind: match p.kind {
                ParamSnapshotKind::Float => GraphEditorParamKind::Float,
                ParamSnapshotKind::Angle => GraphEditorParamKind::Angle,
                ParamSnapshotKind::Frequency => GraphEditorParamKind::Frequency,
                ParamSnapshotKind::Int => GraphEditorParamKind::Int,
                ParamSnapshotKind::Bool => GraphEditorParamKind::Bool,
                ParamSnapshotKind::Enum => GraphEditorParamKind::Enum,
                ParamSnapshotKind::Trigger => GraphEditorParamKind::Trigger,
                ParamSnapshotKind::Color => GraphEditorParamKind::Color,
                ParamSnapshotKind::Vec2 => GraphEditorParamKind::Vec2,
                ParamSnapshotKind::Vec3 => GraphEditorParamKind::Vec3,
                ParamSnapshotKind::Vec4 => GraphEditorParamKind::Vec4,
                ParamSnapshotKind::String => GraphEditorParamKind::String,
                ParamSnapshotKind::Other => GraphEditorParamKind::Other,
            },
            default_value: p.default_value,
            current_value: p.current_value,
            range: p.range,
            enum_labels: p.enum_labels.clone(),
            summary: p.summary.clone(),
            vec_value: p.vec_value.unwrap_or([0.0; 4]),
            string_value: p.string_value.clone(),
            table_value: p.table_value.clone(),
        })
        .collect();
    Some(GraphEditorNodeView {
        runtime_node_id: node.id,
        node_id: node.node_id.clone(),
        node_handle: node.node_handle.clone(),
        title: node.title.clone(),
        parameters,
        wgsl_source: node.wgsl_source.clone(),
    })
}

/// Resolve the selected canvas node — which may be a *boundary* node that has
/// no runtime instance of its own — to a concrete preview-target [`NodeId`] the
/// content thread can capture. Walks the hierarchical snapshot at the canvas
/// scope:
///
/// - A plain node previews itself.
/// - A **Group Output** boundary previews the enclosing group's *container*
///   node_id, so the content side resolves it through the existing
///   `group_preview_map` (producer + interface port name for encoding).
/// - A **Group Input** boundary previews the external node feeding that group's
///   primary input, one scope level up.
/// - A `final_output` sink previews its input producer.
/// - A sub-group producer is previewed by its container node_id (also via
///   `group_preview_map`); a `group_input` producer recurses to the parent feed.
///
/// Pure over the snapshot, so it's unit-tested below.
fn resolve_preview_target(
    snap: &manifold_renderer::node_graph::GraphSnapshot,
    scope: &[u32],
    selected: u32,
) -> Option<manifold_core::NodeId> {
    let (nodes, wires) =
        crate::graph_canvas::resolve_level(snap, scope).unwrap_or((&snap.nodes, &snap.wires));
    let node = nodes.iter().find(|n| n.id == selected)?;
    resolve_boundary_node(node, nodes, wires, snap, scope)
}

fn resolve_boundary_node(
    node: &manifold_renderer::node_graph::NodeSnapshot,
    nodes: &[manifold_renderer::node_graph::NodeSnapshot],
    wires: &[manifold_renderer::node_graph::WireSnapshot],
    snap: &manifold_renderer::node_graph::GraphSnapshot,
    scope: &[u32],
) -> Option<manifold_core::NodeId> {
    use manifold_core::effect_graph_def::{GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID};
    match node.type_id.as_str() {
        GROUP_OUTPUT_TYPE_ID => {
            // The enclosing group's output. Send the group container's node_id;
            // the content side maps it to the producer (+ port name) itself.
            let (group_u32, parent) = scope.split_last()?;
            let (p_nodes, _) =
                crate::graph_canvas::resolve_level(snap, parent).unwrap_or((&snap.nodes, &snap.wires));
            non_empty_node_id(&p_nodes.iter().find(|n| n.id == *group_u32)?.node_id)
        }
        GROUP_INPUT_TYPE_ID => {
            // Data entering the enclosing group: its external feeder, one level up.
            // The boundary's outputs are the group's input ports.
            let port = primary_texture_port(&node.outputs)?;
            let (group_u32, parent) = scope.split_last()?;
            let (p_nodes, p_wires) =
                crate::graph_canvas::resolve_level(snap, parent).unwrap_or((&snap.nodes, &snap.wires));
            let producer = producer_into(*group_u32, port, p_nodes, p_wires)?;
            resolve_producer(producer, p_nodes, p_wires, snap, parent)
        }
        "system.final_output" => {
            let port = primary_texture_port(&node.inputs)?;
            let producer = producer_into(node.id, port, nodes, wires)?;
            resolve_producer(producer, nodes, wires, snap, scope)
        }
        // Plain node, Source, generator_input: preview the node itself.
        _ => non_empty_node_id(&node.node_id),
    }
}

/// Concrete preview target for a producer node: a sub-group resolves by its
/// container id (content `group_preview_map`); a `group_input` producer recurses
/// to the parent feed; anything else previews itself.
fn resolve_producer(
    producer: &manifold_renderer::node_graph::NodeSnapshot,
    nodes: &[manifold_renderer::node_graph::NodeSnapshot],
    wires: &[manifold_renderer::node_graph::WireSnapshot],
    snap: &manifold_renderer::node_graph::GraphSnapshot,
    scope: &[u32],
) -> Option<manifold_core::NodeId> {
    use manifold_core::effect_graph_def::GROUP_INPUT_TYPE_ID;
    if producer.group.is_some() {
        return non_empty_node_id(&producer.node_id);
    }
    if producer.type_id == GROUP_INPUT_TYPE_ID {
        return resolve_boundary_node(producer, nodes, wires, snap, scope);
    }
    non_empty_node_id(&producer.node_id)
}

/// The node feeding `(to_node, to_port)`, or any wire into `to_node` as a
/// fallback when the exact port name doesn't match.
fn producer_into<'a>(
    to_node: u32,
    to_port: &str,
    nodes: &'a [manifold_renderer::node_graph::NodeSnapshot],
    wires: &[manifold_renderer::node_graph::WireSnapshot],
) -> Option<&'a manifold_renderer::node_graph::NodeSnapshot> {
    let w = wires
        .iter()
        .find(|w| w.to_node == to_node && w.to_port == to_port)
        .or_else(|| wires.iter().find(|w| w.to_node == to_node))?;
    nodes.iter().find(|n| n.id == w.from_node)
}

/// First `Texture2D`(-typed) port name, else the first port of any type.
fn primary_texture_port(ports: &[manifold_renderer::node_graph::PortSnapshot]) -> Option<&str> {
    use manifold_renderer::node_graph::PortKindSnapshot;
    ports
        .iter()
        .find(|p| {
            matches!(
                p.kind,
                PortKindSnapshot::Texture2D | PortKindSnapshot::Texture2DTyped { .. }
            )
        })
        .or_else(|| ports.first())
        .map(|p| p.name.as_str())
}

fn non_empty_node_id(id: &manifold_core::NodeId) -> Option<manifold_core::NodeId> {
    if id.as_str().is_empty() {
        None
    } else {
        Some(id.clone())
    }
}

/// Resolve an on-canvas param row `(node_id, inner_param)` to the
/// matching card `UserParamBinding` on the watched effect, returning the
/// data the mapping popover needs to open. `None` when there's no active
/// snapshot/target, the node has no stable handle, or the inner param
/// isn't exposed as a user binding (only user-bound rows get the popover;
/// preset/static routings and plain inner params don't).
///
/// Returned tuple: `(binding_id, label, min, max, invert, curve, range)`.
/// `range` is the binding's declared inner-param bounds, used to span the
/// popover's trim track.
///
/// Free function (not a method) so the editor-window mouse handler can
/// call it while the `&mut GraphCanvas` borrow is live: it takes the
/// disjoint `self` fields (snapshot, target, project) by reference rather
/// than borrowing all of `self`.
#[allow(clippy::type_complexity)]
pub(crate) fn resolve_canvas_binding(
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
    target: Option<&manifold_core::GraphTarget>,
    project: &manifold_core::project::Project,
    node_id: u32,
    inner_param: &str,
) -> Option<(
    String,
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
    Option<(f32, f32)>,
)> {
    let snap = snapshot?;
    // Canvas runtime id → the node's stable NodeId (anonymous boundary
    // nodes have an empty id and can't carry bindings).
    let node = snap.nodes.iter().find(|n| n.id == node_id)?;
    if node.node_id.is_empty() {
        return None;
    }
    // Declared inner-param range, for the trim track span.
    let range = node
        .parameters
        .iter()
        .find(|p| p.name == inner_param)
        .and_then(|p| p.range);
    // Only effect graphs carry card user-bindings; a generator target has none.
    let manifold_core::GraphTarget::Effect(eid) = target? else {
        return None;
    };
    let fx = project.find_effect_by_id(eid)?;
    let b = fx
        .user_param_bindings()
        .into_iter()
        .find(|b| b.node_id == node.node_id && b.inner_param == inner_param)?;
    Some((
        b.id.clone(),
        b.label.clone(),
        b.min,
        b.max,
        b.invert,
        b.curve,
        b.scale,
        b.offset,
        range,
    ))
}

/// Build the unified set of `(node_handle, inner_param)` keys for every
/// inner-node param currently exposed on the outer card. Reads ONLY
/// the snapshot's per-param `exposed` flag — the graph is the single
/// source of truth for exposure state, identical for Effect-hosted
/// and Generator-hosted graphs.
///
/// Drives the per-node "Expose to card" checkbox state in the
/// graph-editor sidebar.
fn build_card_exposures(
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
) -> std::collections::HashSet<(String, String)> {
    let Some(snap) = snapshot else {
        return Default::default();
    };
    let mut out = std::collections::HashSet::new();
    for node in &snap.nodes {
        let Some(handle) = node.node_handle.as_deref() else {
            continue;
        };
        for p in &node.parameters {
            if p.exposed {
                out.insert((handle.to_string(), p.name.clone()));
            }
        }
    }
    out
}

/// Flatten the snapshot's outer→inner routings into a
/// `(node_handle, inner_param) → outer_label` map. Empty when the
/// snapshot is `None` or no effect declares outer routings. Drives
/// the "↳ <outer>" hint on the per-node rows so the user can see
/// which outer slider drives each inner param.
fn build_outer_driven_map(
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
) -> std::collections::HashMap<(String, String), String> {
    let Some(snap) = snapshot else {
        return Default::default();
    };
    snap.outer_routings
        .iter()
        .map(|r| {
            (
                (r.node_handle.clone(), r.inner_param.clone()),
                r.outer_label.clone(),
            )
        })
        .collect()
}

/// `(node_handle, inner_param)` keys for every inner param shadowed
/// by a wire on the node's same-named scalar input port (port-
/// shadows-param convention). Built by walking the snapshot's live
/// `wires` and joining each `to_node` to its `node_handle` via the
/// snapshot's node table. Empty when the snapshot is `None`, when
/// the graph has no wires, or when no wire lands on a handled node.
///
/// Drives the graph-editor sidebar's "← wired" hint and disables the
/// per-row checkbox + value cell so local edits and card-exposure
/// toggles can't lie about what controls a wire-driven param.
fn build_wire_driven_keys(
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
) -> std::collections::HashSet<(String, String)> {
    let Some(snap) = snapshot else {
        return Default::default();
    };
    let handles: std::collections::HashMap<u32, &str> = snap
        .nodes
        .iter()
        .filter_map(|n| n.node_handle.as_deref().map(|h| (n.id, h)))
        .collect();
    snap.wires
        .iter()
        .filter_map(|w| handles.get(&w.to_node).map(|h| ((*h).to_string(), w.to_port.clone())))
        .collect()
}

/// `(node_handle, inner_param) → static-block slot index` map for the
/// active effect. Built by resolving each snapshot
/// `OuterParamRouting.outer_param_id` through the def's `id_to_index`
/// table. Empty when there's no active effect or no snapshot.
///
/// Used by the graph-editor sidebar so the per-node "Expose to card"
/// checkbox can route through `EffectStaticParamExpose` (flipping the
/// slot's `exposed` flag) when the inner param is already driven by
/// a static-block routing — instead of stacking a redundant
/// `UserParamBinding` on top of an already-routed param.
fn build_static_block_targets(
    target: Option<&manifold_core::GraphTarget>,
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
    project: &manifold_core::project::Project,
) -> std::collections::HashMap<(String, String), usize> {
    let Some(snap) = snapshot else {
        return Default::default();
    };
    // Only effect editors have a static-block routing; a generator target (or a
    // closed editor) has no outer→inner param routings to map.
    let Some(manifold_core::GraphTarget::Effect(eid)) = target else {
        return Default::default();
    };
    let Some(fx) = project.find_effect_by_id(eid) else {
        return Default::default();
    };
    let Some(def) = manifold_core::preset_definition_registry::try_get(fx.effect_type()) else {
        return Default::default();
    };
    snap.outer_routings
        .iter()
        .filter_map(|r| {
            def.id_to_index
                .get(&r.outer_param_id)
                .copied()
                .map(|slot| ((r.node_handle.clone(), r.inner_param.clone()), slot))
        })
        .collect()
}




#[cfg(test)]
mod preview_target_tests {
    use super::resolve_preview_target;
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID};
    use manifold_renderer::node_graph::{
        GraphSnapshot, GroupSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
    };

    fn tex(name: &str) -> PortSnapshot {
        PortSnapshot {
            name: name.to_string(),
            kind: PortKindSnapshot::Texture2D,
        }
    }

    fn node(
        id: u32,
        node_id: &str,
        type_id: &str,
        ins: Vec<PortSnapshot>,
        outs: Vec<PortSnapshot>,
    ) -> NodeSnapshot {
        NodeSnapshot {
            id,
            node_id: NodeId::new(node_id),
            node_handle: None,
            type_id: type_id.to_string(),
            title: String::new(),
            inputs: ins,
            outputs: outs,
            parameters: Vec::new(),
            editor_pos: None,
            breaks_dependency_cycle: false,
            group: None,
            wgsl_source: None,
        }
    }

    fn wire(fnode: u32, fport: &str, tnode: u32, tport: &str) -> WireSnapshot {
        WireSnapshot {
            from_node: fnode,
            from_port: fport.to_string(),
            to_node: tnode,
            to_port: tport.to_string(),
        }
    }

    fn snap(nodes: Vec<NodeSnapshot>, wires: Vec<WireSnapshot>) -> GraphSnapshot {
        GraphSnapshot {
            nodes,
            wires,
            outer_routings: Vec::new(),
        }
    }

    /// Group container (id 5, node_id "grp"): body is
    /// group_input(0) -> inner(1) -> group_output(2).
    fn group_container() -> NodeSnapshot {
        let mut g = node(5, "grp", GROUP_TYPE_ID, vec![tex("src")], vec![tex("out")]);
        g.group = Some(Box::new(GroupSnapshot {
            nodes: vec![
                node(0, "", GROUP_INPUT_TYPE_ID, vec![], vec![tex("src")]),
                node(1, "inner", "node.blur", vec![tex("in")], vec![tex("out")]),
                node(2, "", GROUP_OUTPUT_TYPE_ID, vec![tex("out")], vec![]),
            ],
            wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
            tint: None,
        }));
        g
    }

    #[test]
    fn plain_node_previews_itself() {
        let s = snap(
            vec![node(1, "blur", "node.blur", vec![tex("in")], vec![tex("out")])],
            vec![],
        );
        assert_eq!(resolve_preview_target(&s, &[], 1), Some(NodeId::new("blur")));
    }

    #[test]
    fn group_output_boundary_resolves_to_container() {
        let s = snap(
            vec![
                node(10, "src", "system.source", vec![], vec![tex("out")]),
                group_container(),
            ],
            vec![wire(10, "out", 5, "src")],
        );
        // Inside the group, the output boundary previews the group container,
        // so the content side maps it (+ port name) via group_preview_map.
        assert_eq!(resolve_preview_target(&s, &[5], 2), Some(NodeId::new("grp")));
    }

    #[test]
    fn group_input_boundary_resolves_to_external_feeder() {
        let s = snap(
            vec![
                node(10, "src", "system.source", vec![], vec![tex("out")]),
                group_container(),
            ],
            vec![wire(10, "out", 5, "src")],
        );
        // Inside the group, the input boundary previews whatever feeds the
        // group from outside (the source).
        assert_eq!(resolve_preview_target(&s, &[5], 0), Some(NodeId::new("src")));
    }

    #[test]
    fn final_output_resolves_to_its_input_producer() {
        let s = snap(
            vec![
                node(0, "src", "system.source", vec![], vec![tex("out")]),
                node(1, "inv", "node.invert", vec![tex("in")], vec![tex("out")]),
                node(2, "", "system.final_output", vec![tex("in")], vec![]),
            ],
            vec![wire(0, "out", 1, "in"), wire(1, "out", 2, "in")],
        );
        assert_eq!(resolve_preview_target(&s, &[], 2), Some(NodeId::new("inv")));
    }
}
