//! State pushed from the content thread to the UI thread each content frame.
//!
//! The UI thread reads the latest ContentState to display transport info,
//! project data version, and other engine state without accessing the
//! PlaybackEngine or EditingService directly.

use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, LayerType, OscSyncMode};
use manifold_core::{Beats, Bpm, Seconds};
use manifold_playback::stem_audio::STEM_COUNT;
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

/// Sent once when an export finishes.
// FIXME(dead-code-audit): event is constructed and stored in ContentState but
// never read by UI — export-finished feedback isn't wired.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ExportFinishedEvent {
    pub success: bool,
    pub message: String,
    pub output_path: String,
}

/// State snapshot sent from the content thread to the UI thread.
/// The UI thread drains these from a bounded channel and uses the latest.
// FIXME(dead-code-audit): several fields written by content thread but never
// read by UI (link_tempo, link_is_playing, midi_clock_bpm, osc_receiving_timecode,
// osc_timecode_display, stem_expanded, stem_ready, stem_available, led_initialized).
#[derive(Clone)]
#[allow(dead_code)]
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
    pub link_tempo: f64,
    pub link_peers: i32,
    pub link_is_playing: bool,
    pub midi_clock_enabled: bool,
    pub midi_clock_bpm: Bpm,
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
    /// Per-column overlay scalars in lockstep with `spectrogram_columns`: 7 per
    /// column, `[centroid_full, centroid_low, centroid_mid, centroid_high,
    /// onset_low, onset_mid, onset_high]` (the four scrolling per-band centroid
    /// traces + per-band transient ticks). Length is
    /// `7 * (columns / spectrogram_num_bins)`.
    pub spectrogram_col_scalars: Vec<f32>,
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
    pub osc_receiving_timecode: bool,
    pub osc_timecode_display: Arc<str>,

    // ── Stem audio state ──────────────────────────────────────────
    pub stem_expanded: bool,
    pub stem_ready: bool,
    pub stem_muted: [bool; STEM_COUNT],
    pub stem_soloed: [bool; STEM_COUNT],
    pub stem_available: [bool; STEM_COUNT],

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
    /// Whether the LED pipeline is initialized and ready.
    pub led_initialized: bool,

    // ── Live Recording ─────────────────────────────────────────────
    /// Whether a live recording is currently in progress.
    pub is_live_recording: bool,
    /// Number of video frames dropped during recording (pool exhaustion).
    pub recording_dropped_frames: u32,

    // ── Export ────────────────────────────────────────────────────
    /// Whether an export is currently in progress.
    pub is_exporting: bool,
    /// Export progress (0.0..1.0).
    pub export_progress: f32,
    /// Export status text (e.g. "Exporting 120/600 (20%)").
    pub export_status: Arc<str>,
    /// Set once when export finishes (success or failure).
    pub export_finished: Option<ExportFinishedEvent>,

    // ── Ableton bridge ──────────────────────────────────────────
    /// Ableton session data for UI dropdown population.
    pub ableton_session: Option<Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Whether the Ableton bridge is currently connected.
    pub ableton_connected: bool,
    pub ableton_transport_enabled: bool,
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
}

/// Lightweight snapshot of modulated param values.
/// Captures only `param_values` from effects and generator params,
/// avoiding a full `Project::clone()` on every modulation frame.
///
/// All float values are packed into a single flat buffer (`values`).
/// A parallel `block_lens` array records the length of each param block.
/// Layout order: macros, master effects, then per-layer (effects, gen).
/// Clone = 3 Vec allocations (values + block_lens + layer_shapes)
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
        self.layer_shapes.clear();

        // Macros (block 0)
        let macro_count = project.settings.macro_bank.slots.len();
        for slot in &project.settings.macro_bank.slots {
            self.values.push(slot.value);
        }
        self.block_lens.push(macro_count as u16);

        // Master effects — pack only `.value` from each ParamSlot;
        // exposure isn't a modulation concern and must not round-trip
        // through the per-frame fast path.
        self.master_count = project.settings.master_effects.len() as u16;
        for fx in &project.settings.master_effects {
            let len = fx.param_values.len();
            self.values.extend(fx.param_values.iter().map(|p| p.value));
            self.block_lens.push(len as u16);
        }

        // Per-layer: effects + optional gen params
        for layer in &project.timeline.layers {
            let effect_count = layer.effects.as_ref().map_or(0, |effects| effects.len());
            let has_gen = layer.layer_type == LayerType::Generator && layer.gen_params().is_some();

            if let Some(effects) = &layer.effects {
                for fx in effects {
                    let len = fx.param_values.len();
                    self.values.extend(fx.param_values.iter().map(|p| p.value));
                    self.block_lens.push(len as u16);
                }
            }

            if has_gen && let Some(gp) = layer.gen_params() {
                let len = gp.param_values.len();
                self.values.extend(gp.param_values.iter().map(|p| p.value));
                self.block_lens.push(len as u16);
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
                && fx.param_values.len() == len
            {
                for (slot, &v) in fx
                    .param_values
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
                        && fx.param_values.len() == len
                    {
                        for (slot, &v) in fx
                            .param_values
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
                        && gp.param_values.len() == len
                    {
                        for (slot, &v) in gp
                            .param_values
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
            link_tempo: 120.0,
            link_peers: 0,
            link_is_playing: false,
            midi_clock_enabled: false,
            midi_clock_bpm: Bpm(120.0),
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
            osc_receiving_timecode: false,
            osc_timecode_display: Arc::from(""),
            stem_expanded: false,
            stem_ready: false,
            stem_muted: [false; STEM_COUNT],
            stem_soloed: [false; STEM_COUNT],
            stem_available: [false; STEM_COUNT],
            percussion_importing: false,
            percussion_status_message: Arc::from(""),
            percussion_progress: 0.0,
            percussion_show_progress: false,
            profiling_active: false,
            profiling_frame_count: 0,
            led_enabled: false,
            led_initialized: false,
            is_live_recording: false,
            recording_dropped_frames: 0,
            is_exporting: false,
            export_progress: 0.0,
            export_status: Arc::from(""),
            export_finished: None,
            ableton_session: None,
            ableton_connected: false,
            ableton_transport_enabled: false,
            osc_sync_mode: OscSyncMode::M4L,
            project_snapshot: None,
            modulation_snapshot: None,
            active_graph_snapshot: None,
            node_preview_info: None,
            live_node_params: Vec::new(),
            node_atlas_layout: Vec::new(),
        }
    }
}
