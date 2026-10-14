//! State pushed from the content thread to the UI thread each content frame.
//!
//! The UI thread reads the latest ContentState to display transport info,
//! project data version, and other engine state without accessing the
//! PlaybackEngine or EditingService directly.

use manifold_core::effects::ParamId;
use manifold_core::project::Project;
use manifold_core::types::{ClockAuthority, LayerType, OscSyncMode};
use manifold_core::{Beats, EffectId, LayerId, Seconds};
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
    /// Explicit unavailable/ambiguous modifier preview, shown in the inspector.
    pub diagnostic: Option<&'static str>,
    /// Live scalar input port values this frame (`port_name`, value).
    pub inputs: Vec<(String, f32)>,
    /// Live scalar output port values — the signal the node is producing.
    pub outputs: Vec<(String, f32)>,
}

/// LED composite preview payload (LED_STRIPS_DESIGN MVP-P4, D22-D24): the
/// latest completed Art-Net readback rides the per-tick snapshot to the UI,
/// which blits it as a transposed band on the DMX lane's generator card.
/// The pixels are the send path's OWN readback — post-`led_gain`, pre-master-
/// brightness — so the preview tracks pattern and gain, NOT the LED brightness
/// slider. The buffer is the readback's own `Arc` allocation (D23), shared
/// with `pack_and_send` — no clone, no new per-frame allocation.
#[derive(Clone, Debug)]
pub enum LedPreview {
    /// A completed readback: tightly-packed RGBA8, 8×120 (row = LED position,
    /// col = strip index — the LED sample texture's own layout). `version`
    /// bumps per completed readback; the UI uploads on version change only.
    Frame { pixels: Arc<[u8]>, version: u64 },
    /// The send path is in blackout or disabled — the strips are dark. Clears
    /// the cached frame: a stale chase must never animate while the rig is
    /// dark (D24).
    Black,
    /// No LED controller or never enabled — no data exists; the UI renders
    /// no band at all.
    None,
}

/// One output file's result. Consumed once from the content channel by
/// `UIRoot::consume_export_notification` to show the export status toast.
/// A split export produces one result per section; still exports use it too.
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

/// A rejected graph edit that should be explained to the user. The sequence
/// is assigned by the content thread so the UI can edge-trigger a toast even
/// though rejected edits leave `data_version` unchanged.
#[derive(Clone, Debug)]
pub struct GraphEditDiagnostic {
    pub sequence: u64,
    pub message: String,
}

/// Authoritative selection of scene-modifier cards created by one content edit.
/// IDs are minted on the content thread, so the UI must consume this update
/// instead of predicting them from a pre-edit snapshot.
#[derive(Clone, Debug)]
pub struct ModifierSelectionUpdate {
    pub sequence: u64,
    pub layer_id: manifold_core::LayerId,
    pub ids: Vec<manifold_core::NodeId>,
}

#[derive(Clone, Debug)]
pub struct ObjectModifierSelectionUpdate {
    pub sequence: u64,
    pub layer_id: LayerId,
    pub owner_id: u32,
    pub node_doc_id: u32,
}

/// State snapshot sent from the content thread to the UI thread through a
/// one-slot handoff ([`state_channel`]); a replaced state folds into its
/// replacement ([`ContentState::absorb_replaced`]).
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
    /// CPU time spent stepping physics worlds and extracting poses (ms).
    pub physics_cpu_ms: f32,
    /// Bodies evaluated by physics worlds during the live content frame.
    pub physics_body_count: u32,
    /// Maximum unprocessed physics time across worlds, in seconds.
    pub physics_backlog_seconds: f32,
    /// At least one live simulation frame used its final allowed substep.
    pub sim_step_cap_hit: bool,
    /// A live simulation produced genuinely non-finite state. The show keeps running.
    pub sim_nonfinite: bool,
    pub active_clips: usize,

    // ── Editing ────────────────────────────────────────────────────
    pub data_version: u64,
    pub editing_is_dirty: bool,
    /// `EditingService::stamps()` at publish (D1).
    pub stamps: manifold_core::change::PartStamps,
    /// Moves only when a captured live value changed (D15); the modulation
    /// snapshot rides along only then.
    pub values_seq: u64,
    /// Bumped when an audio device or process hot-plug drains.
    pub audio_directory_version: u64,
    /// Content runtime state the UI builds structure from (D16).
    pub runtime: RuntimeSeqs,

    // ── Project settings (from authoritative project) ─────────────
    pub bpm: f64,
    pub frame_rate: f64,
    pub clock_authority: ClockAuthority,
    pub time_signature_numerator: i32,
    /// Runtime-only SDR presentation preview. Partial event/export snapshots
    /// leave this absent so they cannot reset the displayed viewing preference.
    pub sdr_preview: Option<bool>,
    /// The state carries engine state the UI takes as its own: the per-tick
    /// publish and warmup progress. Export notices leave it false and carry
    /// only their own fields.
    pub body: bool,

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
    /// RT quality settings (realtime vs export columns) — mirrors
    /// `project.settings.rt_quality`. Used by the RT quality panel to read
    /// current values when dispatching changes.
    pub rt_quality: manifold_core::settings::RtQualitySettings,
    pub spectrogram_low_hz: f32,
    pub spectrogram_mid_hz: f32,
    /// The tapped (scope-selected) send's latest features, for the spectrogram's
    /// per-band level meters. `None` when no send feeds the scope.
    pub spectrogram_features: Option<manifold_core::SendFeatures>,
    pub osc_sender_enabled: bool,

    // ── Percussion status ─────────────────────────────────────────
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
    /// Composite preview for the DMX lane inspector (LED_STRIPS_DESIGN
    /// MVP-P4). `Some` from the content thread's per-tick build — the inner
    /// `LedPreview::None` means "no LED controller"; the outer `None` is the
    /// degraded/export snapshot default (never published there), which the UI
    /// treats identically to `LedPreview::None` (no band).
    pub led_preview: Option<LedPreview>,

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
    /// Per-file results (success, failure or cancellation) since the UI last
    /// read, oldest first, including still exports.
    pub export_finished: Vec<ExportFinishedEvent>,
    /// Set once when the entire export run finishes, after all sections and
    /// playback restoration have completed.
    pub export_run_finished: bool,

    // ── Warmup ────────────────────────────────────────────────────
    /// Load-time warmup progress. `None` when no warmup is running.
    pub warmup: Option<manifold_core::WarmupProgress>,

    // ── Undo/redo ─────────────────────────────────────────────────
    /// Set on the tick an undo/redo actually happened; `None` every other
    /// tick. D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — see
    /// [`UndoRedoEvent`]'s doc comment for why this differs from
    /// `export_finished`'s out-of-band pattern.
    pub undo_redo_event: Option<UndoRedoEvent>,
    /// Most recent rejected graph-edit diagnostic. Persistent across snapshots
    /// so the UI can observe it once without relying on `data_version`.
    pub graph_edit_diagnostic: Option<Arc<GraphEditDiagnostic>>,
    /// Latched trigger delivery failure; Stop, seek or project load resets it.
    pub trigger_delivery_failure: Option<manifold_playback::engine::trigger_delivery::TriggerDeliveryFailure>,
    /// Most recent authoritative selection update for newly created scene
    /// modifier cards. Persistent across snapshots for sequence-gated UI use.
    pub edit_selection_update: Option<Arc<crate::edit_selection::EditSelectionUpdate>>,
    pub modifier_selection_update: Option<Arc<ModifierSelectionUpdate>>,
    pub object_modifier_selection_update: Option<Arc<ObjectModifierSelectionUpdate>>,

    // ── Ableton bridge ──────────────────────────────────────────
    /// Ableton session data for UI dropdown population.
    pub ableton_session: Option<Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Whether the Ableton bridge is currently connected.
    pub ableton_connected: bool,

    // ── Sync calibration (D19) ──────────────────────────────────
    /// Learned source offsets against Link, s: MIDI clock, AbletonOSC,
    /// timecode. The UI keeps them per rig in `UserPrefs`.
    pub sync_offsets: [f64; 3],
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
    pub active_graph_snapshot: Option<Arc<manifold_node_engine::snapshot::GraphSnapshot>>,
    /// Identity captured with the snapshot; UI requests may already target a
    /// different graph while this content-thread response is in flight.
    pub active_graph_target: Option<Arc<manifold_core::GraphTarget>>,

    /// Live node-output preview state for the editor's value inspector. `None`
    /// when no node is being previewed. See [`NodePreviewInfo`].
    pub node_preview_info: Option<NodePreviewInfo>,
    pub scene_viewport_frames: crate::scene_viewport::SceneViewportFrames,

    /// Live (post-modulation) scalar param values for every node of the watched
    /// effect/generator this frame, keyed by stable `NodeId`. The editor canvas
    /// overlays these onto its node faces so a driver / Ableton / envelope / card
    /// slider is *seen* moving the knob, instead of the frozen authoring def the
    /// `graph_version`-cached `active_graph_snapshot` carries. Empty whenever no
    /// editor is watching. Param names are `&'static`, so this allocates only the
    /// small per-node `Vec`s.
    pub live_node_params: manifold_node_engine::preview_encoding::LiveNodeParams,
    /// `(node_id, atlas_cell_index)` for the per-node thumbnail atlas captured
    /// this frame. The editor canvas maps each visible node to its atlas cell to
    /// blit the thumbnail. Empty unless the editor enabled the atlas.
    pub node_atlas_layout: Vec<(manifold_core::NodeId, u32)>,
    /// `(clip_id, filmstrip_cell_index, atlas_cell_index)` for the timeline
    /// clip-thumbnail **filmstrip** atlas (section 24 5c-2). Each clip owns one entry per
    /// captured filmstrip cell (bar / bar-group); the timeline tiles them across the
    /// clip body. Empty when no clips currently hold a thumbnail.
    pub clip_atlas_layout: Arc<[(manifold_core::ClipId, u32, u32)]>,

    /// Automation-lane override latches active this frame — runtime-only
    /// state (`manifold_playback::automation::AutomationLatches`, owned by
    /// `PlaybackEngine`, never part of `Project`/serialized; see
    /// `docs/AUTOMATION_LANES_DESIGN.md` section 4). Copied each tick from
    /// `PlaybackEngine::automation_latches()`, mirroring how other runtime-only
    /// playback state (e.g. `audio_send_levels`) reaches the UI thread — lane
    /// *data* itself needs no such copy since it lives on `PresetInstance` and
    /// already rides `project_snapshot` for free. Empty means nothing is
    /// overridden (the "Back to Arrangement" affordance is unlit); a
    /// non-empty entry `(EffectId, ParamId)` names one overridden lane, for
    /// per-lane graying. Lane-editing UI (P4) is the consumer; this phase
    /// only wires the data through.
    pub automation_latched_params: Vec<(EffectId, ParamId)>,

    /// Global Automation Arm state (section 5) — runtime-only, owned by
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

/// One sequence per piece of content runtime state the UI builds structure
/// from, bumped on the content thread where the value changes (D16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct RuntimeSeqs {
    pub automation_latches: u64,
    pub led_preview_presence: u64,
    pub warmup: u64,
    pub ableton_session: u64,
    pub sdr_preview: u64,
    pub export_progress: u64,
}

/// Identifies the owner of one param block in a [`ModulationSnapshot`], so
/// `apply` can find the destination manifest by stable id.
#[derive(Clone, PartialEq)]
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

    /// Fill this snapshot from the project, reusing existing vec capacity, and
    /// report whether anything differs from what it held (D15). Values are
    /// compared as they are written, so an unchanged capture writes nothing
    /// and clones no id; the content thread clones the snapshot only when this
    /// returns true.
    pub fn capture_into(&mut self, project: &Project) -> bool {
        let mut w = CaptureWriter { snap: self, value: 0, block: 0, changed: false };

        // Macros — no ParamManifest; guarded per-slot on apply, so its
        // topology slot is a sentinel that's never consulted.
        for slot in &project.settings.macro_bank.slots {
            w.value(slot.value);
        }
        w.block(project.settings.macro_bank.slots.len(), 0, &BlockOwnerRef::Macros);

        // Master effects — pack only `.value` from each ParamSlot;
        // exposure isn't a modulation concern and must not round-trip
        // through the per-frame fast path.
        for fx in &project.settings.master_effects {
            for p in fx.params.iter() {
                w.value(p.value);
            }
            w.block(fx.params.len(), fx.params.topology(), &BlockOwnerRef::Effect(&fx.id));
        }

        // Per-layer: effects + optional gen params
        for layer in &project.timeline.layers {
            let has_gen = layer.layer_type == LayerType::Generator && layer.gen_params().is_some();

            if let Some(effects) = &layer.effects {
                for fx in effects {
                    for p in fx.params.iter() {
                        w.value(p.value);
                    }
                    w.block(fx.params.len(), fx.params.topology(), &BlockOwnerRef::Effect(&fx.id));
                }
            }

            if has_gen && let Some(gp) = layer.gen_params() {
                for p in gp.params.iter() {
                    w.value(p.value);
                }
                w.block(gp.params.len(), gp.params.topology(), &BlockOwnerRef::GenParams(&layer.layer_id));
            }
        }
        w.finish()
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
            physics_cpu_ms: 0.0,
            physics_body_count: 0,
            physics_backlog_seconds: 0.0,
            sim_step_cap_hit: false,
            sim_nonfinite: false,
            active_clips: 0,
            data_version: 0,
            editing_is_dirty: false,
            stamps: Default::default(),
            values_seq: 0,
            audio_directory_version: 0,
            runtime: RuntimeSeqs::default(),
            bpm: 120.0,
            frame_rate: 60.0,
            clock_authority: ClockAuthority::Internal,
            time_signature_numerator: 4,
            sdr_preview: None,
            body: false,
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
            percussion_status_message: Arc::from(""),
            percussion_progress: 0.0,
            percussion_show_progress: false,
            profiling_active: false,
            profiling_frame_count: 0,
            led_enabled: false,
            led_preview: None,
            is_live_recording: false,
            recording_dropped_frames: 0,
            recording_dropped_audio_frames: 0,
            is_exporting: false,
            export_progress: 0.0,
            export_status: Arc::from(""),
            export_finished: Vec::new(),
            export_run_finished: false,
            warmup: None,
            undo_redo_event: None,
            graph_edit_diagnostic: None,
            trigger_delivery_failure: None,
            edit_selection_update: None,
            modifier_selection_update: None,
            object_modifier_selection_update: None,
            ableton_session: None,
            sync_offsets: [0.0; 3],
            ableton_connected: false,
            ableton_transport_enabled: false,
            ableton_sync_status:
                manifold_playback::transport_sync::TransportSyncStatus::Locked,
            osc_sync_mode: OscSyncMode::M4L,
            rt_quality: manifold_core::settings::RtQualitySettings::default(),
            project_snapshot: None,
            modulation_snapshot: None,
            active_graph_snapshot: None,
            active_graph_target: None,
            node_preview_info: None,
            scene_viewport_frames: std::array::from_fn(|_| None),
            live_node_params: Vec::new(),
            node_atlas_layout: Vec::new(),
            clip_atlas_layout: Arc::from([]),
            automation_latched_params: Vec::new(),
            automation_armed: false,
        }
    }
}

/// Spectrogram columns the UI has not drawn yet, capped at one screen width.
/// 4096 is the scope's maximum texture width, so an open scope never loses a
/// column it could show; the cap only bounds memory while nobody draws them.
const MAX_PENDING_SPECTROGRAM_COLS: usize = 4096;

/// Append `cols` (and their per-column overlay records) to the pending
/// spectrogram, dropping the oldest past [`MAX_PENDING_SPECTROGRAM_COLS`].
pub(crate) fn append_spectrogram(
    pending_cols: &mut Vec<f32>,
    pending_scalars: &mut Vec<manifold_spectral::ScopeColumn>,
    cols: &[f32],
    scalars: &[manifold_spectral::ScopeColumn],
    num_bins: usize,
) {
    pending_cols.extend_from_slice(cols);
    pending_scalars.extend_from_slice(scalars);
    if num_bins == 0 {
        return;
    }
    let excess = pending_cols.len().saturating_sub(MAX_PENDING_SPECTROGRAM_COLS * num_bins);
    if excess > 0 {
        pending_cols.drain(0..excess);
        let records = (excess / num_bins).min(pending_scalars.len());
        pending_scalars.drain(0..records);
    }
}

/// The content-to-UI handoff: one slot holding the newest state. Publishing
/// never blocks and never allocates; a state the UI has not taken yet is
/// folded into its replacement, so nothing the UI must see is lost and a
/// stalled UI (a native dialog, a window drag) holds one state, not a queue.
pub(crate) fn state_channel() -> (StateTx, crossbeam_channel::Receiver<ContentState>) {
    let (tx, rx) = crossbeam_channel::bounded(1);
    (StateTx { tx, slot: rx.clone() }, rx)
}

/// The publishing end of [`state_channel`]. Cloned to every thread that
/// publishes (the content thread and the still-export encoder).
#[derive(Clone)]
pub(crate) struct StateTx {
    tx: crossbeam_channel::Sender<ContentState>,
    /// The slot's own receiver, used only to take back a state the UI has
    /// not read so it can fold into the next one.
    slot: crossbeam_channel::Receiver<ContentState>,
}

impl StateTx {
    pub(crate) fn publish(&self, mut state: ContentState) {
        // Only an empty slot accepts a send, and whatever is taken back is
        // folded, so concurrent publishers and the UI's read lose nothing.
        // A race can fold a newer state in as the older one (the encoder takes
        // a tick, the content thread fills the slot, the encoder takes that
        // back as "older"). The fold keeps the project snapshot with the
        // higher data_version and the capture with the higher values_seq, so
        // the race can leave transport values one frame stale but never loses
        // an edit or a value change.
        loop {
            if let Ok(older) = self.slot.try_recv() {
                state.absorb_replaced(older);
            }
            match self.tx.try_send(state) {
                Ok(()) => return,
                Err(crossbeam_channel::TrySendError::Full(back)) => state = back,
                // The slot's own receiver keeps the channel connected.
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

impl ContentState {
    /// An export notice: progress, a finished file or the finished run.
    pub(crate) fn is_export_notice(&self) -> bool {
        self.is_exporting || !self.export_finished.is_empty() || self.export_run_finished
    }

    /// An export notice with no engine state behind it: the UI shows the
    /// notice and keeps its own state. Never true while a snapshot rides it.
    pub(crate) fn is_bare_notice(&self) -> bool {
        self.is_export_notice()
            && !self.body
            && self.project_snapshot.is_none()
            && self.modulation_snapshot.is_none()
    }

    /// Fold `older`, a state the UI never read, into this newer one so the UI
    /// sees what reading both in order would have shown it: the newest values;
    /// the project snapshot with the higher data_version, with the stamps
    /// that describe it; the capture with the higher values_seq; the older
    /// undo toast when this state has none; every spectrogram column and
    /// export file result.
    pub(crate) fn absorb_replaced(&mut self, mut older: ContentState) {
        if self.is_bare_notice() && !older.is_bare_notice() {
            // A bare notice carries no engine state: the older state stays the
            // body and the notice lands on it.
            let mut notice = std::mem::replace(self, older);
            self.export_finished.append(&mut notice.export_finished);
            self.export_run_finished |= notice.export_run_finished;
            self.is_exporting = notice.is_exporting;
            self.export_progress = notice.export_progress;
            self.export_status = notice.export_status;
            return;
        }
        self.body |= older.body;
        if older.project_snapshot.is_some()
            && (self.project_snapshot.is_none() || older.data_version > self.data_version)
        {
            self.project_snapshot = older.project_snapshot.take();
            self.stamps = older.stamps;
            self.data_version = older.data_version;
        }
        if older.modulation_snapshot.is_some()
            && (self.modulation_snapshot.is_none() || older.values_seq > self.values_seq)
        {
            self.modulation_snapshot = older.modulation_snapshot.take();
            self.values_seq = older.values_seq;
        }
        if self.undo_redo_event.is_none() {
            self.undo_redo_event = older.undo_redo_event.take();
        }
        if !older.spectrogram_columns.is_empty() {
            append_spectrogram(
                &mut older.spectrogram_columns,
                &mut older.spectrogram_col_scalars,
                &self.spectrogram_columns,
                &self.spectrogram_col_scalars,
                self.spectrogram_num_bins,
            );
            self.spectrogram_columns = older.spectrogram_columns;
            self.spectrogram_col_scalars = older.spectrogram_col_scalars;
        }
        // Export progress belongs to the newest notice; a per-tick state
        // carries none of its own.
        if !self.is_export_notice() {
            self.is_exporting = older.is_exporting;
            self.export_progress = older.export_progress;
            self.export_status = older.export_status;
        }
        if !older.export_finished.is_empty() {
            older.export_finished.append(&mut self.export_finished);
            self.export_finished = older.export_finished;
        }
        self.export_run_finished |= older.export_run_finished;
    }
}

/// A block owner by reference, so an unchanged capture clones no id.
enum BlockOwnerRef<'a> {
    Macros,
    Effect(&'a EffectId),
    GenParams(&'a manifold_core::LayerId),
}

impl BlockOwnerRef<'_> {
    fn matches(&self, owner: &BlockOwner) -> bool {
        match (self, owner) {
            (BlockOwnerRef::Macros, BlockOwner::Macros) => true,
            (BlockOwnerRef::Effect(a), BlockOwner::Effect(b)) => *a == b,
            (BlockOwnerRef::GenParams(a), BlockOwner::GenParams(b)) => *a == b,
            _ => false,
        }
    }

    fn owned(&self) -> BlockOwner {
        match self {
            BlockOwnerRef::Macros => BlockOwner::Macros,
            BlockOwnerRef::Effect(id) => BlockOwner::Effect((*id).clone()),
            BlockOwnerRef::GenParams(id) => BlockOwner::GenParams((*id).clone()),
        }
    }
}

/// Overwrites a [`ModulationSnapshot`] in place, noting any difference.
struct CaptureWriter<'a> {
    snap: &'a mut ModulationSnapshot,
    value: usize,
    block: usize,
    changed: bool,
}

impl CaptureWriter<'_> {
    fn value(&mut self, v: f32) {
        match self.snap.values.get_mut(self.value) {
            Some(slot) if slot.to_bits() == v.to_bits() => {}
            Some(slot) => {
                *slot = v;
                self.changed = true;
            }
            None => {
                self.snap.values.push(v);
                self.changed = true;
            }
        }
        self.value += 1;
    }

    fn block(&mut self, len: usize, topo: u32, owner: &BlockOwnerRef<'_>) {
        let i = self.block;
        self.block += 1;
        if i < self.snap.block_lens.len() {
            if self.snap.block_lens[i] != len as u16 {
                self.snap.block_lens[i] = len as u16;
                self.changed = true;
            }
            if self.snap.block_topos[i] != topo {
                self.snap.block_topos[i] = topo;
                self.changed = true;
            }
            if !owner.matches(&self.snap.block_owners[i]) {
                self.snap.block_owners[i] = owner.owned();
                self.changed = true;
            }
        } else {
            self.snap.block_lens.push(len as u16);
            self.snap.block_topos.push(topo);
            self.snap.block_owners.push(owner.owned());
            self.changed = true;
        }
    }

    fn finish(self) -> bool {
        let mut changed = self.changed;
        if self.snap.values.len() > self.value {
            self.snap.values.truncate(self.value);
            changed = true;
        }
        if self.snap.block_lens.len() > self.block {
            self.snap.block_lens.truncate(self.block);
            self.snap.block_topos.truncate(self.block);
            self.snap.block_owners.truncate(self.block);
            changed = true;
        }
        changed
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
            tooltip: None,
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
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
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

#[cfg(test)]
mod handoff_tests {
    use super::*;
    use manifold_core::change::{ChangeSet, Part, PartStamps};

    /// A per-tick state at `beat`, as `send_state` builds one.
    fn tick(beat: f64) -> ContentState {
        ContentState {
            current_beat: Beats(beat),
            body: true,
            ..ContentState::default()
        }
    }

    fn finished(message: &str) -> ContentState {
        ContentState {
            export_finished: vec![ExportFinishedEvent {
                success: true,
                message: message.into(),
                output_path: String::new(),
            }],
            ..ContentState::default()
        }
    }

    /// BUG-8wmor (state channel allocates a block every 31 frames): a state
    /// the UI never read folds into its replacement, so the edit's snapshot,
    /// the parts it moved, its undo toast and its scope columns all arrive.
    #[test]
    fn replaced_snapshot_keeps_its_change_set() {
        let (tx, rx) = state_channel();
        let base = PartStamps::default();
        let mut edited = base;
        edited.bump(ChangeSet::of(&[Part::Clips]));
        let snapshot = Arc::new(Project::default());
        tx.publish(ContentState {
            project_snapshot: Some(snapshot.clone()),
            stamps: edited,
            data_version: 3,
            undo_redo_event: Some(UndoRedoEvent { is_redo: false, description: "Move Clip".into() }),
            spectrogram_columns: vec![1.0, 2.0],
            spectrogram_num_bins: 2,
            ..tick(1.0)
        });
        tx.publish(ContentState {
            stamps: edited,
            data_version: 3,
            spectrogram_columns: vec![3.0, 4.0],
            spectrogram_num_bins: 2,
            ..tick(2.0)
        });

        let state = rx.try_recv().expect("one state waits");
        assert!(rx.try_recv().is_err(), "the handoff holds one state");
        assert_eq!(state.current_beat, Beats(2.0), "the newest values win");
        assert!(Arc::ptr_eq(state.project_snapshot.as_ref().expect("snapshot kept"), &snapshot));
        let mut ui = crate::frame_plan::UiStamps::default();
        ui.fold_snapshot(&base);
        assert_eq!(ui.fold_snapshot(&state.stamps), ChangeSet::of(&[Part::Clips]));
        assert_eq!(state.data_version, 3);
        assert_eq!(state.undo_redo_event.map(|e| e.description).as_deref(), Some("Move Clip"));
        assert_eq!(state.spectrogram_columns, [1.0, 2.0, 3.0, 4.0]);
    }

    /// A state carrying a snapshot is never a bare notice: a warmup state
    /// folded over a tick's snapshot and an export notice still hands the
    /// UI the snapshot.
    #[test]
    fn warmup_over_a_notice_keeps_the_snapshot() {
        let (tx, rx) = state_channel();
        let snapshot = Arc::new(Project::default());
        tx.publish(ContentState { project_snapshot: Some(snapshot.clone()), data_version: 4, ..tick(1.0) });
        tx.publish(finished("Exported"));
        tx.publish(ContentState { body: true, warmup: None, ..ContentState::default() });
        let state = rx.try_recv().expect("one state waits");
        assert!(!state.is_bare_notice());
        assert!(Arc::ptr_eq(state.project_snapshot.as_ref().expect("snapshot kept"), &snapshot));
        assert_eq!(state.export_finished.len(), 1);

        let notice_with_snapshot = ContentState {
            project_snapshot: Some(snapshot),
            export_run_finished: true,
            ..ContentState::default()
        };
        assert!(!notice_with_snapshot.is_bare_notice());
    }

    /// A racing publisher can fold the newer state in as the older one; the
    /// snapshot with the higher data_version stays, with its stamps.
    #[test]
    fn higher_version_snapshot_wins_a_race() {
        let mut newer_stamps = PartStamps::default();
        newer_stamps.bump(ChangeSet::of(&[Part::Clips]));
        let newer = Arc::new(Project::default());
        let mut state = ContentState {
            project_snapshot: Some(Arc::new(Project::default())),
            data_version: 1,
            ..tick(1.0)
        };
        state.absorb_replaced(ContentState {
            project_snapshot: Some(newer.clone()),
            data_version: 2,
            stamps: newer_stamps,
            ..tick(2.0)
        });
        assert!(Arc::ptr_eq(state.project_snapshot.as_ref().expect("snapshot kept"), &newer));
        assert_eq!(state.data_version, 2);
        assert_eq!(state.stamps, newer_stamps);
    }

    /// A newer snapshot replaces an older one with its own stamps.
    #[test]
    fn newer_snapshot_wins() {
        let (tx, rx) = state_channel();
        let older = Arc::new(Project::default());
        let newer = Arc::new(Project::default());
        tx.publish(ContentState { project_snapshot: Some(older), data_version: 1, ..tick(1.0) });
        tx.publish(ContentState { project_snapshot: Some(newer.clone()), data_version: 2, ..tick(2.0) });
        let state = rx.try_recv().expect("one state waits");
        assert!(Arc::ptr_eq(state.project_snapshot.as_ref().unwrap(), &newer));
        assert_eq!(state.data_version, 2);
    }

    /// An export notice carries no engine state: folded either way with a
    /// tick, the UI gets the tick's body and the notice's event.
    #[test]
    fn export_notice_and_tick_fold_both_ways() {
        let (tx, rx) = state_channel();
        tx.publish(tick(1.0));
        tx.publish(finished("Exported"));
        let state = rx.try_recv().expect("one state waits");
        assert!(!state.is_bare_notice(), "the tick's body survives a newer notice");
        assert_eq!(state.current_beat, Beats(1.0));
        assert_eq!(state.export_finished.len(), 1);

        tx.publish(finished("Exported"));
        tx.publish(tick(2.0));
        let state = rx.try_recv().expect("one state waits");
        assert!(!state.is_bare_notice());
        assert_eq!(state.current_beat, Beats(2.0));
        assert_eq!(state.export_finished.len(), 1, "the notice survives a newer tick");
    }

    /// Two notices: the newest progress, the older file result, the run end.
    #[test]
    fn export_notices_fold() {
        let (tx, rx) = state_channel();
        tx.publish(finished("Section 1"));
        tx.publish(ContentState { export_run_finished: true, ..ContentState::default() });
        tx.publish(ContentState {
            is_exporting: true,
            export_progress: 0.5,
            ..ContentState::default()
        });
        let state = rx.try_recv().expect("one state waits");
        assert!(state.is_bare_notice());
        assert_eq!(state.export_finished[0].message, "Section 1");
        assert!(state.export_run_finished);
        assert!(state.is_exporting);
        assert_eq!(state.export_progress, 0.5);
    }

    /// Publishing from two threads with no reader loses no event.
    #[test]
    fn concurrent_publishers_lose_nothing() {
        let (tx, rx) = state_channel();
        let notices = tx.clone();
        let encoder = std::thread::spawn(move || notices.publish(finished("Still")));
        for beat in 0..1000 {
            tx.publish(tick(f64::from(beat)));
        }
        encoder.join().unwrap();
        let state = rx.try_recv().expect("one state waits");
        assert_eq!(state.current_beat, Beats(999.0));
        assert_eq!(state.export_finished.len(), 1);
    }
}
