pub mod ableton_picker;
pub mod audio_setup_panel;
pub mod browser_popup;
pub mod clip_chrome;
pub mod copy_to_clipboard_label;
pub mod drawer;
pub mod dropdown;
pub mod footer;
pub mod graph_editor;
pub mod graph_palette;
pub mod header;
pub mod inspector;
pub mod layer_chrome;
pub mod layer_header;
pub mod macros_panel;
pub mod master_chrome;
pub mod overlay;
pub mod param_card;
pub mod param_slider_shared;
pub mod perf_hud;
pub mod stem_lane;
pub mod transport;
pub mod viewport;
pub mod waveform_lane;

use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::Color32;
use crate::tree::UITree;
use manifold_core::{AudioSendId, ClipId, LayerId};
pub use viewport::HitRegion;

/// A stable, distinct identity color for an audio send, derived from its id so
/// it survives reorders without any stored field. Used by the Audio Setup row
/// swatch and the per-slider audio drawer, so a slider driven by "Kick" reads
/// the same color in both places.
pub fn audio_send_color(id: &AudioSendId) -> Color32 {
    // Bright, well-separated hues — the same palette feel as track colors.
    const PALETTE: [(u8, u8, u8); 8] = [
        (236, 110, 110), // red
        (236, 168, 92),  // amber
        (224, 214, 96),  // yellow
        (130, 214, 124), // green
        (104, 206, 206), // teal
        (120, 168, 240), // blue
        (176, 142, 234), // violet
        (234, 134, 198), // pink
    ];
    // FNV-1a over the id bytes → stable index.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in id.as_str().bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let (r, g, b) = PALETTE[(hash as usize) % PALETTE.len()];
    Color32::new(r, g, b, 255)
}

/// Actions for driver configuration sub-panels.
#[derive(Debug, Clone)]
pub enum DriverConfigAction {
    BeatDiv(usize),
    Wave(usize),
    Dot,
    Triplet,
    Reverse,
}

/// Which graph host a per-param [`PanelAction`] targets — the
/// discriminator that replaced the parallel `Effect*` / `Gen*` variant
/// pairs, so a card action can't be emitted (or dispatched) for the wrong
/// kind by construction. `Effect` carries the chain-positional effect
/// index (the card's `effect_index`); `Generator` carries nothing (a layer
/// hosts one generator, resolved from the active layer at dispatch time,
/// exactly as the old `Gen*` arms did).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GraphParamTarget {
    Effect(usize),
    Generator,
}

/// Which modulator's output sub-range a trim-handle drag is editing. The three
/// kinds share one drag path, one set of `Trim*` actions, and one
/// [`reposition_trim_bars`](param_slider_shared::reposition_trim_bars) layout
/// helper; they differ only in which backing store the live drag reads/writes
/// (card-side) and which undo command the commit records (app-side):
///
/// | kind | card-side source | project store | commit command |
/// |------|------------------|---------------|----------------|
/// | `Driver`  | `mod_state.trim_min/max`          | `Driver.trim_min/max`        | `ChangeTrimCommand`        |
/// | `Ableton` | `param_info[pi].ableton_range`    | mapping `range_min/max`      | `ChangeAbletonTrimCommand` |
/// | `Audio`   | `mod_state.audio_range_min/max`   | `AudioModShape.range_min/max`| `SetAudioModShapeCommand`  |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimKind {
    Driver,
    Ableton,
    Audio,
}

/// Actions that panels emit to be handled by the app layer.
/// Panels never touch the engine directly — they fire actions
/// that the app wires to PlaybackEngine/EditingService.
#[derive(Debug, Clone)]
pub enum PanelAction {
    // Transport
    PlayPause,
    Stop,
    Record,
    ResetBpm,
    ClearBpm,
    BpmFieldClicked,

    // Clock/Sync
    CycleClockAuthority,
    ToggleLink,
    ToggleMidiClock,
    SelectClkDevice,
    ToggleSyncOutput,

    // File
    NewProject,
    OpenProject,
    OpenRecent,
    SaveProject,
    SaveProjectAs,

    // Export
    ExportVideo,
    ToggleHdr,
    ExportXml,
    TogglePercussion,

    // Header
    ZoomIn,
    ZoomOut,
    ToggleLiveRecording,
    SelectAudioInputDevice,
    SetAudioInputDevice(String),
    ToggleMonitor,
    EnterPerformMode,
    /// Open (toggle) the Audio Setup panel — the central place to route audio
    /// in and define named sends. Header button; also bound to ⌘⇧A.
    OpenAudioSetup,

    // Footer
    CycleQuantize,
    ResolutionClicked,
    FpsFieldClicked,

    // Inspector tab
    SelectInspectorTab(InspectorTab),

    // Layer
    ToggleMute(usize),
    ToggleSolo(usize),
    ToggleLed(usize),
    SetBlendMode(usize, String),
    ExpandLayer(usize),
    CollapseLayer(usize),

    // Layer header
    LayerClicked(usize, crate::input::Modifiers),
    LayerDoubleClicked(usize),
    ChevronClicked(usize),
    BlendModeClicked(usize),
    FolderClicked(usize),
    NewClipClicked(usize),
    AddGenClipClicked(usize),
    MidiInputClicked(usize),
    MidiChannelClicked(usize),
    MidiDeviceClicked(usize),
    MidiTriggerModeClicked(usize),
    LayerDragStarted(usize),
    LayerDragMoved(usize, usize),
    LayerDragEnded(usize, usize),

    // Inspector chrome — Master
    MasterCollapseToggle,
    MasterExitPathClicked,
    /// Set LED exit path index: -1 = after all FX, 0 = before FX, N = after effect N-1.
    SetLedExitIndex(i32),
    MasterOpacitySnapshot,
    MasterOpacityChanged(f32),
    MasterOpacityCommit,
    MasterOpacityRightClick,

    // Inspector chrome — LED
    LedEnabledToggle,
    LedBrightnessSnapshot,
    LedBrightnessChanged(f32),
    LedBrightnessCommit,
    LedBrightnessRightClick,

    // Inspector chrome — Layer
    LayerChromeCollapseToggle,
    LayerOpacitySnapshot,
    LayerOpacityChanged(f32),
    LayerOpacityCommit,
    LayerOpacityRightClick,

    // Inspector chrome — Clip
    ClipChromeCollapseToggle,
    ClipBpmClicked,
    ClipLoopToggle,
    ClipSlipSnapshot,
    ClipSlipChanged(f32),
    ClipSlipCommit,
    ClipSlipRightClick,
    ClipLoopSnapshot,
    ClipLoopChanged(f32),
    ClipLoopCommit,
    ClipLoopRightClick,

    // Effect card (effect_index, param_index where applicable)
    EffectToggle(usize),
    EffectCollapseToggle(usize),
    EffectCardClicked(usize),
    /// Open the node-graph editor for this effect (cog icon click).
    /// Currently shows a hardcoded test graph regardless of which effect
    /// triggered it; live data sync lands in a future phase.
    OpenGraphEditor(usize),
    /// Open the sideways mapping drawer for an effect user-tail binding
    /// (Author-context card, right-edge chevron). Carries the binding's stable
    /// `param_id`; the host resolves its current range/scale/offset/invert/curve
    /// from the edited effect and anchors the drawer beside the row. Editor-only:
    /// the perform inspector never emits it (the chevron is Author-context).
    OpenCardMapping(manifold_core::effects::ParamId),
    // ── Per-effect-param actions ────────────────────────────────────
    //
    // Every variant in this block carries `(fx_idx: usize, param_id: ParamId)`
    // — `fx_idx` is the chain-positional effect index (structural), and
    // `param_id` identifies the parameter by its stable id, never by
    // positional `pi`. The `ParamId` namespace is shared between
    // registry-declared static params and per-instance user-exposed
    // bindings (`PresetInstance.user_param_bindings[].id`), so the
    // bridge handler walks both tiers transparently without a
    // tier-aware lookup. Pre-Phase-2 these variants carried `pi: usize`
    // and the bridge resolved via the static-tier-only
    // `param_index_to_id` — the bug class that left user-exposed
    // sliders dead for drivers / envelopes / Ableton mapping.
    //
    // Phase-D unification: these were parallel `Effect*` / `Gen*` pairs;
    // each is now one variant carrying a [`GraphParamTarget`] so a card
    // action can't be emitted or dispatched for the wrong kind. The
    // dispatch matches `Effect(fx_idx)` and `Generator` as separate arms,
    // so the two bodies stay distinct where they genuinely differ.
    ParamSnapshot(GraphParamTarget, manifold_core::effects::ParamId),
    ParamChanged(GraphParamTarget, manifold_core::effects::ParamId, f32),
    ParamCommit(GraphParamTarget, manifold_core::effects::ParamId),
    ParamRightClick(GraphParamTarget, manifold_core::effects::ParamId, f32), // target, param_id, default_value
    DriverToggle(GraphParamTarget, manifold_core::effects::ParamId),
    EnvelopeToggle(GraphParamTarget, manifold_core::effects::ParamId),
    DriverConfig(GraphParamTarget, manifold_core::effects::ParamId, DriverConfigAction),
    /// Arm/disarm audio modulation on a param. Arming assigns the project's
    /// first audio send with a default feature; re-clicking toggles enabled.
    /// No-op when no sends exist (the audio button is inert until the Audio
    /// Setup defines one). See `docs/AUDIO_MODULATION_DESIGN.md`.
    AudioModToggle(GraphParamTarget, manifold_core::effects::ParamId),
    /// Set an audio modulation's source: which send + which feature.
    AudioModSetSource(
        GraphParamTarget,
        manifold_core::effects::ParamId,
        manifold_core::AudioSendId,
        manifold_core::AudioFeature,
    ),
    /// Remove the audio modulation from a param.
    AudioModRemove(GraphParamTarget, manifold_core::effects::ParamId),
    /// Toggle an audio modulation's invert flag (`AudioModShape::invert`) — the
    /// drawer's "Inv" button (loud → low).
    AudioModSetInvert(GraphParamTarget, manifold_core::effects::ParamId),
    /// Toggle an audio modulation's rate-of-change flag
    /// (`AudioModShape::rate_of_change`) — the drawer's "d/dt" button; the
    /// feature drives on its motion rather than its level.
    AudioModSetRateOfChange(GraphParamTarget, manifold_core::effects::ParamId),

    // ── Audio Setup panel (project-level send routing) ──
    /// Open the input-device dropdown (anchored to the clicked trigger).
    AudioSetupDeviceClicked,
    /// Open a send's input-channel dropdown (anchored to the clicked trigger).
    AudioSendChannelClicked(manifold_core::AudioSendId),
    /// Set (or clear) the capture input device. `None` = system default input.
    AudioSetDevice(Option<manifold_core::AudioDeviceRef>),
    /// Add a new empty send.
    AudioAddSend,
    /// Remove a send by id.
    AudioRemoveSend(manifold_core::AudioSendId),
    /// Rename a send (commit with the new label).
    AudioRenameSend(manifold_core::AudioSendId, String),
    /// Begin inline editing of a send's label (clicked its name).
    AudioSendLabelClicked(manifold_core::AudioSendId),
    /// Set a send's input channels (downmixed to mono for analysis).
    AudioSetSendChannels(manifold_core::AudioSendId, Vec<u16>),
    /// Toggle a send between mono (one channel) and stereo (a channel pair).
    AudioSendStereoToggle(manifold_core::AudioSendId),
    /// Step a send's input gain trim by a dB delta (the panel's −/＋ buttons).
    /// The host reads the send's current gain, applies the delta, clamps, and
    /// commits — so the project stays the single source of truth.
    AudioSendGainStep(manifold_core::AudioSendId, f32),
    /// A modulator output sub-range handle moved during a drag. `TrimKind`
    /// selects which modulator (driver / Ableton / audio) — the three formerly
    /// parallel `*TrimChanged` variants are one path now.
    TrimChanged(TrimKind, GraphParamTarget, manifold_core::effects::ParamId, f32, f32),
    /// Snapshot trim state before drag (for undo).
    TrimSnapshot(TrimKind, GraphParamTarget, manifold_core::effects::ParamId),
    /// Commit trim drag (record undo command).
    TrimCommit(TrimKind, GraphParamTarget, manifold_core::effects::ParamId),
    /// Envelope target (orange handle / `target_normalized`) changed.
    TargetChanged(GraphParamTarget, manifold_core::effects::ParamId, f32),
    /// Snapshot target before drag (for undo).
    TargetSnapshot(GraphParamTarget, manifold_core::effects::ParamId),
    /// Commit target drag (record undo command).
    TargetCommit(GraphParamTarget, manifold_core::effects::ParamId),
    /// Envelope decay slider (`decay_beats`) changed.
    EnvDecayChanged(GraphParamTarget, manifold_core::effects::ParamId, f32),
    /// Snapshot decay before drag (for undo).
    EnvDecaySnapshot(GraphParamTarget, manifold_core::effects::ParamId),
    /// Commit decay drag (record undo command).
    EnvDecayCommit(GraphParamTarget, manifold_core::effects::ParamId),
    /// Reorder effect card: move from_index to to_index.
    /// Unity: EffectsListBitmapPanel.onCardReorder.
    EffectReorder(usize, usize),
    /// Reorder multiple effect cards as a group: (sorted source indices, target index).
    EffectReorderGroup(Vec<usize>, usize),
    /// Toggle whether an inner-graph param is exposed on the outer
    /// card. **Single variant for both Effect-hosted and Generator-
    /// hosted graphs** — the graph editor is one surface and the click
    /// handler emits this regardless of target. Dispatch resolves the
    /// watched `GraphTarget` and routes to `ToggleNodeParamExposeCommand`.
    ///
    /// `label` / `min` / `max` / `default_value` / `convert` are the
    /// inner-node ParamDef metadata captured at panel-build time. They
    /// feed both the synthesised user binding (when the param has no
    /// preset binding) and the undo restore path. Reading them in the
    /// UI thread keeps the renderer registry off the click hot path.
    ToggleNodeParamExpose {
        /// Stable id of the inner node — the addressing identity the
        /// expose command stores. Sourced from the node's snapshot.
        node_id: manifold_core::NodeId,
        /// Current display handle, carried only so the command can mint a
        /// readable `user.<handle>.<param>.<n>` id.
        node_handle: String,
        inner_param: String,
        expose: bool,
        label: String,
        min: f32,
        max: f32,
        default_value: f32,
        convert: manifold_core::effects::ParamConvert,
        /// Angle presentation hint, from the inner param's
        /// `GraphEditorParamKind::Angle`. Carried onto the appended
        /// `UserParamBinding` so the card slider shows degrees.
        is_angle: bool,
        /// Enum option labels from the inner param's live `ParamDef`. Carried
        /// onto the appended `UserParamBinding` (and its `ParamSpecDef`) so an
        /// exposed enum renders as a labelled stepped card slider. Empty for
        /// non-enum params.
        value_labels: Vec<String>,
    },

    // ── User param-binding mapping edits ──────────────────────────────
    //
    // Emitted by the graph-editor mapping sidebar when the user edits a
    // `UserParamBinding`'s card-slider mapping (display label, min/max
    // range, invert flag, response curve). The app layer resolves the
    // watched effect by id from `watched_graph_target` and routes to
    // `EditUserParamBindingCommand`, addressing the binding by its stable
    // `binding_id` (never mutated).
    //
    // The min/max range uses the snapshot/changed/commit triad so a drag
    // coalesces into ONE undo entry: snapshot captures the pre-drag
    // value at drag start, changed writes the live value each frame
    // (no undo command), commit records the single command on release.
    // Label / invert / curve are discrete (text-entry / single-click /
    // cycle), so each fires its own one-shot edit command directly.
    /// Snapshot the binding's `(min, max)` before a range drag begins.
    EffectMappingRangeSnapshot {
        binding_id: String,
    },
    /// Live `(min, max)` update during a range drag — writes the local
    /// project + content thread but records no undo command.
    EffectMappingRangeChanged {
        binding_id: String,
        min: f32,
        max: f32,
    },
    /// Commit a range drag — records the single `EditUserParamBinding`
    /// undo command spanning the whole drag.
    EffectMappingRangeCommit {
        binding_id: String,
    },
    /// Set the binding's display label. One-shot edit (one undo entry).
    EffectMappingLabel {
        binding_id: String,
        label: String,
    },
    /// Set the binding's card-slider invert flag. One-shot edit.
    EffectMappingInvert {
        binding_id: String,
        invert: bool,
    },
    /// Set the binding's response curve. One-shot edit.
    EffectMappingCurve {
        binding_id: String,
        curve: manifold_core::macro_bank::MacroCurve,
    },
    /// Snapshot the binding's pre-drag (scale, offset) before a
    /// scale/offset scrub, so the matching commit records one undo entry
    /// for the whole drag. Mirrors `EffectMappingRangeSnapshot`.
    EffectMappingAffineSnapshot {
        binding_id: String,
    },
    /// Live scale/offset drag: update the binding's card→consumer affine
    /// remap (`out = value * scale + offset`) without recording undo.
    EffectMappingAffineChanged {
        binding_id: String,
        scale: f32,
        offset: f32,
    },
    /// Scale/offset drag release: record one `EditUserParamBindingCommand`
    /// spanning the whole drag.
    EffectMappingAffineCommit {
        binding_id: String,
    },
    /// Jump the graph-editor canvas to the node this card binding is exposed
    /// from — "show me where this slider is mapped from." Read-only navigation
    /// (no undo, no model write); the app resolves the binding's stable
    /// `NodeId` from the snapshot and centres the canvas on it.
    EffectMappingGotoNode {
        binding_id: String,
    },

    // ── Graph editor mutations (Phase 4) ──────────────────────────────
    //
    // Sent by the graph-editor canvas + palette. The app layer resolves
    // each into the matching command from
    // `manifold_editing::commands::graph` using the watched effect's
    // EffectId and catalog default.
    /// Add a new node of `type_id` to the watched graph at the canvas
    /// center. Emitted by clicking an entry in the palette.
    AddGraphNode {
        type_id: String,
    },
    /// Open the node picker over the canvas, anchored at `screen_pos`, to
    /// spawn the chosen node at `graph_pos`. Emitted by a double-click on
    /// empty canvas space. The app resolves the spawn into an
    /// `AddGraphNodeAt` once a node is picked.
    OpenNodePicker {
        screen_pos: (f32, f32),
        graph_pos: (f32, f32),
    },
    /// Add a new node of `type_id` at a specific `graph_pos`. Emitted after
    /// a node is chosen in the picker (the positioned sibling of
    /// `AddGraphNode`, which drops at a fixed canvas spot).
    AddGraphNodeAt {
        type_id: String,
        graph_pos: (f32, f32),
    },
    /// Connect an output port to an input port. Emitted by the
    /// wire-drag completion path on the canvas.
    ConnectPorts {
        from_node: u32,
        from_port: String,
        to_node: u32,
        to_port: String,
    },
    /// Remove a node from the watched graph plus every wire that
    /// touches it. Emitted by the canvas's delete-key handler.
    RemoveGraphNode {
        node_id: u32,
    },
    /// Disconnect the wire feeding `(to_node, to_port)`. The input
    /// side uniquely identifies the wire because each input port has
    /// at most one incoming wire. Emitted by clicking on an already-
    /// connected input port on the canvas.
    DisconnectPorts {
        to_node: u32,
        to_port: String,
    },
    /// Revert the watched effect's graph to the bundled preset
    /// (`instance.graph = None`). Emitted by the "Reset to Default"
    /// button in the graph editor header when the card is diverged
    /// from the bundle. The "library picker" affordance from §6.6 #30
    /// — bundled presets are the only "library" today; user-saved
    /// named presets will plug into the same dispatch path when added.
    RevertEffectGraph,
    /// Update a node's editor position. Emitted by the canvas's
    /// node-drag completion path.
    MoveGraphNode {
        node_id: u32,
        new_pos: (f32, f32),
    },
    /// Re-position every node at `scope_path` in one undoable step. Emitted
    /// by the canvas's Tidy command (Cmd+L), which runs the layered
    /// auto-layout over the current level and ships the resulting positions
    /// here. Routed to `LayoutGraphNodesCommand`.
    RelayoutGraph {
        scope_path: Vec<u32>,
        positions: Vec<(u32, (f32, f32))>,
    },
    /// Set an inner-node parameter to a new value. Emitted by the
    /// right-sidebar inspector when the user clicks a Bool toggle,
    /// cycles an Enum cell, or drag-scrubs a Float/Int value. The
    /// `node_id` matches the graph-editor canvas's stable node id;
    /// `new_value` is already coerced to the inner param's expected
    /// kind (Float / Int / Bool / Enum), so the app-side handler can
    /// hand it straight to `SetGraphNodeParamCommand`.
    SetGraphNodeParam {
        node_id: u32,
        param_name: String,
        new_value: manifold_core::effect_graph_def::SerializedParamValue,
    },
    /// Open a native folder picker for a path-like String param and set it to
    /// the chosen path. Emitted by the inspector's Browse button; the host runs
    /// the (blocking) dialog and dispatches a `SetGraphNodeParam` String value.
    BrowseGraphNodePath {
        node_id: u32,
        param_name: String,
    },
    /// Open the inline text editor over a free-text String param's value cell
    /// (e.g. `render_text.text`). Emitted when the value cell is clicked; the
    /// host begins a `GraphStringParam` text-input session anchored at `anchor`
    /// (`x, y, w, h` in logical px) seeded with `current`. Commit routes back
    /// through `SetGraphNodeParamCommand` with a String value.
    EditGraphNodeStringParam {
        node_id: u32,
        param_name: String,
        current: String,
        anchor: (f32, f32, f32, f32),
    },
    /// Open the multiline WGSL code editor over the selected `wgsl_compute`
    /// node's kernel source. Emitted by the inspector's "Edit Code" button; the
    /// host begins a `GraphWgsl` text-input session anchored at `anchor`
    /// (`x, y, w, h`) seeded with `current`. Commit routes to `SetWgslSourceCommand`.
    EditGraphNodeWgsl {
        node_id: u32,
        current: String,
        anchor: (f32, f32, f32, f32),
    },
    /// Open the inline numeric editor over one cell of a `Table` param's grid
    /// (gradient stop / numeric sequence). Emitted when a grid cell is clicked.
    /// The host begins a `GraphTableCell` session anchored at `anchor` seeded
    /// with `current`, and stashes `rows` + `(row, col)` so commit can rebuild
    /// the one edited cell into a full `Table` value through
    /// `SetGraphNodeParamCommand`.
    EditGraphNodeTableCell {
        node_id: u32,
        param_name: String,
        row: usize,
        col: usize,
        current: f32,
        rows: Vec<Vec<f32>>,
        anchor: (f32, f32, f32, f32),
    },
    /// Collapse a set of nodes at `scope_path` (the canvas's current view
    /// depth, a path of group ids; empty = document root) into a single group
    /// node. Emitted by Ctrl+G on a canvas selection. `handle` is the new
    /// group's stable handle — auto-named and collision-free at its level;
    /// `centroid` is the graph-space point the group node drops at. Routed to
    /// `GroupNodesCommand`.
    GroupSelection {
        scope_path: Vec<u32>,
        node_ids: Vec<u32>,
        handle: String,
        centroid: (f32, f32),
    },
    /// Dissolve a group node at `scope_path` back into its level, splicing its
    /// body in where the group sat. Emitted by Ctrl+Shift+G on a selected
    /// group. Routed to `UngroupNodeCommand`.
    Ungroup {
        scope_path: Vec<u32>,
        group_id: u32,
    },
    /// Set (or clear) the accent colour of a group node at `scope_path`.
    /// Emitted by the recolour gesture on a selected group; routed to
    /// `SetGroupTintCommand`. Cosmetic only — `None` restores the default tint.
    SetGroupTint {
        scope_path: Vec<u32>,
        group_id: u32,
        tint: Option<[f32; 4]>,
    },
    /// Flip auto-gain/normalization on the editor's node-output preview pane.
    /// `on` is the new state. Emitted by the toggle under the preview; routed to
    /// `ContentCommand::SetNodePreviewNormalize`. Node preview only.
    SetNodePreviewNormalize(bool),

    // ── Generator-only per-param actions ───────────────────────────────
    //
    // The effect/generator mirror pairs (snapshot / changed / commit /
    // right-click, drivers, envelope Amount, trims) collapsed into the
    // target-carrying `ParamSnapshot` … `TargetCommit` variants above. Only
    // these have no effect counterpart and stay generator-only.
    GenTypeClicked(Option<LayerId>), // layer_id
    GenParamToggle(manifold_core::effects::ParamId),
    /// Outer-card click on a `is_trigger` param's button — increment the
    /// underlying monotonic counter by one. Same write path as a toggle
    /// (`ChangeGraphParamCommand` on the generator), but `+1` instead of a
    /// `0↔1` flip. Wired in [`crate::panels::param_card`].
    GenParamFire(manifold_core::effects::ParamId),

    // Generator string params (per-clip text, etc.)
    GenStringParamClicked(usize), // string_param_index — open text input
    GenStringParamDropdownClicked(usize), // string_param_index — open dropdown selector
    GenStringParamSelected(usize, String), // string_param_index, selected value

    // Generator card actions
    GenCollapseToggle,
    GenCardClicked,
    /// User clicked the "open graph editor" affordance on the
    /// generator card header. Mirror of
    /// [`PanelAction::OpenGraphEditor`] for effects — the host opens
    /// the graph editor scoped to the currently-selected layer's
    /// generator.
    OpenGeneratorGraphEditor,
    CopyGenerator,
    PasteGenerator,

    // Card context menu + fork-preset actions (effect OR generator, addressed
    // by the unified GraphParamTarget). The card-right-click opens a context
    // menu whose CONTENTS may differ per kind (generators add Copy/Paste), but
    // the fork / export / import actions and their dispatch are one path.
    /// Right-click on a preset card header → open its context menu.
    CardRightClicked(GraphParamTarget),
    /// Fork the targeted preset into a project-embedded copy and retarget the
    /// instance to it ("make unique"), so a per-instance recalibration becomes
    /// a named, shareable variant.
    MakePresetUnique(GraphParamTarget),
    /// Export the targeted preset's graph to a `.json` file (native save dialog,
    /// writes via `manifold_io::preset_file`).
    ExportPreset(GraphParamTarget),
    /// Import a `.json` preset file as a project-embedded preset and retarget
    /// the targeted instance to it (native open dialog).
    ImportPreset(GraphParamTarget),

    // Macros panel collapse
    MacrosCollapseToggle,

    // Macro sliders (macro_index 0-7)
    MacroSnapshot(usize),
    MacroChanged(usize, f32),
    MacroCommit(usize),
    MacroRightClick(usize),
    MacroLabelRightClick(usize), // macro_index — opens mappings dropdown
    MacroLabelRename(usize),     // macro_index — opens inline rename input

    // Macro mapping (from context menu on param right-click). Param
    // is addressed by `ParamId`, macro slot by positional index
    // (macros are a fixed 8-slot bank).
    MapParamToMacro(GraphParamTarget, manifold_core::effects::ParamId, usize), // gpt, param_id, macro_idx
    UnmapMacro(usize, usize),                                                  // macro_idx, mapping_idx
    ClearMacroMappings(usize),                                                 // macro_idx

    // Param label right-click → opens "Map to Macro" / "Map from Ableton" context menu
    ParamLabelRightClick(GraphParamTarget, manifold_core::effects::ParamId),

    // Ableton mapping (from context menu on param right-click). Map + unmap
    // both address the param by the unified `GraphParamTarget`; the dispatch
    // resolves the `AbletonMappingTarget` through the one shared helper.
    MapParamToAbleton(
        GraphParamTarget,
        manifold_core::effects::ParamId,
        manifold_core::ableton_mapping::AbletonMacroAddress,
    ), // gpt, param_id, address
    UnmapParamAbleton(GraphParamTarget, manifold_core::effects::ParamId), // gpt, param_id
    /// Open the Ableton picker popup for a param (effect or generator).
    OpenAbletonPickerForParam(GraphParamTarget, manifold_core::effects::ParamId), // gpt, param_id
    /// Ableton mapping for macro slots.
    MapMacroToAbleton(usize, manifold_core::ableton_mapping::AbletonMacroAddress),
    UnmapMacroAbleton(usize),
    OpenAbletonPickerForMacro(usize),

    // Driver / Ableton / audio trim handles are unified into the `Trim*`
    // triad above (carrying `TrimKind`). Macro-slot trims stay separate: they
    // live on the macro bank, addressed by a positional `slot_idx`, not a
    // graph param.
    AbletonMacroTrimSnapshot(usize),                                      // slot_idx
    AbletonMacroTrimChanged(usize, f32, f32),                             // slot_idx, min, max
    AbletonMacroTrimCommit(usize),                                        // slot_idx

    // Ableton config actions
    AbletonInvertToggle(GraphParamTarget, manifold_core::effects::ParamId),
    AbletonMacroInvertToggle(usize),                             // slot_idx

    // Reset macro from context menu (distinct from MacroRightClick to avoid re-triggering dropdown)
    MacroReset(usize), // macro_idx — reset to 0 from context menu

    // Inspector scroll
    InspectorScrolled(f32),
    InspectorSectionClicked(usize),

    // Timeline
    Seek(f32),
    SetInsertCursor(f32),
    /// Overview strip scrub — normalized [0,1] position. Centers viewport.
    OverviewScrub(f32),

    // Viewport clip interaction (generated by InteractionOverlay, not panels)
    ClipClicked(String, Modifiers), // clip_id, modifiers (for Ctrl detection)
    ClipDoubleClicked(String),      // clip_id
    TrackClicked(f32, usize, Modifiers), // beat, layer, modifiers
    TrackDoubleClicked(f32, usize), // beat, layer (create clip)
    ViewportHoverChanged(Option<ClipId>), // clip_id or None
    ClipRightClicked(String),       // clip_id (context menu)
    TrackRightClicked(f32, usize),  // beat, layer (context menu)

    // Layer management
    AddLayerClicked,
    DeleteLayerClicked(usize),

    // Effect management
    AddEffectClicked(InspectorTab),
    RemoveEffect(usize),
    BrowserSearchClicked,
    PasteEffects,

    // OSC — click param label to copy address to system clipboard.
    // Unity: UIElementBuilder.CopyToClipboardLabel.
    CopyOscAddress(String),

    // Dropdown results (context-routed from UIRoot)
    SetMidiNote(usize, i32),              // layer_index, note (0-127)
    SetMidiChannel(usize, i32),           // layer_index, channel (0-15 internal, displayed 1-16)
    SetMidiDevice(usize, Option<String>), // layer_index, device name (None = any)
    SetMidiTriggerMode(usize, manifold_core::types::MidiTriggerMode),
    SetResolution(usize),           // preset index
    SetDisplayResolution(i32, i32), // direct width, height (no undo, matches Unity)
    SetRenderScale(f32),            // render scale: 1.0 (native), 0.75 (quality), 0.5 (performance)
    SetTonemapCurve(manifold_core::TonemapCurve),
    AddEffect(InspectorTab, manifold_core::PresetTypeId), // tab, preset type id
    SetGenType(Option<LayerId>, manifold_core::PresetTypeId), // layer_id, preset type id
    SetMidiClockDevice(i32),            // MIDI device index

    // Layer header right-click
    LayerHeaderRightClicked(usize), // layer_index

    // Context menu results
    ContextSplitAtPlayhead(String),  // clip_id
    ContextDeleteClip(String),       // clip_id
    ContextDuplicateClip(String),    // clip_id
    ContextPasteAtTrack(f32, usize), // beat, layer
    ContextAddVideoLayer(usize),     // after_layer
    ContextAddGeneratorLayer(usize), // after_layer
    ContextDeleteLayer(usize),       // layer_index
    ContextDuplicateLayer(usize),    // layer_index
    ContextPasteAtLayer(usize),      // layer_index
    ContextImportMidi(usize),        // layer_index
    ContextGroupSelectedLayers,
    ContextUngroup(usize),                             // layer_index
    ContextSetLayerColor(usize, crate::node::Color32), // layer_index, color

    // Timeline Markers
    MarkerClicked(String, Modifiers), // marker_id, modifiers (Shift for multi-select)
    MarkerDoubleClicked(String),      // marker_id (rename)
    MarkerDragStarted(String),        // marker_id
    MarkerDragMoved(String, f32),     // marker_id, new_beat
    MarkerDragEnded(String, f32),     // marker_id, final_beat
    MarkerRightClicked(String),       // marker_id (context menu)
    DeleteSelectedMarkers,

    // Waveform lane
    ImportAudioClicked,
    RemoveAudioClicked,
    WaveformScrub(f32, f32),  // screen_x, screen_y
    WaveformDragDelta(f32),   // delta_beats (snapped to whole beats)
    WaveformDragEnd(f32),     // total_snapped_delta
    ExpandStemsToggled(bool), // expanded

    // Re-analysis buttons (UI chrome — callbacks to percussion pipeline)
    ReAnalyzeDrums,
    ReAnalyzeBass,
    ReAnalyzeSynth,
    ReAnalyzeVocal,
    ReImportStems,

    // Stem mute/solo
    StemMuteToggled(usize), // stem_index (0-3)
    StemSoloToggled(usize), // stem_index (0-3)

    // Generic dropdown fallback (should not normally reach dispatch)
    DropdownSelected(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncSource {
    Internal,
    AbletonLink,
    Midi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Master,
    Layer,
    Clip,
}

/// Trait for all UI panels.
///
/// Lifecycle:
///   1. `build()` — create all nodes in the tree (called once or on rebuild)
///   2. `update()` — push state changes to existing nodes (called each frame)
///   3. `handle_event()` — process UI events, return actions for the app layer
pub trait Panel {
    /// Build all nodes for this panel into the tree.
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout);

    /// Push state updates to existing nodes (text, colors, visibility).
    fn update(&mut self, tree: &mut UITree);

    /// Handle a UI event. Returns actions for the app layer to process.
    fn handle_event(&mut self, event: &UIEvent, tree: &UITree) -> Vec<PanelAction>;

    /// First node index in the tree. Returns usize::MAX if not built.
    fn first_node(&self) -> usize {
        usize::MAX
    }

    /// Number of nodes this panel owns.
    fn node_count(&self) -> usize {
        0
    }

    /// Node range as (start, end). Convenience wrapper.
    fn node_range(&self) -> (usize, usize) {
        let first = self.first_node();
        if first == usize::MAX {
            return (0, 0);
        }
        (first, first + self.node_count())
    }
}
