pub mod ableton_picker;
pub mod browser_popup;
pub mod clip_chrome;
pub mod copy_to_clipboard_label;
pub mod dropdown;
pub mod footer;
pub mod graph_card_mirror;
pub mod graph_editor;
pub mod graph_palette;
pub mod header;
pub mod inspector;
pub mod layer_chrome;
pub mod layer_header;
pub mod macros_panel;
pub mod master_chrome;
pub mod param_card;
pub mod param_slider_shared;
pub mod perf_hud;
pub mod stem_lane;
pub mod transport;
pub mod viewport;
pub mod waveform_lane;

use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::tree::UITree;
use manifold_core::{ClipId, LayerId};
pub use viewport::HitRegion;

/// Actions for driver configuration sub-panels.
#[derive(Debug, Clone)]
pub enum DriverConfigAction {
    BeatDiv(usize),
    Wave(usize),
    Dot,
    Triplet,
    Reverse,
}

/// ADSR envelope parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeParam {
    Attack,
    Decay,
    Sustain,
    Release,
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
    // ── Per-effect-param actions ────────────────────────────────────
    //
    // Every variant in this block carries `(fx_idx: usize, param_id: ParamId)`
    // — `fx_idx` is the chain-positional effect index (structural), and
    // `param_id` identifies the parameter by its stable id, never by
    // positional `pi`. The `ParamId` namespace is shared between
    // registry-declared static params and per-instance user-exposed
    // bindings (`EffectInstance.user_param_bindings[].id`), so the
    // bridge handler walks both tiers transparently without a
    // tier-aware lookup. Pre-Phase-2 these variants carried `pi: usize`
    // and the bridge resolved via the static-tier-only
    // `param_index_to_id` — the bug class that left user-exposed
    // sliders dead for drivers / envelopes / Ableton mapping.
    EffectParamSnapshot(usize, manifold_core::effects::ParamId),
    EffectParamChanged(usize, manifold_core::effects::ParamId, f32),
    EffectParamCommit(usize, manifold_core::effects::ParamId),
    EffectParamRightClick(usize, manifold_core::effects::ParamId, f32), // fx_idx, param_id, default_value
    EffectDriverToggle(usize, manifold_core::effects::ParamId),
    EffectEnvelopeToggle(usize, manifold_core::effects::ParamId),
    EffectDriverConfig(usize, manifold_core::effects::ParamId, DriverConfigAction),
    EffectEnvParamChanged(usize, manifold_core::effects::ParamId, EnvelopeParam, f32),
    /// Snapshot ADSR state before drag (for undo). Unity: onEnvConfigSnapshot.
    EffectEnvParamSnapshot(usize, manifold_core::effects::ParamId),
    /// Commit ADSR drag (record undo command). Unity: onEnvConfigCommit.
    EffectEnvParamCommit(usize, manifold_core::effects::ParamId),
    /// Toggle envelope mode between ADSR and Random.
    EffectEnvModeToggle(usize, manifold_core::effects::ParamId),
    /// Toggle random_jump flag on a Random-mode envelope.
    EffectEnvRandomJumpToggle(usize, manifold_core::effects::ParamId),
    EffectTrimChanged(usize, manifold_core::effects::ParamId, f32, f32),
    /// Snapshot trim state before drag (for undo). Unity: onTrimSnapshot.
    EffectTrimSnapshot(usize, manifold_core::effects::ParamId),
    /// Commit trim drag (record undo command). Unity: onTrimCommit.
    EffectTrimCommit(usize, manifold_core::effects::ParamId),
    EffectTargetChanged(usize, manifold_core::effects::ParamId, f32),
    /// Snapshot target state before drag (for undo). Unity: onTargetSnapshot.
    EffectTargetSnapshot(usize, manifold_core::effects::ParamId),
    /// Commit target drag (record undo command). Unity: onTargetCommit.
    EffectTargetCommit(usize, manifold_core::effects::ParamId),
    /// Envelope range changed: fx_idx, param_id, range_min, range_max.
    EffectEnvRangeChanged(usize, manifold_core::effects::ParamId, f32, f32),
    /// Snapshot envelope range before drag (for undo).
    EffectEnvRangeSnapshot(usize, manifold_core::effects::ParamId),
    /// Commit envelope range drag (record undo command).
    EffectEnvRangeCommit(usize, manifold_core::effects::ParamId),
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
    },

    // ── User param-binding mapping edits ──────────────────────────────
    //
    // Emitted by the graph-editor mapping sidebar when the user edits a
    // `UserParamBinding`'s card-slider mapping (display label, min/max
    // range, invert flag, response curve). The app layer resolves the
    // watched effect target + index from `current_editor_target` and
    // routes to `EditUserParamBindingCommand`, addressing the binding by
    // its stable `binding_id` (never mutated).
    //
    // The min/max range uses the snapshot/changed/commit triad so a drag
    // coalesces into ONE undo entry: snapshot captures the pre-drag
    // value at drag start, changed writes the live value each frame
    // (no undo command), commit records the single command on release.
    // Label / invert / curve are discrete (text-entry / single-click /
    // cycle), so each fires its own one-shot edit command directly.
    /// Snapshot the binding's `(min, max)` before a range drag begins.
    EffectMappingRangeSnapshot { binding_id: String },
    /// Live `(min, max)` update during a range drag — writes the local
    /// project + content thread but records no undo command.
    EffectMappingRangeChanged {
        binding_id: String,
        min: f32,
        max: f32,
    },
    /// Commit a range drag — records the single `EditUserParamBinding`
    /// undo command spanning the whole drag.
    EffectMappingRangeCommit { binding_id: String },
    /// Set the binding's display label. One-shot edit (one undo entry).
    EffectMappingLabel { binding_id: String, label: String },
    /// Set the binding's card-slider invert flag. One-shot edit.
    EffectMappingInvert { binding_id: String, invert: bool },
    /// Set the binding's response curve. One-shot edit.
    EffectMappingCurve {
        binding_id: String,
        curve: manifold_core::macro_bank::MacroCurve,
    },

    // ── Graph editor mutations (Phase 4) ──────────────────────────────
    //
    // Sent by the graph-editor canvas + palette. The app layer resolves
    // each into the matching command from
    // `manifold_editing::commands::graph` using the watched effect's
    // EffectId and catalog default.
    /// Add a new node of `type_id` to the watched graph at the canvas
    /// center. Emitted by clicking an entry in the palette.
    AddGraphNode { type_id: String },
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
    RemoveGraphNode { node_id: u32 },
    /// Disconnect the wire feeding `(to_node, to_port)`. The input
    /// side uniquely identifies the wire because each input port has
    /// at most one incoming wire. Emitted by clicking on an already-
    /// connected input port on the canvas.
    DisconnectPorts { to_node: u32, to_port: String },
    /// Revert the watched effect's graph to the bundled preset
    /// (`instance.graph = None`). Emitted by the "Reset to Default"
    /// button in the graph editor header when the card is diverged
    /// from the bundle. The "library picker" affordance from §6.6 #30
    /// — bundled presets are the only "library" today; user-saved
    /// named presets will plug into the same dispatch path when added.
    RevertEffectGraph,
    /// Update a node's editor position. Emitted by the canvas's
    /// node-drag completion path.
    MoveGraphNode { node_id: u32, new_pos: (f32, f32) },
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

    // ── Per-generator-param actions ────────────────────────────────
    //
    // Mirror of the effect-side block but without `fx_idx` — a layer
    // owns at most one generator. `param_id` is always a static-tier
    // id today (generators don't expose user-tier bindings yet) but
    // the wire format matches the effect side for symmetry and future
    // extension.
    GenTypeClicked(Option<LayerId>), // layer_id
    GenParamSnapshot(manifold_core::effects::ParamId),
    GenParamChanged(manifold_core::effects::ParamId, f32),
    GenParamCommit(manifold_core::effects::ParamId),
    GenParamRightClick(manifold_core::effects::ParamId, f32), // param_id, default_value
    GenParamToggle(manifold_core::effects::ParamId),
    /// Outer-card click on a `is_trigger` param's button — increment
    /// the underlying monotonic counter by one. Consumed by the same
    /// `ChangeGeneratorParamsCommand` path as toggles, but with `+1`
    /// instead of `0↔1` flip. Wired in [`crate::panels::param_card`].
    GenParamFire(manifold_core::effects::ParamId),
    GenDriverToggle(manifold_core::effects::ParamId),
    GenEnvelopeToggle(manifold_core::effects::ParamId),
    GenDriverConfig(manifold_core::effects::ParamId, DriverConfigAction),
    GenEnvParamChanged(manifold_core::effects::ParamId, EnvelopeParam, f32),
    /// Snapshot ADSR state before drag (for undo). Unity: onEnvConfigSnapshot.
    GenEnvParamSnapshot(manifold_core::effects::ParamId),
    /// Commit ADSR drag (record undo command). Unity: onEnvConfigCommit.
    GenEnvParamCommit(manifold_core::effects::ParamId),
    /// Toggle envelope mode between ADSR and Random.
    GenEnvModeToggle(manifold_core::effects::ParamId),
    /// Toggle random_jump flag on a Random-mode envelope.
    GenEnvRandomJumpToggle(manifold_core::effects::ParamId),
    GenTrimChanged(manifold_core::effects::ParamId, f32, f32),
    /// Snapshot trim state before drag (for undo). Unity: onTrimSnapshot.
    GenTrimSnapshot(manifold_core::effects::ParamId),
    /// Commit trim drag (record undo command). Unity: onTrimCommit.
    GenTrimCommit(manifold_core::effects::ParamId),
    GenTargetChanged(manifold_core::effects::ParamId, f32),
    /// Snapshot target state before drag (for undo). Unity: onTargetSnapshot.
    GenTargetSnapshot(manifold_core::effects::ParamId),
    /// Commit target drag (record undo command). Unity: onTargetCommit.
    GenTargetCommit(manifold_core::effects::ParamId),
    /// Envelope range changed: param_id, range_min, range_max.
    GenEnvRangeChanged(manifold_core::effects::ParamId, f32, f32),
    /// Snapshot envelope range before drag (for undo).
    GenEnvRangeSnapshot(manifold_core::effects::ParamId),
    /// Commit envelope range drag (record undo command).
    GenEnvRangeCommit(manifold_core::effects::ParamId),

    // Generator string params (per-clip text, etc.)
    GenStringParamClicked(usize), // string_param_index — open text input
    GenStringParamDropdownClicked(usize), // string_param_index — open dropdown selector
    GenStringParamSelected(usize, String), // string_param_index, selected value

    // Generator card actions
    GenCollapseToggle,
    GenCardClicked,
    GenCardRightClicked,
    /// User clicked the "open graph editor" affordance on the
    /// generator card header. Mirror of
    /// [`PanelAction::OpenGraphEditor`] for effects — the host opens
    /// the graph editor scoped to the currently-selected layer's
    /// generator.
    OpenGeneratorGraphEditor,
    CopyGenerator,
    PasteGenerator,

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
    MapEffectParamToMacro(InspectorTab, usize, manifold_core::effects::ParamId, usize), // tab, fx_idx, param_id, macro_idx
    MapGenParamToMacro(manifold_core::effects::ParamId, usize), // param_id, macro_idx
    UnmapMacro(usize, usize),                                   // macro_idx, mapping_idx
    ClearMacroMappings(usize),                                  // macro_idx

    // Param label right-click → opens "Map to Macro" / "Map from Ableton" context menu
    EffectParamLabelRightClick(usize, manifold_core::effects::ParamId), // fx_idx, param_id
    GenParamLabelRightClick(manifold_core::effects::ParamId),           // param_id

    // Ableton mapping (from context menu on param right-click)
    MapEffectParamToAbleton(
        InspectorTab,
        usize,
        manifold_core::effects::ParamId,
        manifold_core::ableton_mapping::AbletonMacroAddress,
    ), // tab, fx_idx, param_id, address
    MapGenParamToAbleton(
        manifold_core::effects::ParamId,
        manifold_core::ableton_mapping::AbletonMacroAddress,
    ), // param_id, address
    UnmapEffectParamAbleton(InspectorTab, usize, manifold_core::effects::ParamId), // tab, fx_idx, param_id
    UnmapGenParamAbleton(manifold_core::effects::ParamId),                         // param_id
    /// Open the Ableton picker popup for an effect parameter.
    OpenAbletonPickerForEffect(InspectorTab, usize, manifold_core::effects::ParamId), // tab, fx_idx, param_id
    /// Open the Ableton picker popup for a generator parameter.
    OpenAbletonPickerForGen(manifold_core::effects::ParamId), // param_id
    /// Ableton mapping for macro slots.
    MapMacroToAbleton(usize, manifold_core::ableton_mapping::AbletonMacroAddress),
    UnmapMacroAbleton(usize),
    OpenAbletonPickerForMacro(usize),

    // Ableton trim handles (range_min / range_max adjustment).
    // Effect-side variants carry `param_id` (Phase 2 — see the per-param
    // action block above for the rationale). Gen-side and macro-side
    // keep their positional indices: generators have a single static
    // tier with no user-exposed extensions, and macro slots are
    // structurally positional in the 8-slot macro bank.
    AbletonTrimSnapshot(usize, manifold_core::effects::ParamId), // fx_idx, param_id
    AbletonTrimChanged(usize, manifold_core::effects::ParamId, f32, f32), // fx_idx, param_id, min, max
    AbletonTrimCommit(usize, manifold_core::effects::ParamId),   // fx_idx, param_id
    AbletonGenTrimSnapshot(manifold_core::effects::ParamId),     // param_id
    AbletonGenTrimChanged(manifold_core::effects::ParamId, f32, f32), // param_id, min, max
    AbletonGenTrimCommit(manifold_core::effects::ParamId),       // param_id
    AbletonMacroTrimSnapshot(usize),                             // slot_idx
    AbletonMacroTrimChanged(usize, f32, f32),                    // slot_idx, min, max
    AbletonMacroTrimCommit(usize),                               // slot_idx

    // Ableton config actions
    AbletonInvertToggle(usize, manifold_core::effects::ParamId), // fx_idx, param_id
    AbletonGenInvertToggle(manifold_core::effects::ParamId),     // param_id
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
    AddEffect(InspectorTab, usize),     // tab, effect_type index
    SetGenType(Option<LayerId>, usize), // layer_id, gen_type index
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
