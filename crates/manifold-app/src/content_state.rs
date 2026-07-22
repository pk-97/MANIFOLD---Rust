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
    /// D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    /// P3c, BUG-082's fix): the shaped `AudioModShape::condition()` signal
    /// for every fire-mode config (param gate cards + clip triggers)
    /// evaluated this tick — the SAME value the evaluator edge-detects
    /// against the fixed 0.5 threshold (D3 AS-BUILT). `Copy`/fixed-size
    /// (`manifold_core::audio_trigger::FireMeterCapture`), so this field
    /// costs nothing extra to carry across the content→UI snapshot. Read by
    /// `ui_root.rs::update_fire_meters` every UI tick to push live levels
    /// onto already-built drawer meters, in place — never rebuilt.
    pub fire_meters: manifold_core::audio_trigger::FireMeterCapture,
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
    /// Video frames dropped this recording (texture-pool exhaustion). BUG-084
    /// — surfaced on the layer-header record button. Restored (with its
    /// consumer, per UI_PROJECTION_LAYER_DESIGN I1) from the P0 orphan purge
    /// that originally deleted it un-consumed.
    pub recording_dropped_frames: u32,
    /// Audio sample-frames dropped this recording by the native encoder's
    /// backpressure gate (`LiveRecordingPlugin.m`'s `WriteAudioSamples`).
    /// Feeds the same drop indicator as `recording_dropped_frames`; also an
    /// instrument for BUG-086 (recorded audio track under-covering duration
    /// on longer takes, root cause unconfirmed) — a non-zero reading during
    /// a short take implicates this gate, a zero reading rules it out.
    pub recording_dropped_audio_frames: u32,

    // ── Export ────────────────────────────────────────────────────
    /// Whether an export is currently in progress. BUG-083: restored (with
    /// its UI consumer, per UI_PROJECTION_LAYER_DESIGN I1) from the P0
    /// orphan purge that originally deleted this un-consumed.
    pub is_exporting: bool,
    /// Export progress, 0.0..1.0. BUG-083.
    pub export_progress: f32,
    /// Export status text (e.g. "Exporting 120/600 (20%)"). BUG-083.
    pub export_status: Arc<str>,
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
/// avoiding a full `Project::clone()` on every frame.
///
/// All float values are packed into a single flat buffer (`values`).
/// A parallel `block_lens` array records the length of each param block.
/// Each block also carries its owner id (`block_owners`), so `apply`
/// routes every block by IDENTITY — layer/effect insertion, removal, or
/// reorder between capture and apply can no longer misroute or silently
/// starve a block. The `topology` stamp still guards a found-by-id block
/// against a same-frame manifest reshape (skipped for exactly one frame —
/// the next capture re-keys).
/// Clone = 4 Vec allocations (values + block_lens + block_topos +
/// block_owners) instead of ~128 separate Vec<f32> clones.
#[derive(Clone)]
pub struct ModulationSnapshot {
    /// All param values concatenated in block order.
    values: Vec<f32>,
    /// Length of each param block in `values`, in the same order.
    block_lens: Vec<u16>,
    /// `ParamManifest::topology` for each block at capture time, parallel to
    /// `block_lens` (D8). Apply skips any block whose live manifest topology no
    /// longer matches the captured stamp — this catches same-length param
    /// reorders, which a `len == len` check silently misroutes. The macro block
    /// has no manifest; its slot holds a sentinel `0` and is guarded per-slot
    /// instead.
    block_topos: Vec<u32>,
    /// Owner of each block, parallel to `block_lens`, captured at capture time.
    /// Apply resolves the destination by this id, never by walk position.
    block_owners: Vec<BlockOwner>,
}

/// Identifies the owner of one param block in a [`ModulationSnapshot`], so
/// `apply` can find the destination manifest by stable id.
#[derive(Clone)]
enum BlockOwner {
    /// The macro bank — no per-slot ids, routed by slot index.
    Macros,
    /// A master or per-layer effect instance's `ParamManifest`.
    Effect(EffectId),
    /// A layer's generator `ParamManifest`.
    GenParams(manifold_core::LayerId),
}

impl ModulationSnapshot {
    /// Create an empty snapshot (used as scratch buffer on content thread).
    pub fn empty() -> Self {
        Self {
            values: Vec::new(),
            block_lens: Vec::new(),
            block_topos: Vec::new(),
            block_owners: Vec::new(),
        }
    }

    /// Fill this snapshot from the project, reusing existing vec capacity.
    /// Called each frame on the content thread's scratch instance — zero
    /// allocation after the first frame (vecs grow once, never shrink; id
    /// clones are `Arc<str>` refbumps).
    pub fn capture_into(&mut self, project: &Project) {
        self.values.clear();
        self.block_lens.clear();
        self.block_topos.clear();
        self.block_owners.clear();

        // Macros — no ParamManifest; guarded per-slot on apply, so its
        // topology slot is a sentinel that's never consulted.
        let macro_count = project.settings.macro_bank.slots.len();
        for slot in &project.settings.macro_bank.slots {
            self.values.push(slot.value);
        }
        self.block_lens.push(macro_count as u16);
        self.block_topos.push(0);
        self.block_owners.push(BlockOwner::Macros);

        // Master effects — pack only `.value` from each ParamSlot;
        // exposure isn't a modulation concern and must not round-trip
        // through the per-frame fast path.
        for fx in &project.settings.master_effects {
            let len = fx.params.len();
            self.values.extend(fx.params.iter().map(|p| p.value));
            self.block_lens.push(len as u16);
            self.block_topos.push(fx.params.topology());
            self.block_owners.push(BlockOwner::Effect(fx.id.clone()));
        }

        // Per-layer: effects + optional gen params
        for layer in &project.timeline.layers {
            let has_gen = layer.layer_type == LayerType::Generator && layer.gen_params().is_some();

            if let Some(effects) = &layer.effects {
                for fx in effects {
                    let len = fx.params.len();
                    self.values.extend(fx.params.iter().map(|p| p.value));
                    self.block_lens.push(len as u16);
                    self.block_topos.push(fx.params.topology());
                    self.block_owners.push(BlockOwner::Effect(fx.id.clone()));
                }
            }

            if has_gen && let Some(gp) = layer.gen_params() {
                let len = gp.params.len();
                self.values.extend(gp.params.iter().map(|p| p.value));
                self.block_lens.push(len as u16);
                self.block_topos.push(gp.params.topology());
                self.block_owners.push(BlockOwner::GenParams(layer.layer_id.clone()));
            }
        }
    }

    /// Apply modulated values to a project in-place. Overwrites only
    /// `param_values` — no structural changes, no allocations.
    ///
    /// Routing is by owner id, never by position: a layer or effect added,
    /// removed, or reordered between capture and apply can no longer misroute
    /// values or starve later blocks. The per-block topology stamp still skips
    /// a found-by-id block whose manifest was reshaped mid-flight (a one-frame
    /// skip; the next capture re-keys). A block whose owner id no longer
    /// resolves (deleted since capture) is skipped with a loud log — the old
    /// positional walk dropped this silently.
    pub fn apply(&self, project: &mut Project) {
        let mut cursor = 0usize; // position in values

        for (block, owner) in self.block_owners.iter().enumerate() {
            let len = self.block_lens.get(block).copied().unwrap_or(0) as usize;
            let stamp = self.block_topos.get(block).copied().unwrap_or(u32::MAX);
            let slice = &self.values[cursor..cursor + len];
            match owner {
                BlockOwner::Macros => {
                    for (i, &v) in slice.iter().enumerate() {
                        if let Some(slot) = project.settings.macro_bank.slots.get_mut(i) {
                            slot.value = v;
                        }
                    }
                }
                // Write only `.value` per slot; the `.exposed` flag is
                // host-state, not modulation-state.
                BlockOwner::Effect(id) => {
                    match project.find_effect_by_id_mut(id) {
                        Some(fx) if fx.params.topology() == stamp => {
                            for (slot, &v) in fx.params.iter_mut().zip(slice) {
                                slot.value = v;
                            }
                        }
                        Some(_) => { /* manifest reshaped mid-flight — one-frame skip */ }
                        None => {
                            log::warn!(
                                "[ModulationSnapshot] effect {id} not found at apply — \
                                 deleted since capture; block skipped"
                            );
                        }
                    }
                }
                BlockOwner::GenParams(layer_id) => {
                    let gp = project
                        .timeline
                        .find_layer_by_id_mut(layer_id.as_str())
                        .and_then(|(_, layer)| layer.gen_params_mut());
                    match gp {
                        Some(gp) if gp.params.topology() == stamp => {
                            for (slot, &v) in gp.params.iter_mut().zip(slice) {
                                slot.value = v;
                            }
                        }
                        Some(_) => { /* manifest reshaped mid-flight — one-frame skip */ }
                        None => {
                            log::warn!(
                                "[ModulationSnapshot] gen params for layer {layer_id} not found \
                                 at apply — deleted since capture; block skipped"
                            );
                        }
                    }
                }
            }
            cursor += len;
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
            fire_meters: manifold_core::audio_trigger::FireMeterCapture::default(),
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
            recording_dropped_frames: 0,
            recording_dropped_audio_frames: 0,
            is_exporting: false,
            export_progress: 0.0,
            export_status: Arc::from(""),
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
    use manifold_core::layer::Layer;
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
            wraps: false,
            section: None,
            card_visible: true,
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

    fn effect(entries: &[(&str, f32)]) -> PresetInstance {
        let mut fx = PresetInstance::new(PresetTypeId::new("test.effect"));
        fx.params = manifest(entries);
        fx
    }

    fn project_with_master(params: ParamManifest) -> Project {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("test.effect"));
        fx.params = params;
        project.settings.master_effects.push(fx);
        project
    }

    fn layer_with_effect(name: &str, index: i32, entries: &[(&str, f32)]) -> Layer {
        let mut layer = Layer::new_video(name.to_string(), index);
        layer.effects = Some(vec![effect(entries)]);
        layer
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

    /// Id-keyed routing (a): a layer INSERTED between capture and apply must
    /// not shift the routing of the layers after it. The old positional walk
    /// routed by layer index, so every block after the insertion landed on the
    /// wrong layer's effects; the id-keyed apply resolves by LayerId/EffectId.
    #[test]
    fn apply_routes_by_id_when_layer_inserted() {
        let mut project = Project::default();
        let layer_a = layer_with_effect("A", 0, &[("p", 0.10)]);
        let layer_b = layer_with_effect("B", 1, &[("q", 0.20)]);
        let fx_a = layer_a.effects.as_ref().unwrap()[0].id.clone();
        let fx_b = layer_b.effects.as_ref().unwrap()[0].id.clone();
        project.timeline.layers.push(layer_a);
        project.timeline.layers.push(layer_b);

        let mut snap = ModulationSnapshot::empty();
        snap.capture_into(&project);

        // UI side drifts (as modulation would), then a structural insert lands
        // before this snapshot is applied.
        project
            .find_effect_by_id_mut(&fx_a)
            .unwrap()
            .params
            .get_mut("p")
            .unwrap()
            .value = 0.90;
        project
            .find_effect_by_id_mut(&fx_b)
            .unwrap()
            .params
            .get_mut("q")
            .unwrap()
            .value = 0.80;
        let inserted = layer_with_effect("NEW", 2, &[("z", 0.50)]);
        let fx_new = inserted.effects.as_ref().unwrap()[0].id.clone();
        project.timeline.layers.insert(0, inserted);

        snap.apply(&mut project);

        assert_eq!(
            project
                .find_effect_by_id(&fx_a)
                .unwrap()
                .params
                .get("p")
                .unwrap()
                .value,
            0.10,
            "layer A's effect must receive ITS captured value despite the insert"
        );
        assert_eq!(
            project
                .find_effect_by_id(&fx_b)
                .unwrap()
                .params
                .get("q")
                .unwrap()
                .value,
            0.20,
            "layer B's effect must receive ITS captured value despite the insert"
        );
        assert_eq!(
            project
                .find_effect_by_id(&fx_new)
                .unwrap()
                .params
                .get("z")
                .unwrap()
                .value,
            0.50,
            "the inserted layer is not in the snapshot — must stay untouched"
        );
    }

    /// Id-keyed routing (b): effects REORDERED between capture and apply must
    /// still receive their own captured values. The old positional walk wrote
    /// effect 0's values into effect 1 and vice versa; id routing can't.
    #[test]
    fn apply_routes_by_id_when_effects_reordered() {
        let mut project = Project::default();
        let fx_x = effect(&[("p", 0.10)]);
        let fx_y = effect(&[("q", 0.20)]);
        let id_x = fx_x.id.clone();
        let id_y = fx_y.id.clone();
        project.settings.master_effects.push(fx_x);
        project.settings.master_effects.push(fx_y);

        let mut snap = ModulationSnapshot::empty();
        snap.capture_into(&project);

        // Reorder + drift.
        project.settings.master_effects.swap(0, 1);
        for fx in &mut project.settings.master_effects {
            for p in fx.params.iter_mut() {
                p.value = 0.99;
            }
        }

        snap.apply(&mut project);

        assert_eq!(
            project
                .find_effect_by_id(&id_x)
                .unwrap()
                .params
                .get("p")
                .unwrap()
                .value,
            0.10,
            "effect X must get X's captured value after reorder"
        );
        assert_eq!(
            project
                .find_effect_by_id(&id_y)
                .unwrap()
                .params
                .get("q")
                .unwrap()
                .value,
            0.20,
            "effect Y must get Y's captured value after reorder"
        );
    }

    /// Always-send contract (c): capture must produce a complete snapshot from
    /// a project with NO drivers/envelopes/Ableton activity — the content
    /// thread sends the snapshot every tick now, so the capture path carries
    /// no dependence on a `modulation_active` flag. Two consecutive ticks on
    /// the same scratch buffer both yield a full snapshot.
    #[test]
    fn capture_is_complete_without_any_modulation_active() {
        let mut project = Project::default();
        project
            .timeline
            .layers
            .push(layer_with_effect("A", 0, &[("p", 0.10)]));
        project.settings.master_effects.push(effect(&[("m", 0.30)]));

        let mut scratch = ModulationSnapshot::empty();
        for tick in 0..2 {
            scratch.capture_into(&project);
            let snap = scratch.clone();
            // macros + master fx + layer fx = 3 blocks, every tick.
            assert_eq!(
                snap.block_owners.len(),
                3,
                "tick {tick}: snapshot must be complete with no modulation source"
            );
            assert_eq!(
                snap.values.len(),
                project.settings.macro_bank.slots.len() + 2,
                "tick {tick}: values must cover macros + master fx + layer fx"
            );
        }
    }
}
