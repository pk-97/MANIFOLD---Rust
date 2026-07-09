//! State pushed from the content thread to the UI thread each content frame.
//!
//! The UI thread reads the latest ContentState to display transport info,
//! project data version, and other engine state without accessing the
//! PlaybackEngine or EditingService directly.

use manifold_core::effects::ParamId;
use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, LayerType, OscSyncMode};
use manifold_core::{Beats, EffectId, Seconds};
use std::sync::Arc;

/// Live state of the editor's node-output preview, pushed each frame so the
/// UI can show a value inspector for non-image nodes (control / math /
/// envelope) instead of a black pane. Present only while a node is previewed.
#[derive(Clone, Debug)]
pub struct NodePreviewInfo {
    /// The previewed node's stable id — lets the UI confirm it matches the
    /// current selection before showing the inspector.
    pub node_id: manifold_core::NodeId,
    /// True if the node produced a Texture2D output (the image pane is shown);
    /// false → the UI shows the value inspector built from `inputs`/`outputs`.
    pub has_image: bool,
    /// Live scalar input port values this frame (`port_name`, value).
    pub inputs: Vec<(String, f32)>,
    /// Live scalar output port values — the signal the node is producing.
    pub outputs: Vec<(String, f32)>,
}

/// Sent once when an export finishes. Consumed by `push_state`
/// (`ui_bridge/state_sync.rs`) to fire the D17 export-complete toast
/// (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — no longer dead code as of that wiring.
#[derive(Clone, Debug)]
pub struct ExportFinishedEvent {
    pub success: bool,
    pub message: String,
    pub output_path: String,
}

/// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — the real command
/// description, so the toast reads "Undo: Move Clip" instead of a generic
/// "Undo". Unlike `ExportFinishedEvent` (a rare, out-of-band send from a
/// blocking export thread), undo/redo run inline on every normal content-tick
/// loop iteration, so this rides the REGULAR per-tick `ContentState` build
/// instead of a separate degraded-snapshot message — see
/// `ContentThread::pending_undo_redo_event` (content_thread.rs) and its
/// `.take()` at the state-build site. `content_commands.rs`'s `Undo`/`Redo`
/// handlers populate it by peeking `EditingService::peek_undo_description` /
/// `peek_redo_description` BEFORE calling `undo`/`redo` (the command moves
/// stacks once acted on).
#[derive(Clone, Debug)]
pub struct UndoRedoEvent {
    pub is_redo: bool,
    pub description: String,
}

/// State snapshot sent from the content thread to the UI thread.
/// The UI thread drains these from a bounded channel and uses the latest.
///
/// Orphan enforcement (UI_PROJECTION_LAYER_DESIGN.md P0): manifold-app is a
/// bin crate, so rustc's dead_code lint sees every read site — a field written
/// by the content thread but never read on the UI side fails
/// `cargo clippy -- -D warnings`. Never re-add `#[allow(dead_code)]` here;
/// delete the orphan field (and its emit write) instead.
#[derive(Clone)]
pub struct ContentState {
    // ── Transport ──────────────────────────────────────────────────
    pub current_beat: Beats,
    pub current_time: Seconds,
    pub is_playing: bool,
    pub is_recording: bool,

    // ── Content thread perf ─────────────────────────────────────
    pub content_fps: f32,
    pub content_frame_time_ms: f32,
    /// Time spent waiting for a GPU surface (ms). Non-zero = GPU saturation.
    pub gpu_fence_wait_ms: f32,
    pub active_clips: usize,

    // ── Editing ────────────────────────────────────────────────────
    pub data_version: u64,
    pub editing_is_dirty: bool,

    // ── Project settings (from authoritative project) ─────────────
    pub bpm: f64,
    pub frame_rate: f64,
    pub clock_authority: ClockAuthority,
    pub time_signature_numerator: i32,

    // ── Transport controller state ────────────────────────────────
    pub link_enabled: bool,
    pub link_peers: i32,
    pub midi_clock_enabled: bool,
    pub midi_clock_position_display: Arc<str>,
    pub midi_clock_receiving: bool,
    pub midi_clock_device_name: Arc<str>,
    /// Available MIDI input device names for the CLK device dropdown.
    pub midi_device_names: Arc<[String]>,
    /// Live per-send audio levels (RMS amplitude 0..1), indexed by send order.
    /// Fixed-size + count so it rides the snapshot with no per-frame alloc.
    pub audio_send_levels: [f32; manifold_audio::analysis::MAX_SENDS],
    /// Number of valid entries in [`Self::audio_send_levels`].
    pub audio_send_count: usize,
    /// New VQT spectrogram columns produced since the last snapshot, flattened
    /// (`k * spectrogram_num_bins` magnitudes, oldest → newest). Empty unless the
    /// Audio Setup scope is open on a send.
    pub spectrogram_columns: Vec<f32>,
    /// Per-column overlay records in lockstep with `spectrogram_columns` — one
    /// [`manifold_spectral::ScopeColumn`] (the four scrolling per-band centroid
    /// traces + the onset tick lanes) per column. Length is
    /// `columns / spectrogram_num_bins`.
    pub spectrogram_col_scalars: Vec<manifold_spectral::ScopeColumn>,
    /// Bins per spectrogram column (column length). 0 = no scope.
    pub spectrogram_num_bins: usize,
    /// Analysed frequency range of the scope (Hz), for axis + band overlays.
    pub spectrogram_fmin: f32,
    pub spectrogram_fmax: f32,
    /// Low/mid and mid/high crossover frequencies (Hz) — the editable band
    /// dividers drawn on the spectrogram and used to position the per-band
    /// meters. Mirror `project.audio_setup.{low_hz,mid_hz}`.
    pub spectrogram_low_hz: f32,
    pub spectrogram_mid_hz: f32,
    /// The tapped (scope-selected) send's latest features, for the spectrogram's
    /// per-band level meters. `None` when no send feeds the scope.
    pub spectrogram_features: Option<manifold_core::SendFeatures>,
    pub osc_sender_enabled: bool,

    // ── Percussion status ─────────────────────────────────────────
    pub percussion_importing: bool,
    pub percussion_status_message: Arc<str>,
    pub percussion_progress: f32,
    pub percussion_show_progress: bool,

    // ── Profiling ────────────────────────────────────────────────
    /// Whether a profiling session is currently recording.
    pub profiling_active: bool,
    /// Number of frames recorded in the current session.
    pub profiling_frame_count: u64,

    // ── LED output ────────────────────────────────────────────────
    /// Whether LED output is enabled.
    pub led_enabled: bool,

    // ── Live Recording ─────────────────────────────────────────────
    /// Whether a live recording is currently in progress.
    pub is_live_recording: bool,

    // ── Export ────────────────────────────────────────────────────
    // (An export *progress* display never existed — the old is_exporting /
    // export_progress / export_status fields were emitted into a void and were
    // deleted 2026-07-09; BUG-083 tracks building the display. Same for the
    // recording drop counter, BUG-084.)
    /// Set once when export finishes (success or failure).
    pub export_finished: Option<ExportFinishedEvent>,

    // ── Undo/redo ─────────────────────────────────────────────────
    /// Set on the tick an undo/redo actually happened; `None` every other
    /// tick. D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — see
    /// [`UndoRedoEvent`]'s doc comment for why this differs from
    /// `export_finished`'s out-of-band pattern.
    pub undo_redo_event: Option<UndoRedoEvent>,

    // ── Ableton bridge ──────────────────────────────────────────
    /// Ableton session data for UI dropdown population.
    pub ableton_session: Option<Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Whether the Ableton bridge is currently connected.
    pub ableton_connected: bool,
    pub ableton_transport_enabled: bool,
    /// Closed-loop transport sync health (Locked / Confirming / degraded /
    /// warning) — drives the SYNC chip (ABLETON_TRANSPORT_SYNC_DESIGN D9/D10).
    pub ableton_sync_status: manifold_playback::transport_sync::TransportSyncStatus,
    pub osc_sync_mode: OscSyncMode,

    // ── Project snapshot ──────────────────────────────────────────
    /// Sent when data_version changes so the UI thread can update its
    /// local_project. Only created on structural changes (editing commands,
    /// undo/redo) — never on modulation-only frames.
    pub project_snapshot: Option<Arc<Project>>,

    /// Lightweight modulation delta — just the param_values that
    /// drivers/envelopes wrote this frame. Applied in-place to the UI's
    /// local_project without a full Project clone.
    pub modulation_snapshot: Option<ModulationSnapshot>,

    /// Live snapshot of the first graph-backed effect's internal node
    /// graph, for the editor canvas. `None` when no graph-backed effect
    /// has run yet, or when the editor window isn't open. Wrapped in
    /// `Arc` so cloning the `ContentState` per snapshot is cheap.
    pub active_graph_snapshot: Option<Arc<manifold_renderer::node_graph::GraphSnapshot>>,

    /// Live node-output preview state for the editor's value inspector. `None`
    /// when no node is being previewed. See [`NodePreviewInfo`].
    pub node_preview_info: Option<NodePreviewInfo>,

    /// Live (post-modulation) scalar param values for every node of the watched
    /// effect/generator this frame, keyed by stable `NodeId`. The editor canvas
    /// overlays these onto its node faces so a driver / Ableton / envelope / card
    /// slider is *seen* moving the knob, instead of the frozen authoring def the
    /// `graph_version`-cached `active_graph_snapshot` carries. Empty whenever no
    /// editor is watching. Param names are `&'static`, so this allocates only the
    /// small per-node `Vec`s.
    pub live_node_params: manifold_renderer::node_graph::LiveNodeParams,
    /// `(node_id, atlas_cell_index)` for the per-node thumbnail atlas captured
    /// this frame. The editor canvas maps each visible node to its atlas cell to
    /// blit the thumbnail. Empty unless the editor enabled the atlas.
    pub node_atlas_layout: Vec<(manifold_core::NodeId, u32)>,
    /// `(clip_id, filmstrip_cell_index, atlas_cell_index)` for the timeline
    /// clip-thumbnail **filmstrip** atlas (§24 5c-2). Each clip owns one entry per
    /// captured filmstrip cell (bar / bar-group); the timeline tiles them across the
    /// clip body. Empty when no clips currently hold a thumbnail.
    pub clip_atlas_layout: Vec<(manifold_core::ClipId, u32, u32)>,

    /// Automation-lane override latches active this frame — runtime-only
    /// state (`manifold_playback::automation::AutomationLatches`, owned by
    /// `PlaybackEngine`, never part of `Project`/serialized; see
    /// `docs/AUTOMATION_LANES_DESIGN.md` §4). Copied each tick from
    /// `PlaybackEngine::automation_latches()`, mirroring how other runtime-only
    /// playback state (e.g. `audio_send_levels`) reaches the UI thread — lane
    /// *data* itself needs no such copy since it lives on `PresetInstance` and
    /// already rides `project_snapshot` for free. Empty means nothing is
    /// overridden (the "Back to Arrangement" affordance is unlit); a
    /// non-empty entry `(EffectId, ParamId)` names one overridden lane, for
    /// per-lane graying. Lane-editing UI (P4) is the consumer; this phase
    /// only wires the data through.
    pub automation_latched_params: Vec<(EffectId, ParamId)>,

    /// Global Automation Arm state (§5) — runtime-only, owned by
    /// `PlaybackEngine`, copied each tick from `PlaybackEngine::
    /// automation_armed()` exactly like `automation_latched_params` above.
    /// Drives the transport-bar arm button's lit/unlit state (P4).
    pub automation_armed: bool,
}

/// Lightweight snapshot of modulated param values.
/// Captures only `param_values` from effects and generator params,
/// avoiding a full `Project::clone()` on every modulation frame.
///
/// All float values are packed into a single flat buffer (`values`).
/// A parallel `block_lens` array records the length of each param block.
/// Layout order: macros, master effects, then per-layer (effects, gen).
/// Clone = 4 Vec allocations (values + block_lens + block_topos + layer_shapes)
/// instead of ~128 separate Vec<f32> clones.
#[derive(Clone)]
pub struct ModulationSnapshot {
    /// All param values concatenated: macros | master_fx0 | master_fx1 | ...
    /// | layer0_fx0 | layer0_fx1 | ... | layer0_gen | layer1_fx0 | ...
    values: Vec<f32>,
    /// Length of each param block in `values`, in the same order.
    /// block 0 = macros, blocks 1..1+master_count = master effects,
    /// remaining blocks = per-layer effects and gen params (see layer_shapes).
    block_lens: Vec<u16>,
    /// `ParamManifest::topology` for each block at capture time, parallel to
    /// `block_lens` (D8). Apply skips any block whose live manifest topology no
    /// longer matches the captured stamp — this catches same-length param
    /// reorders, which a `len == len` check silently misroutes. The macro block
    /// (index 0) has no manifest; its slot holds a sentinel `0` and is guarded
    /// per-slot instead.
    block_topos: Vec<u32>,
    /// Number of master effect blocks (immediately after the macro block).
    master_count: u16,
    /// Per-layer: (effect_count, has_gen_params). Determines how many
    /// blocks in `block_lens` belong to each layer.
    layer_shapes: Vec<(u16, bool)>,
}

impl ModulationSnapshot {
    /// Create an empty snapshot (used as scratch buffer on content thread).
    pub fn empty() -> Self {
        Self {
            values: Vec::new(),
            block_lens: Vec::new(),
            block_topos: Vec::new(),
            master_count: 0,
            layer_shapes: Vec::new(),
        }
    }

    /// Fill this snapshot from the project, reusing existing vec capacity.
    /// Called each frame on the content thread's scratch instance — zero
    /// allocation after the first frame (vecs grow once, never shrink).
    pub fn capture_into(&mut self, project: &Project) {
        self.values.clear();
        self.block_lens.clear();
        self.block_topos.clear();
        self.layer_shapes.clear();

        // Macros (block 0) — no ParamManifest; guarded per-slot on apply, so
        // its topology slot is a sentinel that's never consulted.
        let macro_count = project.settings.macro_bank.slots.len();
        for slot in &project.settings.macro_bank.slots {
            self.values.push(slot.value);
        }
        self.block_lens.push(macro_count as u16);
        self.block_topos.push(0);

        // Master effects — pack only `.value` from each ParamSlot;
        // exposure isn't a modulation concern and must not round-trip
        // through the per-frame fast path.
        self.master_count = project.settings.master_effects.len() as u16;
        for fx in &project.settings.master_effects {
            let len = fx.params.len();
            self.values.extend(fx.params.iter().map(|p| p.value));
            self.block_lens.push(len as u16);
            self.block_topos.push(fx.params.topology());
        }

        // Per-layer: effects + optional gen params
        for layer in &project.timeline.layers {
            let effect_count = layer.effects.as_ref().map_or(0, |effects| effects.len());
            let has_gen = layer.layer_type == LayerType::Generator && layer.gen_params().is_some();

            if let Some(effects) = &layer.effects {
                for fx in effects {
                    let len = fx.params.len();
                    self.values.extend(fx.params.iter().map(|p| p.value));
                    self.block_lens.push(len as u16);
                    self.block_topos.push(fx.params.topology());
                }
            }

            if has_gen && let Some(gp) = layer.gen_params() {
                let len = gp.params.len();
                self.values.extend(gp.params.iter().map(|p| p.value));
                self.block_lens.push(len as u16);
                self.block_topos.push(gp.params.topology());
            }

            self.layer_shapes.push((effect_count as u16, has_gen));
        }
    }

    /// Apply modulated values to a project in-place. Overwrites only
    /// `param_values` — no structural changes, no allocations if lengths match.
    pub fn apply(&self, project: &mut Project) {
        let mut cursor = 0usize; // position in values
        let mut block = 0usize; // position in block_lens

        // Macros (block 0)
        let macro_len = *self.block_lens.get(block).unwrap_or(&0) as usize;
        for i in 0..macro_len {
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(i) {
                slot.value = self.values[cursor + i];
            }
        }
        cursor += macro_len;
        block += 1;

        // Master effects — write only `.value` per slot; the `.exposed`
        // flag is host-state, not modulation-state.
        for i in 0..self.master_count as usize {
            let len = *self.block_lens.get(block).unwrap_or(&0) as usize;
            if let Some(fx) = project.settings.master_effects.get_mut(i)
                && fx.params.topology() == self.block_topos.get(block).copied().unwrap_or(u32::MAX)
            {
                for (slot, &v) in fx
                    .params
                    .iter_mut()
                    .zip(&self.values[cursor..cursor + len])
                {
                    slot.value = v;
                }
            }
            cursor += len;
            block += 1;
        }

        // Layer effects + generator params
        for (i, &(effect_count, has_gen)) in self.layer_shapes.iter().enumerate() {
            // Advance cursor/block for every effect block regardless of
            // whether the layer/effect exists on the UI side. The flat buffer
            // layout is authoritative — skipping blocks without advancing
            // would desync all subsequent data.
            if let Some(layer) = project.timeline.layers.get_mut(i) {
                for j in 0..effect_count as usize {
                    let len = *self.block_lens.get(block).unwrap_or(&0) as usize;
                    if let Some(effects) = &mut layer.effects
                        && let Some(fx) = effects.get_mut(j)
                        && fx.params.topology() == self.block_topos.get(block).copied().unwrap_or(u32::MAX)
                    {
                        for (slot, &v) in fx
                            .params
                            .iter_mut()
                            .zip(&self.values[cursor..cursor + len])
                        {
                            slot.value = v;
                        }
                    }
                    cursor += len;
                    block += 1;
                }

                // Generator params — write only `.value` per slot, mirroring
                // the effect path; `.exposed` is host-state, not modulation.
                if has_gen {
                    let len = *self.block_lens.get(block).unwrap_or(&0) as usize;
                    if let Some(gp) = layer.gen_params_mut()
                        && gp.params.topology() == self.block_topos.get(block).copied().unwrap_or(u32::MAX)
                    {
                        for (slot, &v) in gp
                            .params
                            .iter_mut()
                            .zip(&self.values[cursor..cursor + len])
                        {
                            slot.value = v;
                        }
                    }
                    cursor += len;
                    block += 1;
                }
            } else {
                // Layer gone — skip its blocks
                for _ in 0..effect_count {
                    cursor += *self.block_lens.get(block).unwrap_or(&0) as usize;
                    block += 1;
                }
                if has_gen {
                    cursor += *self.block_lens.get(block).unwrap_or(&0) as usize;
                    block += 1;
                }
            }
        }
    }
}

impl Default for ContentState {
    fn default() -> Self {
        Self {
            current_beat: Beats::ZERO,
            current_time: Seconds::ZERO,
            is_playing: false,
            is_recording: false,
            content_fps: 0.0,
            content_frame_time_ms: 0.0,
            gpu_fence_wait_ms: 0.0,
            active_clips: 0,
            data_version: 0,
            editing_is_dirty: false,
            bpm: 120.0,
            frame_rate: 60.0,
            clock_authority: ClockAuthority::Internal,
            time_signature_numerator: 4,
            link_enabled: false,
            link_peers: 0,
            midi_clock_enabled: false,
            midi_clock_position_display: Arc::from(""),
            midi_clock_receiving: false,
            midi_clock_device_name: Arc::from(""),
            midi_device_names: Arc::from([]),
            audio_send_levels: [0.0; manifold_audio::analysis::MAX_SENDS],
            audio_send_count: 0,
            spectrogram_columns: Vec::new(),
            spectrogram_col_scalars: Vec::new(),
            spectrogram_num_bins: 0,
            spectrogram_fmin: 0.0,
            spectrogram_fmax: 0.0,
            spectrogram_low_hz: manifold_core::audio_setup::DEFAULT_LOW_HZ,
            spectrogram_mid_hz: manifold_core::audio_setup::DEFAULT_MID_HZ,
            spectrogram_features: None,
            osc_sender_enabled: false,
            percussion_importing: false,
            percussion_status_message: Arc::from(""),
            percussion_progress: 0.0,
            percussion_show_progress: false,
            profiling_active: false,
            profiling_frame_count: 0,
            led_enabled: false,
            is_live_recording: false,
            export_finished: None,
            undo_redo_event: None,
            ableton_session: None,
            ableton_connected: false,
            ableton_transport_enabled: false,
            ableton_sync_status:
                manifold_playback::transport_sync::TransportSyncStatus::Locked,
            osc_sync_mode: OscSyncMode::M4L,
            project_snapshot: None,
            modulation_snapshot: None,
            active_graph_snapshot: None,
            node_preview_info: None,
            live_node_params: Vec::new(),
            node_atlas_layout: Vec::new(),
            clip_atlas_layout: Vec::new(),
            automation_latched_params: Vec::new(),
            automation_armed: false,
        }
    }
}

#[cfg(test)]
mod modulation_topology_guard_tests {
    use super::*;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::PresetTypeId;

    fn spec(id: &str) -> ParamSpecDef {
        ParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: manifold_core::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
        }
    }

    fn manifest(entries: &[(&str, f32)]) -> ParamManifest {
        ParamManifest::from_params(
            entries
                .iter()
                .map(|(id, v)| {
                    let mut p = Param::bundled(spec(id));
                    p.value = *v;
                    p
                })
                .collect(),
        )
    }

    fn project_with_master(params: ParamManifest) -> Project {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("test.effect"));
        fx.params = params;
        project.settings.master_effects.push(fx);
        project
    }

    /// D8 — the exact case the old `len == len` guard missed: a same-length
    /// reorder between capture and apply must invalidate the block, so stale
    /// captured modulation values are NOT written onto the reordered slots.
    #[test]
    fn apply_skips_block_on_same_length_reorder() {
        let mut project = project_with_master(manifest(&[("a", 0.10), ("b", 0.20)]));

        let mut snap = ModulationSnapshot::empty();
        snap.capture_into(&project);

        // Same-length reorder on the content side after capture → topology bump.
        let m = &mut project.settings.master_effects[0].params;
        let a = m.remove("a").unwrap();
        m.push(a); // order is now [b, a], same length
        // Live values also advanced (as modulation/edits would).
        m.get_mut("a").unwrap().value = 0.90;
        m.get_mut("b").unwrap().value = 0.80;

        snap.apply(&mut project);

        let m = &project.settings.master_effects[0].params;
        assert_eq!(
            m.get("a").unwrap().value,
            0.90,
            "reordered block must be skipped, not overwritten with stale capture"
        );
        assert_eq!(m.get("b").unwrap().value, 0.80);
    }

    /// Control — with no structural change the topology matches and the block
    /// applies normally, overwriting live drift with captured values.
    #[test]
    fn apply_writes_block_when_topology_unchanged() {
        let mut project = project_with_master(manifest(&[("a", 0.10), ("b", 0.20)]));

        let mut snap = ModulationSnapshot::empty();
        snap.capture_into(&project);

        project.settings.master_effects[0]
            .params
            .get_mut("a")
            .unwrap()
            .value = 0.99;

        snap.apply(&mut project);

        let m = &project.settings.master_effects[0].params;
        assert_eq!(
            m.get("a").unwrap().value,
            0.10,
            "unchanged topology must apply the captured value"
        );
        assert_eq!(m.get("b").unwrap().value, 0.20);
    }
}
