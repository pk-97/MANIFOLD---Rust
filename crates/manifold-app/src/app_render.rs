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

    /// Cursor position over the audio scope's waterfall, if inside it:
    /// `(uv_x, uv_y, freq_hz)` with uv in 0..1 (y top→bottom) and the frequency
    /// at that height on the log axis. `None` when the cursor is elsewhere, the
    /// scope is closed, or the analysed range is degenerate.
    fn scope_hover_uv(&self) -> Option<(f32, f32, f32)> {
        let rect = self.ws.ui_root.audio_setup_panel.scope_rect()?;
        if !rect.contains(self.cursor_pos) || rect.width <= 0.0 || rect.height <= 0.0 {
            return None;
        }
        let fmin = self.content_state.spectrogram_fmin;
        let fmax = self.content_state.spectrogram_fmax;
        if !(fmin > 0.0 && fmax > fmin) {
            return None;
        }
        let ux = ((self.cursor_pos.x - rect.x) / rect.width).clamp(0.0, 1.0);
        let uy = ((self.cursor_pos.y - rect.y) / rect.height).clamp(0.0, 1.0);
        // uv.y=1 (bottom) → fmin, uv.y=0 (top) → fmax; freq is geometric in height.
        let freq = fmin * (fmax / fmin).powf(1.0 - uy);
        Some((ux, uy, freq))
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

    /// Resolve an effect card's row index (in the active inspector tab) to its
    /// stable [`EffectId`]. Mirrors the cog's `OpenGraphEditor` resolution:
    /// keyed by instance id, tab-aware (Master / Layer|Group / Clip). Takes
    /// `&mut self` only because the Clip-tab lookup runs through
    /// `Timeline::find_clip_by_id`, which self-heals its location cache.
    fn resolve_effect_card_id(&mut self, ei: usize) -> Option<manifold_core::EffectId> {
        match self.ws.ui_root.inspector.last_effect_tab() {
            manifold_ui::InspectorTab::Master => self
                .local_project
                .settings
                .master_effects
                .get(ei)
                .map(|e| e.id.clone()),
            manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group => self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                .and_then(|(_, l)| l.effects.as_ref())
                .and_then(|effects| effects.get(ei))
                .map(|e| e.id.clone()),
            manifold_ui::InspectorTab::Clip => self
                .selection
                .primary_selected_clip_id
                .as_ref()
                .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                .and_then(|c| c.effects.get(ei))
                .map(|e| e.id.clone()),
        }
    }

    /// Point the graph editor at an effect instance: push the watched identity
    /// to the content thread and cache its catalog-default graph def (so the
    /// per-card edit commands can lift `instance.graph` from `None` on first
    /// edit). Does NOT open the window — shared by the card cog (which also
    /// sets `pending_open_graph_editor`) and selection-follows (retarget-only).
    fn watch_effect_graph(&mut self, effect_id: manifold_core::EffectId) {
        self.send_content_cmd(ContentCommand::WatchEffectGraph(Some(effect_id.clone())));
        self.watched_catalog_default = self.local_project.find_effect_by_id(&effect_id).and_then(
            |instance| manifold_renderer::node_graph::catalog_graph_def_for(instance.effect_type()),
        );
        self.watched_graph_target = Some(manifold_core::GraphTarget::Effect(effect_id));
    }

    /// Point the graph editor at a layer's generator graph. Caches the bundled
    /// preset JSON for the layer's generator type as the catalog default. Does
    /// NOT open the window — shared by the generator-card cog and
    /// selection-follows.
    fn watch_generator_graph(&mut self, layer_id: manifold_core::LayerId) {
        self.send_content_cmd(ContentCommand::WatchGeneratorGraph(Some(layer_id.clone())));
        self.watched_catalog_default = self
            .local_project
            .timeline
            .find_layer_by_id(&layer_id)
            .map(|(_, l)| l.generator_type().clone())
            .filter(|gt| !gt.is_none())
            .and_then(|gt| manifold_renderer::node_graph::bundled_preset_json(&gt))
            .and_then(|json| serde_json::from_str(&json).ok());
        self.watched_graph_target = Some(manifold_core::GraphTarget::Generator(layer_id));
    }

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
                        // Restore actively-dragged inspector field so snapshot
                        // doesn't overwrite the value the user is manipulating.
                        if let Some(ref drag) = self.active_inspector_drag {
                            drag.apply(&mut self.local_project);
                        }
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
                    // Restore actively-dragged inspector field so modulation
                    // doesn't overwrite the value the user is manipulating.
                    if let Some(ref drag) = self.active_inspector_drag {
                        drag.apply(&mut self.local_project);
                    }
                }
                // Accumulate VQT columns from EVERY drained snapshot — the
                // assignment below keeps only the latest, so reading columns off
                // `content_state` would drop those from earlier snapshots when
                // several arrive in one UI frame, and re-push them on frames that
                // drain none. The render path consumes (clears) this buffer.
                self.pending_spectrogram_columns
                    .extend_from_slice(&state.spectrogram_columns);
                // Overlay scalars ride in lockstep (2 per column).
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
                        // Drop the matching scalar records (4 per column).
                        let cols = excess / nb;
                        let scalar_excess = (cols * 4).min(self.pending_spectrogram_scalars.len());
                        self.pending_spectrogram_scalars.drain(0..scalar_excess);
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

        // 1e2. Sync live recording state to layer header record button.
        self.ws.ui_root.layer_headers.set_recording_active(
            &mut self.ws.ui_root.tree,
            self.content_state.is_live_recording,
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
                M::New => actions.push(P::NewProject),
                M::Open => actions.push(P::OpenProject),
                M::OpenRecentPath(path) => {
                    self.open_project_from_path(path);
                    self.needs_structural_sync = true;
                }
                M::ClearRecentProjects => {
                    self.project_io.clear_recent_projects(&mut self.user_prefs);
                    self.refresh_recent_menu();
                }
                M::Save => actions.push(P::SaveProject),
                M::SaveAs => actions.push(P::SaveProjectAs),
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
                M::ExportVideo => actions.push(P::ExportVideo),
                M::ExportFrame => actions.push(P::ExportFrame),
                M::Perform => actions.push(P::EnterPerformMode),
                M::Monitor => actions.push(P::ToggleMonitor),
                M::Audio => actions.push(P::OpenAudioSetup),
                M::ImportVideo => self.import_video_clip(),
                M::Undo => {
                    if let Some(tx) = self.content_tx.as_ref() {
                        crate::ui_bridge::undo(tx);
                    }
                    // D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2):
                    // the real "Undid: <command name>" label now fires from
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
        // harmlessly. One drain point for both scroll inputs.
        if self.ws.ui_root.inspector.take_scrolled_in_place()
            && let Some(cm) = &mut self.ui_cache_manager
        {
            cm.invalidate_inspector();
        }
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
                                    graph_edits.push(
                                        manifold_ui::GraphEditCommand::AddGraphNodeAt {
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
                                &self.ws.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { .. } => {
                            self.overlay.on_end_drag(&mut host, &self.ws.ui_root.viewport);
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
                PanelAction::OpenGraphEditor(ei) => {
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
                                curve: Some(crate::ui_translate::macro_curve_to_core(*curve)),
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
                    if let Some(ui_snap) = self.editor_ui_snapshot()
                        && let Some(node_id) =
                            crate::graph_canvas::resolve_card_param_node_id(&ui_snap, binding_id)
                        && let Some(canvas) = self.graph_canvas.as_mut()
                    {
                        canvas.focus_node(&ui_snap, &node_id);
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
                PanelAction::ExportFrame => {
                    self.export_frame();
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
                PanelAction::FpsFieldClicked => {
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
                PanelAction::BeginParamTextInput {
                    target,
                    param_id,
                    anchor,
                    value,
                    min,
                    max,
                    whole_numbers,
                } => {
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
                        target: *target,
                        param_id: param_id.clone(),
                        old_value: *value,
                        min: *min,
                        max: *max,
                        whole_numbers: *whole_numbers,
                    });
                    continue;
                }
                PanelAction::BeginDriverPeriodTextInput {
                    target,
                    param_id,
                    anchor,
                    value,
                } => {
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
                            target: *target,
                            param_id: param_id.clone(),
                        });
                    continue;
                }
                PanelAction::LayerDoubleClicked(idx) => {
                    // Open text input for layer rename
                    {
                        let project = &self.local_project;
                        if let Some(layer) = project.timeline.layers.get(*idx) {
                            let r = if let Some(nid) = self.ws.ui_root.layer_headers.name_node_id(*idx) {
                                self.ws.ui_root.tree.get_bounds(nid)
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
                            // Typed (2b.11): each font carries its GenStringParamSelected.
                            let items: Vec<manifold_ui::panels::dropdown::DropdownItem> = if key
                                == "fontFamily"
                            {
                                manifold_renderer::text_rasterizer::TextRasterizer::available_font_families()
                                        .into_iter()
                                        .map(|name| manifold_ui::panels::dropdown::DropdownItem::new(&name)
                                            .with_action(PanelAction::GenStringParamSelected(*sp_idx, name.clone())))
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
                PanelAction::EffectCardClicked(ei) => {
                    if action_idx < editor_card_seg_start
                        && self.graph_editor_window_id.is_some()
                        && let Some(eid) = self.resolve_effect_card_id(*ei)
                    {
                        self.watch_effect_graph(eid);
                    }
                }
                PanelAction::GenCardClicked => {
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
                &mut self.audio_shape_snapshot,
                &mut self.audio_crossover_snapshot,
                &mut self.user_prefs,
                &mut self.active_inspector_drag,
                None,
            );
            if result.structural_change {
                needs_structural_sync = true;
            }
            if result.resolution_changed {
                needs_resolution_resize = true;
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
            if let Some(ed) = self.graph_editor.as_mut() {
                let content_tx = self.content_tx.as_ref().unwrap();
                for action in &actions[editor_card_seg_start..] {
                    match action {
                        PanelAction::EffectCardClicked(ei) => retarget_effect = Some(*ei),
                        PanelAction::GenCardClicked => retarget_generator = true,
                        _ => {}
                    }
                    let result = crate::ui_bridge::dispatch(
                        action,
                        &mut self.local_project,
                        content_tx,
                        &self.content_state,
                        &mut ed.ui_root,
                        &mut self.selection,
                        &mut self.active_layer_id,
                        &mut self.slider_snapshot,
                        &mut self.trim_snapshot,
                        &mut self.target_snapshot,
                        &mut self.decay_snapshot,
                        &mut self.audio_shape_snapshot,
                        &mut self.audio_crossover_snapshot,
                        &mut self.user_prefs,
                        &mut self.active_inspector_drag,
                        None,
                    );
                    if result.structural_change {
                        needs_structural_sync = true;
                    }
                    if result.resolution_changed {
                        needs_resolution_resize = true;
                    }
                }
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
                    if let (Some(eid), Some(default)) = (
                        self.watched_graph_target.as_ref(),
                        self.watched_catalog_default.as_ref(),
                    ) {
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            eid.clone(),
                            *node_id,
                            param_name.clone(),
                            crate::ui_translate::serialized_param_value_to_core(new_value),
                            default.clone(),
                        )
                        .with_scope(canvas_scope.clone());
                        self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                    }
                    continue;
                }
                manifold_ui::GraphEditCommand::BrowseGraphNodePath { node_id, param_name } => {
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

            // Per-row trigger meters + firing flash: the selected send's per-band
            // transient levels (Whole/Low/Mid/High) — the exact impulse the routes
            // fire on, so the row meter is "what gets detected," and the flash
            // blinks when a band crosses its threshold.
            let trig_levels = self.content_state.spectrogram_features.map(|f| {
                use manifold_core::AudioBand;
                [
                    f.bands[AudioBand::Full.index()].transients,
                    f.bands[AudioBand::Low.index()].transients,
                    f.bands[AudioBand::Mid.index()].transients,
                    f.bands[AudioBand::High.index()].transients,
                ]
            });
            self.ws.ui_root.update_audio_trigger_levels(trig_levels);

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
            self.present_graph_editor_window();
            self.ui_profile.add("present_graph_editor", g0.elapsed());
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.ui_profile.add("update_repaint_upload", seg.elapsed());
            self.present_all_windows(0);
            let g0 = std::time::Instant::now();
            self.present_graph_editor_window();
            self.ui_profile.add("present_graph_editor", g0.elapsed());
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
        // Same `Dock` the present pass reads, so this mapping viewport tracks
        // whatever the user dragged the columns to.
        let area = manifold_ui::Rect::new(0.0, 0.0, logical_w, logical_h);
        let c = self.graph_editor.as_ref()?.dock.canvas(area);
        Some(crate::graph_canvas::Rect::new(c.x, c.y, c.width, c.height))
    }

    /// The current editor graph as the UI-local view-model the canvas reads,
    /// translating (and caching) the renderer snapshot on change. Returns an
    /// owned `Arc` (no `self` borrow held), so callers can use it alongside a
    /// `&mut self.graph_canvas` borrow. `None` when no graph is active.
    pub(crate) fn editor_ui_snapshot(
        &mut self,
    ) -> Option<std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>> {
        let src = self.content_state.active_graph_snapshot.as_ref()?;
        let fresh =
            matches!(&self.editor_ui_graph, Some((cached, _)) if std::sync::Arc::ptr_eq(cached, src));
        if !fresh {
            let ui = std::sync::Arc::new(crate::ui_translate::graph_snapshot_to_ui(src));
            self.editor_ui_graph = Some((src.clone(), ui));
        }
        self.editor_ui_graph.as_ref().map(|(_, ui)| ui.clone())
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
        // The UI-local graph view-model the canvas + these editor helpers read
        // (Phase 8). Translated once (cached by Arc identity); the renderer
        // snapshot stays the source for the binding/exposure helpers below.
        let editor_ui_snap = self.editor_ui_snapshot();
        let preview_node = match (self.graph_canvas.as_ref(), editor_ui_snap.as_deref()) {
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
            editor_ui_snap.as_deref(),
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

        // ── Mini-timeline data ────────────────────────────────────────
        // Built from the UI project mirror + playhead BEFORE the disjoint `ws`
        // borrow below. Cheap: walks the clip list once, off the per-node hot
        // path. Shared with the headless snapshot path via `mini_timeline_data`.
        let mini_current_beat = self.content_state.current_beat.as_f32();
        let mini_is_playing = self.content_state.is_playing;
        let (mini_clips, mini_layer_labels, mini_rows, mini_total_beats, mini_beats_per_bar, mini_readout) =
            mini_timeline_data(&self.local_project, mini_current_beat);

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
        // Left preview sidebar (monitors) + center canvas + right card lane
        // (param expose) — card on the right, same convention as the main
        // timeline's inspector (`inspector_width` docks at `screen_width -
        // inspector_width`). Built BEFORE rendering so the tree's nodes
        // (panels + buttons + labels) are ready to draw alongside the canvas.
        // Column geometry comes from the workspace's resizable `Dock` (left
        // preview column + right card lane). One `rects()` call feeds render,
        // `editor_canvas_viewport`, and the pointer handlers alike, so the
        // canvas origin and click hit-testing stay in lockstep no matter how
        // the user drags the dividers.
        let editor_area = manifold_ui::Rect::new(0.0, 0.0, logical_w as f32, logical_h as f32);
        let dock = ws.dock.rects(editor_area);
        let preview_width = dock.left.width;
        let card_width = dock.right.width;
        let canvas_x = dock.canvas.x;
        let canvas_width = dock.canvas.width;
        // Canvas height is short of the window by the bottom mini-timeline strip
        // (via the same `dock` the input pass reads, so hit-testing tracks it).
        let canvas_height = dock.canvas.height;
        let card_x = dock.right.x;
        // When a node is being previewed, the preview pane occupies the top of
        // the sidebar; the expose/param rows start below it so they don't
        // overlap. Logical units; the present pass draws the pane to match.
        let preview_pad = 8.0_f32;
        let preview_title_h = 18.0_f32;
        // Monitors take the project aspect ratio (a portrait show → portrait
        // monitors), fit to the column width and clamped in height so the two
        // stacked panes always fit the editor window, never spilling onto the
        // chrome below. Landscape stays full-width (unchanged); portrait is
        // narrower and centred in the column.
        let monitor_aspect = self
            .content_pipeline_output
            .as_ref()
            .map(|p| p.get_dimensions())
            .filter(|(_, h)| *h > 0)
            .map(|(w, h)| w as f32 / h as f32)
            .unwrap_or(16.0 / 9.0);
        let avail_w = (preview_width - 2.0 * preview_pad).max(1.0);
        // Height budget so 2×(title + body) + 3 pads fits the column vertically.
        // `canvas_height` (not the full window) — the column stops above the
        // full-width mini-timeline strip.
        let max_body_h =
            ((canvas_height - 3.0 * preview_pad - 2.0 * preview_title_h) * 0.5).max(1.0);
        let width_bound_h = avail_w / monitor_aspect;
        let (preview_w, preview_h) = if width_bound_h <= max_body_h {
            (avail_w, width_bound_h)
        } else {
            (max_body_h * monitor_aspect, max_body_h)
        };
        let preview_x = (preview_width - preview_w) * 0.5;
        // The left column is monitors-only now — the inner-node param list moved
        // under the right card. Two equal stacked 16:9 monitors (the selected
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
        let mut pane_y = ((canvas_height - pane_block_h) * 0.5).max(preview_pad);
        // Node-output monitor: title row + project-aspect body.
        let node_title_y = pane_y;
        let node_img_y = node_title_y + preview_title_h;
        pane_y = node_img_y + preview_h + preview_pad;
        // Master-out monitor: just below the node pane.
        let master_title_y = pane_y;
        let master_img_y = master_title_y + preview_title_h;
        let card_viewport = manifold_ui::Rect::new(card_x, 0.0, card_width, canvas_height);

        // The graph-editor panel is now just the node-output inspector + the
        // Smart-preview toggle (all param authoring moved onto the node face in
        // the canvas), so it needs only the preview state + the value-inspector
        // for a non-image node. `snap_arc` still feeds the value inspector's node
        // title lookup and the effect card below.
        let snap_arc = self.content_state.active_graph_snapshot.as_ref().cloned();
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

        // Right lane = the WHOLE main-window inspector column (master/layer/clip
        // tabs, every effect card, generator params, macros, chrome). Same panel
        // type as the main window, this window's own `ws.ui_root.inspector`
        // instance, driven by the same `Arc<Project>` snapshot — configured in the
        // main tick in lockstep with `self.ws`'s inspector, so here we only lay it
        // out into the right lane. Selecting a card retargets the canvas; edits
        // mirror both windows next snapshot. Replaces the single watched
        // `editor_card`. See docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md (Change 3).

        // Rebuild the editor's UITree from scratch each frame: tree state
        // is small, so a clear + rebuild is cheaper than dirty-tracking and
        // means stale rows can never linger after the target changes.
        ws.ui_root.tree.clear();
        ws.ui_root.build_inspector_in_rect(card_viewport);
        let _ = (card_x, card_width);

        // Pinned preview monitors in the left column: a backing panel, the two pane
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
            let title_x = preview_x;
            // Backing panel for the whole left column so the two centred monitors
            // sit on a clean, uniform surface rather than a black void. Stops at
            // the column height so it doesn't paint over the bottom strip.
            ws.ui_root.tree.add_panel(
                None,
                0.0,
                0.0,
                preview_width,
                canvas_height,
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
            // The "Smart preview" auto-gain toggle relocated here from the deleted
            // param sidebar: it belongs beside the node-output monitor it controls.
            // Only meaningful when a node-output IMAGE is on screen (a value-
            // inspector node has no gain to normalize), so draw it in the title row
            // alongside "Node Output". Capture its id so the input pass can hang the
            // SetNodePreviewNormalize intent on it.
            self.editor_smart_preview_toggle_id = None;
            if !inspector_drawn {
                ws.ui_root.tree.add_label(
                    None,
                    title_x,
                    node_title_y,
                    preview_w,
                    preview_title_h,
                    "Node Output",
                    title_style,
                );
                if show_image {
                    // Right-aligned toggle in the title row: "[✓] Smart preview".
                    let toggle_region = manifold_ui::Rect::new(
                        title_x + preview_w * 0.42,
                        node_title_y,
                        preview_w * 0.58,
                        preview_title_h,
                    );
                    let id = self
                        .graph_editor_panel
                        .render_smart_preview_toggle(&mut ws.ui_root.tree, toggle_region);
                    self.editor_smart_preview_toggle_id = Some(id);
                }
                if !show_image {
                    // Nothing selected — the image body stays empty; hint it.
                    ws.ui_root.tree.add_label(
                        None,
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
                None,
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

        // The editor rebuilt its whole tree above, clearing every node's flags.
        // Re-apply HOVERED / PRESSED from the input system's durable widget state
        // so interaction visuals (button hover/press) persist across the per-frame
        // rebuild instead of flickering off until the next pointer move.
        {
            let ui_root = &mut ws.ui_root;
            ui_root.input.apply_interaction_flags(&mut ui_root.tree);
        }

        // A pending jump-to-node centres now that `set_snapshot` has laid the
        // canvas out for this frame's scope (no-op until the target is present).
        if let Some(canvas) = self.graph_canvas.as_mut() {
            let vp = crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, canvas_height);
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
                crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, canvas_height),
            );
            // Layer the sidebar UITree on top of the canvas's immediate-mode
            // draws (the flush protocol covers them with their own batches).
            ui.render_tree_range(&ws.ui_root.tree, 0, usize::MAX);
            // Column dividers: a thin seam always, a highlight band on
            // hover/drag. Drawn after the panels so the seam reads on top of
            // both the canvas and the sidebar backgrounds.
            ws.dock.draw(editor_area, ui);
            // Bottom mini-timeline: scrub strip + clip minimap in the dock's
            // bottom band. Authoring-only — scrub to preview the graph at a
            // point, play to watch motion. Same `dock.bottom` the input pass
            // hit-tests for scrub/play.
            if ws.dock.show_bottom {
                manifold_ui::MiniTimeline::draw(
                    dock.bottom,
                    mini_total_beats,
                    mini_beats_per_bar,
                    mini_current_beat,
                    mini_rows,
                    &mini_clips,
                    &mini_layer_labels,
                    &mini_readout,
                    mini_is_playing,
                    ui,
                );
            }
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
                    // Each source is letterboxed into its 16:9 cell (transparent
                    // margins). The on-canvas screen now takes the project aspect,
                    // so sample only the letterboxed *content* sub-rect of the cell
                    // — otherwise a non-16:9 image would be squashed to fill the
                    // cell's full 16:9 extent. Centred fractions, computed once.
                    let cell_aspect = crate::content_pipeline::ATLAS_CELL_W as f32
                        / crate::content_pipeline::ATLAS_CELL_H as f32;
                    let (content_w_frac, content_h_frac) = if monitor_aspect > cell_aspect {
                        (1.0, cell_aspect / monitor_aspect)
                    } else {
                        (monitor_aspect / cell_aspect, 1.0)
                    };
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
                        // Narrow the cell UV to its centred content sub-rect so the
                        // project-aspect screen maps the image 1:1, not the cell's
                        // transparent letterbox margins.
                        let u0 = u0 + du * (1.0 - content_w_frac) * 0.5;
                        let v0 = v0 + dv * (1.0 - content_h_frac) * 0.5;
                        let du = du * content_w_frac;
                        let dv = dv * content_h_frac;
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
            let x = preview_x * scale;
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

        // UI frame profiler cursor (no-op unless MANIFOLD_UI_FRAME_PROFILE=1).
        let mut pseg = std::time::Instant::now();

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
        self.ui_profile.add("present.panel_cache", pseg.elapsed());
        pseg = std::time::Instant::now();

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
            self.ui_profile
                .add("present.fast_next_drawable", pseg.elapsed());
            pseg = std::time::Instant::now();
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
            self.ui_profile.add("present.fast_blit_present", pseg.elapsed());
            return;
        }
        self.ws.offscreen_dirty = false;

        // Reset overlay TextRenderer pool index
        if let Some(ui) = &mut self.ui_renderer {
            ui.begin_frame();
        }

        // ── Build the frame ──
        let mut encoder = gpu.device.create_encoder("Frame");
        pseg = std::time::Instant::now();

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

        self.ui_profile.add("present.clear_atlas_compositor", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Pass 4: timeline tracks, in z-ordered sub-passes (§24 5b) ──
        //   4a  grid bitmaps (per layer)              — under the clips
        //   4b  GPU clip bodies (rounded, gradient, lift)
        //   4b' GPU per-clip waveform textures        — inside the audio bodies
        //   4c  lane / stem / overview / group bitmaps — separate regions
        //   5   GPU overlays (region / cursor / markers) + clip names
        let layer_rects = self.ws.ui_root.viewport.layer_bitmap_rects();

        // Pass 4a: Per-layer grid bitmaps (grid + top separator), under the clip
        // bodies — opaque bodies occlude the grid, it shows through the gaps.
        if let Some(bg_gpu) = self.layer_bitmap_gpu.as_mut()
            && !layer_rects.is_empty()
        {
            bg_gpu.render_layers(
                &gpu.device,
                &mut encoder,
                offscreen,
                logical_w,
                logical_h,
                &layer_rects,
            );
        }

        self.ui_profile.add("present.pass4a_grid", pseg.elapsed());
        pseg = std::time::Instant::now();

        // Pass 4b: GPU clip bodies — rounded gradient tiles with a lift-on-select
        // shadow, in their own UIRenderer prepare/render cycle (reusing the shared
        // SDF rect pipeline). Emitted from the viewport's visible-clip list, so
        // only on-screen clips cost anything.
        if let Some(ui) = self.ui_renderer.as_mut() {
            self.ws
                .ui_root
                .viewport
                .visible_clip_rects(&mut self.clip_rect_scratch);
            if !self.clip_rect_scratch.is_empty() {
                // Resolve per-clip selection (incl. the marquee case: when the
                // region IS the selection, clips it covers style as selected —
                // same overlap test the bitmap path used, kept WYSIWYG).
                let region = self.ws.ui_root.viewport.selection_region_ref();
                let region_selects_clips =
                    region.is_some() && self.selection.selected_clip_ids.is_empty();
                let hovered = self.ws.ui_root.viewport.hovered_clip_id();
                self.clip_body_scratch.clear();
                // P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D15/D17): grab
                // lift + grid settle + error shake are pure X/Y offsets
                // applied to the SAME `ClipScreenRect` used below for
                // waveforms/thumbnails/names — mutating `cr.rect` in place
                // (rather than only the local `ClipBody`) keeps the whole
                // clip (body, waveform, label) moving together instead of
                // the body sliding out from under its own text.
                let lift_dy = -2.0 * self.overlay.lift_amount();
                let drag_dx = self.overlay.settle_dx_px() + self.overlay.error_shake_offset_px();
                let ghost_alpha = self.overlay.ghost_alpha();
                for cr in &mut self.clip_rect_scratch {
                    let in_marquee = region_selects_clips
                        && region.is_some_and(|r| {
                            manifold_ui::bitmap_renderer::clip_overlaps_region(
                                r,
                                cr.layer_index,
                                cr.start_beat.as_f32(),
                                cr.end_beat.as_f32(),
                            )
                        });
                    let selected = self.selection.is_selected(&cr.clip_id) || in_marquee;
                    let is_hovered = hovered == Some(cr.clip_id.as_str());
                    let is_drag_visual = self.overlay.is_drag_visual_target(&cr.clip_id);
                    if is_drag_visual {
                        cr.rect.x += drag_dx;
                        cr.rect.y += lift_dy;
                    }
                    // D17 "clip split flick": a brief 1px separation between
                    // the two just-split halves, independent of drag state.
                    cr.rect.x += self.ws.ui_root.viewport.split_flick_offset(&cr.clip_id);
                    self.clip_body_scratch
                        .push(manifold_renderer::clip_draw::ClipBody {
                            rect: cr.rect,
                            base_color: cr.base_color,
                            selected,
                            hovered: is_hovered,
                            muted: cr.is_muted,
                            locked: cr.is_locked,
                            generator: cr.is_generator,
                            alpha: if is_drag_visual { ghost_alpha } else { 1.0 },
                        });
                }

                let tracks = self.ws.ui_root.viewport.get_tracks_rect();
                ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
                manifold_renderer::clip_draw::emit_clips(ui, &self.clip_body_scratch);
                ui.pop_immediate_clip();
                if ui.prepare(&gpu.device, logical_w, logical_h, scale) {
                    ui.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
                }
            }
        }
        self.ui_profile
            .count("present.pass4b_clip_bodies", self.clip_rect_scratch.len() as u64);
        self.ui_profile.add("present.pass4b_clip_bodies", pseg.elapsed());
        pseg = std::time::Instant::now();

        // Pass 4b': Per-clip waveform textures, painted INSIDE the audio-clip
        // bodies (§24 5b). Reuses the visible-clip list resolved in 4b; only audio
        // clips with a decoded waveform cost anything, and a still timeline
        // re-uploads nothing (textures are cached per clip).
        if let Some(content_gpu) = self.clip_content_gpu.as_mut()
            && !self.clip_rect_scratch.is_empty()
        {
            let tracks = self.ws.ui_root.viewport.get_tracks_rect();
            content_gpu.render(
                &gpu.device,
                &mut encoder,
                offscreen,
                logical_w,
                logical_h,
                scale as f32,
                tracks,
                &self.clip_rect_scratch,
            );
        }
        self.ui_profile.add("present.pass4b_waveforms", pseg.elapsed());
        pseg = std::time::Instant::now();

        // Tell the content thread which clips want a thumbnail (non-audio,
        // wide enough), deduped so a stable view sends nothing. The content thread
        // snapshots those clips' live output into the shared atlas.
        {
            const MIN_THUMB_W: f32 = 24.0;
            let thumb_clips: Vec<manifold_core::ClipId> = self
                .clip_rect_scratch
                .iter()
                .filter(|cr| !cr.is_audio && cr.rect.width >= MIN_THUMB_W)
                .map(|cr| cr.clip_id.clone())
                .collect();
            if thumb_clips != self.last_clip_atlas_visible_sent {
                self.send_content_cmd(
                    crate::content_command::ContentCommand::SetClipAtlasVisible(thumb_clips.clone()),
                );
                self.last_clip_atlas_visible_sent = thumb_clips;
            }
        }

        // Pass 4b″: Clip thumbnails (§24 5c) — blit each visible generator/video
        // clip's atlas cell (published by the content thread) into its body, after
        // the waveform pass (audio clips draw waveforms, others draw thumbnails),
        // centre-cropped to the body aspect and masked to the rounded shape.
        #[cfg(target_os = "macos")]
        if !self.clip_rect_scratch.is_empty()
            && !self.content_state.clip_atlas_layout.is_empty()
        {
            let front = self
                .clip_atlas_texture_bridge
                .as_ref()
                .map(|b| b.front_index() as usize);
            if let Some(front) = front
                && let Some(atlas) = self.ui_clip_atlas_textures.get(front).and_then(|t| t.as_ref())
            {
                // clip → (filmstrip cell index → atlas cell), from the published
                // layout. Each clip tiles its captured bar cells across its body.
                let mut strips_of: ahash::AHashMap<&str, ahash::AHashMap<u32, u32>> =
                    ahash::AHashMap::new();
                for (cid, idx, cell) in &self.content_state.clip_atlas_layout {
                    strips_of.entry(cid.as_str()).or_default().insert(*idx, *cell);
                }
                let cell_aspect = crate::content_pipeline::CLIP_ATLAS_CELL_W as f32
                    / crate::content_pipeline::CLIP_ATLAS_CELL_H as f32;
                let inv_cols = 1.0 / crate::content_pipeline::CLIP_ATLAS_COLS as f32;
                let inv_rows = 1.0 / crate::content_pipeline::CLIP_ATLAS_ROWS as f32;
                let bpb = self.ws.ui_root.viewport.beats_per_bar() as f64;
                self.clip_thumb_quad_scratch.clear();
                // §F aspect-locked window scratch — reused across clips this frame
                // (cleared per clip; grows once), like `strips_of` above.
                let mut thumb_cells: Vec<(u32, f32)> = Vec::new();
                let mut thumb_windows: Vec<(u32, f32, f32)> = Vec::new();
                for cr in &self.clip_rect_scratch {
                    // Match the SetClipAtlasVisible filter so a clip too narrow to
                    // have requested a cell never draws one.
                    if cr.is_audio || cr.rect.width < 24.0 {
                        continue;
                    }
                    let Some(strip) = strips_of.get(cr.clip_id.as_str()) else {
                        continue;
                    };
                    // Reserve the bottom name-strip band: the thumbnail tiles only
                    // the PREVIEW area above it (mockup `.clip .body{bottom:16px}`),
                    // so the layer-coloured strip + name below are never covered.
                    // Same `clip_strip_height` the clip-body pass uses → they agree.
                    // Then inset by CLIP_THUMB_INSET on top/left/right (and leave the
                    // same gap above the strip) so the darker well frames the
                    // thumbnail as a dedicated panel instead of bleeding to the edge.
                    let strip_h = manifold_renderer::clip_draw::clip_strip_height(cr.rect.height)
                        .unwrap_or(0.0);
                    let m = manifold_ui::color::CLIP_THUMB_INSET;
                    let preview_h = (cr.rect.height - strip_h).max(1.0);
                    let body = manifold_ui::node::Rect::new(
                        cr.rect.x + m,
                        cr.rect.y + m,
                        (cr.rect.width - 2.0 * m).max(1.0),
                        (preview_h - 2.0 * m).max(1.0),
                    );
                    let body_right = body.x + body.width;
                    let start_b = cr.start_beat.as_f32() as f64;
                    let dur_b = (cr.end_beat - cr.start_beat).as_f32() as f64;
                    let count = crate::clip_filmstrip::cell_count(
                        crate::clip_filmstrip::clip_bar_count(dur_b, bpb),
                    );
                    // §F/§G: collect the captured cells with their on-screen start x,
                    // then lay a continuous grid of aspect-locked windows over the body,
                    // each filled by the nearest captured frame — gapless and regularly
                    // spaced even when only some bars have been swept/captured.
                    thumb_cells.clear();
                    for (&idx, &cell) in strip {
                        if idx >= count {
                            continue; // stale layout entry (clip shortened since capture)
                        }
                        let (sb, _eb) =
                            crate::clip_filmstrip::cell_beat_range(idx, start_b, dur_b, bpb);
                        thumb_cells.push((cell, self.ws.ui_root.viewport.beat_f64_to_pixel(sb)));
                    }
                    thumb_cells.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
                    // Window width = a project-aspect frame at the lane height, decoupled
                    // from bar width — the §F fix for the squished low-zoom filmstrip.
                    let win_w = body.height * cell_aspect;
                    crate::clip_filmstrip::grid_windows(
                        &thumb_cells,
                        body.x,
                        body_right,
                        win_w,
                        &mut thumb_windows,
                    );
                    for &(cell, x0, w) in &thumb_windows {
                        let sub = manifold_ui::node::Rect::new(x0, body.y, w, body.height);
                        // Atlas cell UV in the non-square COLS×ROWS grid.
                        let gx = (cell % crate::content_pipeline::CLIP_ATLAS_COLS) as f32;
                        let gy = (cell / crate::content_pipeline::CLIP_ATLAS_COLS) as f32;
                        let (u0, v0) = (gx * inv_cols, gy * inv_rows);
                        let (u1, v1) = (u0 + inv_cols, v0 + inv_rows);
                        // A full aspect-locked window shows the whole frame (no crop);
                        // only a clamped partial last window is centre-cropped.
                        let sub_aspect = (w / body.height.max(1.0)).max(0.01);
                        let (uu0, vv0, uu1, vv1) = if sub_aspect >= cell_aspect {
                            let f = cell_aspect / sub_aspect; // crop height
                            let vc = (v0 + v1) * 0.5;
                            let h = (v1 - v0) * f * 0.5;
                            (u0, vc - h, u1, vc + h)
                        } else {
                            let f = sub_aspect / cell_aspect; // crop width
                            let uc = (u0 + u1) * 0.5;
                            let cw = (u1 - u0) * f * 0.5;
                            (uc - cw, v0, uc + cw, v1)
                        };
                        self.clip_thumb_quad_scratch.push(
                            manifold_renderer::clip_thumb_gpu::ThumbQuad {
                                rect: sub,
                                body_rect: body,
                                radius: manifold_ui::color::CLIP_RADIUS,
                                uv_min: [uu0, vv0],
                                uv_max: [uu1, vv1],
                            },
                        );
                    }
                }
                let tracks = self.ws.ui_root.viewport.get_tracks_rect();
                if !self.clip_thumb_quad_scratch.is_empty()
                    && let Some(thumb) = self.clip_thumb_gpu.as_mut()
                {
                    thumb.render(
                        &gpu.device,
                        &mut encoder,
                        offscreen,
                        logical_w,
                        logical_h,
                        scale as f32,
                        tracks,
                        atlas,
                        &self.clip_thumb_quad_scratch,
                    );
                }
            }
        }
        self.ui_profile
            .count("present.pass4b_thumbnails", self.clip_thumb_quad_scratch.len() as u64);
        self.ui_profile.add("present.pass4b_thumbnails", pseg.elapsed());
        pseg = std::time::Instant::now();

        // Pass 4c: The lane / stem / overview / collapsed-group panel bitmaps.
        // These are separate regions (below / beside the tracks, or collapsed group
        // rows that carry no clips), so their z-order vs the clips is moot — they
        // ride the same single layer-bitmap instance as the grid, indices 1000+.
        if let Some(bitmap_gpu) = self.layer_bitmap_gpu.as_mut() {
            let mut rects: Vec<(usize, manifold_ui::node::Rect)> = Vec::new();

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

        self.ui_profile.add("present.pass4c_panels", pseg.elapsed());
        pseg = std::time::Instant::now();

        // Timeline overlays (region highlight / insert cursor / beat markers) as
        // GPU rects (§24 5b — no longer baked into a per-layer bitmap). Resolved
        // here while `self` is free, then drawn inside the Pass 5 UIRenderer cycle
        // under the clip names. The insert cursor's layer comes from the app's
        // selection (it owns the resolved layer id).
        let overlay_tracks = self.ws.ui_root.viewport.get_tracks_rect();
        let insert_layer = self
            .selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        let timeline_overlays = self.ws.ui_root.viewport.timeline_overlays(
            insert_layer,
            self.selection.has_insert_cursor(),
            &mut self.timeline_marker_scratch,
        );

        // Pass 5: Overlay UI (playhead, HUD, dropdowns, text). Its own cycle —
        // begin_frame here resets the text pool that the Pass 4b clip cycle did
        // not touch, isolating the two prepare/render cycles cleanly.
        if let Some(ui) = &mut self.ui_renderer {
            ui.begin_frame();

            // Region / cursor / markers, scissored to the tracks rect, UNDER the
            // clip names (bottom→top: region, cursor, markers — matches the old
            // bitmap paint order). All sit on top of the clip bodies + waveforms.
            ui.push_immediate_clip(
                overlay_tracks.x,
                overlay_tracks.y,
                overlay_tracks.width,
                overlay_tracks.height,
            );
            if let Some((r, c)) = timeline_overlays.region {
                ui.draw_rect(r.x, r.y, r.width, r.height, c);
            }
            if let Some((r, c)) = timeline_overlays.cursor {
                ui.draw_rect(r.x, r.y, r.width, r.height, c);
            }
            for (x, c) in &self.timeline_marker_scratch {
                ui.draw_rect(*x, overlay_tracks.y, 1.0, overlay_tracks.height, *c);
            }
            // D15 "landing-line flash" — a brief vertical line at the beat a
            // move-drag just snapped to, spanning the dragged selection's
            // layer range. Fades out over the `Transient`'s own duration;
            // purely visual (the model already snapped in
            // `InteractionOverlay::finalize_move_snap`).
            if let Some((progress, beat, min_layer, max_layer)) = self.overlay.landing_flash() {
                let x = self.ws.ui_root.viewport.beat_to_pixel(beat);
                let y0 = self.ws.ui_root.viewport.track_y(min_layer);
                let y1 = self.ws.ui_root.viewport.track_y(max_layer)
                    + self.ws.ui_root.viewport.track_height(max_layer);
                if y1 > y0 {
                    let alpha = ((1.0 - progress) * 230.0).round().clamp(0.0, 255.0) as u8;
                    let c = manifold_ui::color::with_alpha(manifold_ui::color::INSERT_CURSOR_BLUE, alpha);
                    ui.draw_rect(x - 1.0, y0, 2.0, y1 - y0, c);
                }
            }
            ui.pop_immediate_clip();

            // Clip name labels (§24 5b) — on top of the bodies + waveforms, at
            // BASE depth (under the dropdown/modal overlays). Reuses the visible
            // clip list resolved for the Pass 4b body emission this frame.
            manifold_renderer::clip_draw::emit_clip_names(
                ui,
                &self.clip_rect_scratch,
                overlay_tracks,
            );

            // Automation lane strips (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) —
            // on top of the clip names, same overlay pass. Empty whenever
            // automation mode is off (the viewport never populated any lanes
            // this frame), so this is a no-op cost in the common case.
            let automation_lanes = self
                .ws
                .ui_root
                .viewport
                .automation_lane_screens(&self.content_state.automation_latched_params);
            manifold_renderer::automation_lane_draw::emit_automation_lanes(
                ui,
                &automation_lanes,
                overlay_tracks,
            );

            // Playhead — a red line spanning ruler + tracks, capped by a downward
            // triangle head at the top of the ruler (§24 5e). The head is the
            // single dominant "now" marker so it never competes with the blue
            // insert cursor for "where am I".
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
                    manifold_ui::color::PLAYHEAD_RED,
                );
                let s = manifold_ui::color::PLAYHEAD_HEAD_SIZE;
                ui.draw_icon(
                    manifold_ui::icons::Icon::Playhead.id(),
                    px - s * 0.5,
                    top,
                    s,
                    s,
                    manifold_ui::color::PLAYHEAD_RED,
                    None,
                );
            }

            // Horizontal scrollbar (§24 5e): a slim track + draggable thumb in the
            // reserved strip below the tracks. Geometry comes from the viewport —
            // the same source the drag hit-test uses — so the drawn thumb and the
            // grabbable region can't drift. Drawn here as GPU rects, like the
            // playhead. Hidden (None) when the whole timeline fits.
            if let Some((track, thumb)) = self.ws.ui_root.viewport.scrollbar_h_layout() {
                ui.draw_rect(
                    track.x,
                    track.y,
                    track.width,
                    track.height,
                    manifold_ui::color::SCROLLBAR_TRACK_C32,
                );
                let active = self.ws.ui_root.viewport.scrollbar_h_dragging()
                    || thumb.contains(self.cursor_pos);
                let thumb_color = if active {
                    manifold_ui::color::SCROLLBAR_THUMB_HOVER_C32
                } else {
                    manifold_ui::color::SCROLLBAR_THUMB_C32
                };
                let radius = (thumb.height * 0.5).min(thumb.width * 0.5);
                ui.draw_rounded_rect(
                    thumb.x,
                    thumb.y,
                    thumb.width,
                    thumb.height,
                    thumb_color,
                    radius,
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
                // Soft drop-shadow under the floating panel (§17). Drawn first
                // so it sits under the panel's own fill at this depth. Skip a
                // leading full-screen scrim (dim-modal backdrop) so the shadow
                // lifts the panel, not the whole screen.
                if start < end {
                    let tree = &self.ws.ui_root.tree;
                    let mut r = tree.get_bounds(tree.id_at(start));
                    if r.width >= logical_w as f32 - 1.0
                        && r.height >= logical_h as f32 - 1.0
                        && start + 1 < end
                    {
                        r = tree.get_bounds(tree.id_at(start + 1));
                    }
                    ui.draw_shadow(
                        r.x,
                        r.y + manifold_ui::color::SHADOW_OFFSET_Y,
                        r.width,
                        r.height,
                        manifold_ui::color::POPUP_RADIUS,
                        manifold_ui::color::SHADOW_BLUR,
                        manifold_ui::color::SHADOW,
                    );
                }
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

        // Audio Setup spectrogram waterfall. Drawn AFTER the overlay flush so it
        // lands on top of the modal's scope-area background (the modal is an
        // overlay, drawn into `offscreen` just above). Render the live VQT
        // columns into a UI-device texture, then blit it into the panel's
        // reserved scope rect via the unified TexturePane path.
        //
        // Suppressed while a dropdown is open: dropdowns (device / channel) are
        // overlays that expand down over the scope, and this blit lands on top of
        // them — so painting the waterfall would hide the open list. The scope
        // briefly shows its dark background instead, and returns when it closes.
        #[cfg(target_os = "macos")]
        if self.ws.ui_root.audio_setup_panel.is_open()
            && !self.ws.ui_root.dropdown.is_open()
            && self.content_state.spectrogram_num_bins > 0
            && let Some(rect) = self.ws.ui_root.audio_setup_panel.scope_rect()
            && let (Some(blit_pipeline), Some(blit_sampler)) =
                (&self.blit_pipeline, &self.blit_sampler)
        {
            use manifold_spectral::{Spectrogram, SpectrogramConfig};
            let num_bins = self.content_state.spectrogram_num_bins;
            let cfg = SpectrogramConfig::default();

            // Render the waterfall at the scope's physical-pixel size so it stays
            // crisp at any (resizable) modal size — the shader supersamples the
            // column ring directly to display resolution. Clamped to bound VRAM.
            let tex_w = ((rect.width * sf).round() as u32).clamp(256, 4096);
            let tex_h = ((rect.height * sf).round() as u32).clamp(128, 4096);

            // (Re)create the renderer if the bin count or the on-screen width
            // changed. The ring is sized to the texture's pixel width so each
            // column owns one pixel (crisp 1:1 sweep, no resampling).
            let cur_cols = self.spectrogram.as_ref().map(|s| s.num_cols());
            if cur_cols != Some(tex_w as usize) || self.spectrogram_num_bins != num_bins {
                // Drop buffered columns — chunking them at the new `num_bins`
                // would misalign, and a width change resets the sweep anyway.
                self.pending_spectrogram_columns.clear();
                self.spectrogram = Some(Spectrogram::new(
                    &gpu.device,
                    num_bins,
                    tex_w as usize,
                    manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    cfg.db_min,
                    cfg.db_max,
                    cfg.tilt_slope,
                ));
                self.spectrogram_num_bins = num_bins;
            }
            // (Re)create the target pane when the scope's pixel size changes
            // (modal resize) or it doesn't exist yet.
            if self.spectrogram_pane.is_none() || self.spectrogram_tex_dims != (tex_w, tex_h) {
                let tex = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: tex_w,
                    height: tex_h,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                    label: "Audio Scope",
                    mip_levels: 1,
                });
                self.spectrogram_pane = Some(crate::texture_pane::TexturePane::local(tex));
                self.spectrogram_tex_dims = (tex_w, tex_h);
            }

            // Cursor frequency-line position (uv.y), computed before the mutable
            // spectrogram borrow below. Negative = not hovering.
            let scope_cursor_y = self.scope_hover_uv().map_or(-1.0, |(_, uy, _)| uy);
            if let (Some(spectrogram), Some(pane)) =
                (self.spectrogram.as_mut(), self.spectrogram_pane.as_mut())
                && let Some(target) = pane.local_target().cloned()
            {
                // Feed new columns (post-gain magnitudes from the worker), each
                // exactly once, then clear — see `pending_spectrogram_columns`.
                // The overlay scalars ride in lockstep (7 per column: four per-band
                // centroids + three onsets); a column with no matching record
                // (shouldn't happen) gets the hide sentinel.
                let mut scalars = self.pending_spectrogram_scalars.chunks_exact(7);
                for col in self.pending_spectrogram_columns.chunks_exact(num_bins) {
                    let (centroids, onsets) = match scalars.next() {
                        Some(s) => ([s[0], s[1], s[2], s[3]], [s[4], s[5], s[6]]),
                        None => ([-1.0; 4], [0.0; 3]),
                    };
                    spectrogram.push_column(col, centroids, onsets);
                }
                self.pending_spectrogram_columns.clear();
                self.pending_spectrogram_scalars.clear();
                // Band dividers at the editable low/mid + mid/high crossovers (the
                // modulation's Low/Mid/High splits), as normalised y on the log
                // axis. Drag-retuned live via the Audio Setup scope.
                let (fmin, fmax) =
                    (self.content_state.spectrogram_fmin, self.content_state.spectrogram_fmax);
                let y_of = |f: f32| {
                    if fmin > 0.0 && fmax > fmin {
                        (f / fmin).log2() / (fmax / fmin).log2()
                    } else {
                        -1.0
                    }
                };
                // Octave span of the displayed range — drives the pink tilt
                // (centred on the geometric-mean freq). 0 disables it.
                let freq_log_ratio =
                    if fmin > 0.0 && fmax > fmin { (fmax / fmin).log2() } else { 0.0 };
                let lo_yfb = y_of(self.content_state.spectrogram_low_hz);
                let hi_yfb = y_of(self.content_state.spectrogram_mid_hz);
                // Which divider the cursor is over, for the grip-handle hover
                // glow. Use the panel's OWN hit-test (same px tolerance the grab
                // uses) so the glow and the grab zone match exactly — converting
                // the cursor's uv.y back to a screen y. Off-scope cursor (< 0)
                // maps far away → no hover.
                let cursor_screen_y = if scope_cursor_y < 0.0 {
                    -1.0e9
                } else {
                    rect.y + scope_cursor_y * rect.height
                };
                let hovered_divider = self
                    .ws
                    .ui_root
                    .audio_setup_panel
                    .divider_hover_index(cursor_screen_y);
                spectrogram.render(
                    &mut encoder,
                    &target,
                    [lo_yfb, hi_yfb],
                    freq_log_ratio,
                    scope_cursor_y,
                    hovered_divider,
                );

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

        self.ui_profile.add("present.pass5_overlay", pseg.elapsed());
        pseg = std::time::Instant::now();

        // ── Commit offscreen render ──
        // Capture the offscreen buffer's TRUE GPU execution time async — tells
        // us whether next_drawable's block is our own GPU work (heavy offscreen)
        // or external starvation (content thread hogging the shared GPU).
        if let Some((sink_us, sink_n)) = self.ui_profile.gpu_sink() {
            encoder.add_gpu_time_handler(move |secs| {
                sink_us.fetch_add((secs * 1_000_000.0) as u64, std::sync::atomic::Ordering::Relaxed);
                sink_n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
        encoder.commit();
        self.ui_profile.add("present.commit", pseg.elapsed());
        pseg = std::time::Instant::now();

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
        self.ui_profile.add("present.next_drawable", pseg.elapsed());
        pseg = std::time::Instant::now();

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
        self.ui_profile.add("present.blit_present", pseg.elapsed());
    }
}

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

/// Format the audio scope's hover readout: frequency (kHz above 1 kHz, else Hz)
/// and the raw level in dB, e.g. `4.17 kHz   -17.9 dB`.
fn format_scope_readout(freq: f32, db: f32) -> String {
    let f = if freq >= 1000.0 {
        format!("{:.2} kHz", freq / 1000.0)
    } else {
        format!("{freq:.0} Hz")
    };
    format!("{f}   {db:.1} dB")
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
        manifold_ui::Color32::new(89, 115, 179, 204), // sRGB, was [0.35, 0.45, 0.7, 0.8]
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
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
    selected: u32,
) -> Option<manifold_core::NodeId> {
    let (nodes, wires) =
        crate::graph_canvas::resolve_level(snap, scope).unwrap_or((&snap.nodes, &snap.wires));
    let node = nodes.iter().find(|n| n.id == selected)?;
    resolve_boundary_node(node, nodes, wires, snap, scope)
}

fn resolve_boundary_node(
    node: &manifold_ui::graph_view::NodeSnapshot,
    nodes: &[manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
    snap: &manifold_ui::graph_view::GraphSnapshot,
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
    producer: &manifold_ui::graph_view::NodeSnapshot,
    nodes: &[manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
    snap: &manifold_ui::graph_view::GraphSnapshot,
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
    nodes: &'a [manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
) -> Option<&'a manifold_ui::graph_view::NodeSnapshot> {
    let w = wires
        .iter()
        .find(|w| w.to_node == to_node && w.to_port == to_port)
        .or_else(|| wires.iter().find(|w| w.to_node == to_node))?;
    nodes.iter().find(|n| n.id == w.from_node)
}

/// First `Texture2D`(-typed) port name, else the first port of any type.
fn primary_texture_port(ports: &[manifold_ui::graph_view::PortSnapshot]) -> Option<&str> {
    use manifold_ui::graph_view::PortKindSnapshot;
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

// The `build_card_exposures` / `build_outer_driven_map` / `build_wire_driven_keys`
// / `build_static_block_targets` joins that fed the deleted inner-node param
// sidebar are gone: the canvas now derives exposed / wire-driven / outer-driven
// state itself from the snapshot (see `GraphCanvas::apply_driven_state`), and the
// per-node expose checkbox lives on the node face.




#[cfg(test)]
mod preview_target_tests {
    use super::resolve_preview_target;
    use manifold_core::NodeId;
    use manifold_ui::graph_view::{
        GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GraphSnapshot, GroupSnapshot,
        NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
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
            category: manifold_ui::graph_view::Category::Uncategorized,
            tooltip: None,
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
