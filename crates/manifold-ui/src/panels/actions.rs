//! Per-domain intent enums — the FLAT sum decomposition of the god-enum
//! [`super::PanelAction`] (UI_FUNNEL_DECOMPOSITION_DESIGN.md D5, P-D / D-D1).
//!
//! Each of the 12 enums below holds a disjoint slice of the original
//! `PanelAction` variants, moved VERBATIM (name, fields, doc comments, attrs).
//! `PanelAction` is now a thin sum with one arm per domain; `From<DomainAction>`
//! wraps a domain value into that arm. Partition source of truth:
//! `.claude/orchestration/pd-partition.md`.

use super::PanelAction;
use super::{
    AudioShapeParam, BandDivider, DriverConfigAction, GraphParamTarget, InspectorTab,
    TrimKind, UiRelightField, UiRelightHeightFrom,
};
use super::{browser_popup, picker_core};
use crate::input::Modifiers;
use crate::node::Rect;
use crate::types::{
    AbletonMacroAddress, AudioDeviceRef, AudioFeature, MacroCurve, MidiTriggerMode,
    PresetTypeId, TonemapCurve,
};
use crate::view::UiGraphTarget;
use manifold_foundation::{AudioSendId, Beats, ClipId, LayerId, ParamId};

#[derive(Debug, Clone)]
pub enum TransportAction {
    PlayPause,
    Stop,
    Record,
    ResetBpm,
    ClearBpm,
    BpmFieldClicked,
    CycleClockAuthority,
    ToggleLink,
    ToggleMidiClock,
    SelectClkDevice,
    ToggleSyncOutput,
    /// Toggle the global Automation Arm: while armed, touching an automated
    /// param (while playing) records into its lane instead of latching an
    /// override (§5).
    ToggleAutomationArm,
    /// Back to Arrangement: clears every automation override latch, resuming
    /// every automated param's lane (§4). Lit red in the transport bar
    /// whenever any latch is active.
    AutomationBackToArrangement,
    /// Show/hide lane strips across the timeline (Live's `A`) — a pure UI
    /// view-state toggle, not a project mutation or runtime playback state.
    /// Lit when lanes are currently visible.
    ToggleAutomationMode,
    ZoomIn,
    ZoomOut,
    CycleQuantize,
    ResolutionClicked,
    FpsFieldClicked,
    InspectorScrolled(f32),
    InspectorSectionClicked(usize),
    Seek(f32),
    SetInsertCursor(f32),
    /// Overview strip scrub — normalized [0,1] position. Centers viewport.
    OverviewScrub(f32),
    /// Horizontal scrollbar drag/jump — an absolute scroll-x in beats (§24 5e).
    TimelineScrollbarH(f32),
    SetMidiClockDevice(i32),            // MIDI device index
}

#[derive(Debug, Clone)]
pub enum EditingAction {
    ClipClicked(String, Modifiers), // clip_id, modifiers (for Ctrl detection)
    ClipDoubleClicked(String),      // clip_id
    TrackClicked(f32, usize, Modifiers), // beat, layer, modifiers
    TrackDoubleClicked(f32, usize), // beat, layer (create clip)
    ViewportHoverChanged(Option<ClipId>), // clip_id or None
    ClipRightClicked(String),       // clip_id (context menu)
    TrackRightClicked(f32, usize),  // beat, layer (context menu)
    /// Right-click anywhere on an automation lane strip/segment/dot
    /// (BUG-184) — opens the lane's context menu.
    AutomationLaneRightClicked(UiGraphTarget, ParamId),
    LayerHeaderRightClicked(LayerId),
    ContextSplitAtPlayhead(String),  // clip_id
    ContextDeleteClip(String),       // clip_id
    ContextDuplicateClip(String),    // clip_id
    ContextPasteAtTrack(f32, usize), // beat, layer (track-content click; positional by design — no stable layer identity at that hit-test site)
    ContextAddVideoLayer(LayerId),     // after_layer
    ContextAddGeneratorLayer(LayerId), // after_layer
    ContextAddAudioLayer(LayerId),     // after_layer
    ContextDeleteLayer(LayerId),       // layer
    ContextDuplicateLayer(LayerId),    // layer
    ContextPasteAtLayer(LayerId),      // layer
    ContextImportMidi(LayerId),        // layer
    ContextGroupSelectedLayers,
    ContextUngroup(LayerId),                             // layer
    ContextSetLayerColor(LayerId, crate::node::Color32), // layer, color
    DropdownSelected(usize),
}

#[derive(Debug, Clone)]
pub enum LayerAction {
    ToggleMute(LayerId),
    ToggleSolo(LayerId),
    /// Toggle an audio layer's analysis-only output state (silent to master, still
    /// feeding its send). See LAYER_CONTROLS_DESIGN §5.3.
    ToggleAnalysisOnly(LayerId),
    ToggleLed(LayerId),
    SetBlendMode(LayerId, String),
    ExpandLayer(LayerId),
    CollapseLayer(LayerId),
    LayerClicked(LayerId, crate::input::Modifiers),
    LayerDoubleClicked(LayerId),
    ChevronClicked(LayerId),
    BlendModeClicked(LayerId),
    FolderClicked(LayerId),
    NewClipClicked(LayerId),
    AddGenClipClicked(LayerId),
    MidiInputClicked(LayerId),
    MidiChannelClicked(LayerId),
    MidiDeviceClicked(LayerId),
    /// Route an audio layer to a send (layer, send id). `None` clears the
    /// layer's send routing (reverts the previously-fed send to a capture source).
    SetLayerAudioSend(LayerId, Option<AudioSendId>),
    LayerDragStarted(usize),
    LayerDragMoved(usize, usize),
    LayerDragEnded(usize, usize),
    AddLayerClicked,
    DeleteLayerClicked(LayerId),
}

#[derive(Debug, Clone)]
pub enum MarkerAction {
    MarkerClicked(String, Modifiers), // marker_id, modifiers (Shift for multi-select)
    MarkerDoubleClicked(String),      // marker_id (rename)
    MarkerDragStarted(String),        // marker_id
    MarkerDragMoved(String, f32),     // marker_id, new_beat
    MarkerDragEnded(String, f32),     // marker_id, final_beat
    MarkerRightClicked(String),       // marker_id (context menu)
    DeleteSelectedMarkers,
}

#[derive(Debug, Clone)]
pub enum ProjectAction {
    NewProject,
    OpenProject,
    OpenRecent,
    SaveProject,
    SaveProjectAs,
    ExportVideo,
    ExportFrame,
    ToggleHdr,
    ExportXml,
    ToggleLiveRecording,
    SelectAudioInputDevice,
    SetAudioInputDevice(String),
    ToggleMonitor,
    EnterPerformMode,
    /// A Scene Setup panel row write: `(layer_id, scope_path, node_doc_id,
    /// param_name, new_value)`. Dispatched through the identical
    /// `SetGraphNodeParamCommand` the graph editor's node face already uses —
    /// the panel's "fourth surface" (D3: every editable row carries its
    /// `(scope_path, node_doc_id, param_id)` write address). `scope_path` is
    /// empty for root-level rows (Environment/Fog, Lights, Camera, object
    /// transforms) and `[group_node_id]` for a P2 Objects material/modifier
    /// row living inside the object's own group.
    SceneSetupParamChanged(LayerId, Vec<u32>, u32, String, f32),
    /// "Add environment" (D3): spawn `node.bake_environment` wired to the
    /// scene's `envmap` port. `(layer_id, render_scene_node_doc_id)`.
    SceneSetupAddEnvironment(LayerId, u32),
    /// "Add fog" (D3): spawn `node.atmosphere` wired to the scene's
    /// `atmosphere` port. `(layer_id, render_scene_node_doc_id)`.
    SceneSetupAddFog(LayerId, u32),
    /// D7 "New 3D Scene" empty-state action: assign the bundled Scene
    /// Starter generator preset to the selected layer — the SAME
    /// generator-assignment path the picker's `SetGenType` already uses
    /// (§1 VERIFY marker, resolved: `PanelAction::SetGenType`).
    SceneSetupNewScene(LayerId),
    /// P2 "+ Object" button: `(layer_id, render_scene_node_doc_id,
    /// next_index)`. Dispatches the EXISTING `AddSceneObjectCommand`
    /// (SCENE_BUILD P5) — no new mutation path.
    SceneSetupAddObject(LayerId, u32, u32),
    /// P2 "+ Light" button: `(layer_id, render_scene_node_doc_id,
    /// next_index)`. Dispatches the EXISTING `AddSceneLightCommand`.
    SceneSetupAddLight(LayerId, u32, u32),
    /// P5 properties-header "Duplicate" button (Object selection):
    /// `(layer_id, render_scene_node_doc_id, source_index)`. Dispatches the
    /// existing `DuplicateSceneObjectCommand` (D11).
    SceneSetupDuplicateObject(LayerId, u32, u32),
    /// P4 "Import Model…" button: `(layer_id, render_scene_node_doc_id)`.
    /// Opens a native file dialog (the app's existing open-file plumbing,
    /// same `rfd::FileDialog` pattern as `ClipReplaceAudioClicked`) and, on
    /// a picked `.glb`/`.gltf`, merges its objects into THIS scene via
    /// `merge_import_into_graph` + `ImportModelIntoSceneCommand` (D5) — a
    /// second (third, nth) model added to a scene the panel already shows,
    /// no graph editor trip required.
    SceneSetupImportModelClicked(LayerId, u32),
    /// P5 "Add modifier" chip: `(layer_id, group_node_id, type_id)`.
    /// Dispatches `InsertMeshModifierCommand`, appending the chosen D6 atom
    /// at the end of the object's stack (no position picker in v1 — D6's
    /// default: "end of stack, just before the group output").
    SceneSetupAddModifier(LayerId, u32, String),
    /// P5 modifier-row remove button: `(layer_id, group_node_id,
    /// modifier_node_id)`. Dispatches `RemoveMeshModifierCommand`.
    SceneSetupRemoveModifier(LayerId, u32, u32),
    /// P5 modifier-row up/down reorder: `(layer_id, group_node_id,
    /// modifier_node_id, new_position)`. Dispatches
    /// `MoveMeshModifierCommand` — `new_position` is resolved by the panel
    /// from the row's own live index in the Vm's stack order (one hop
    /// forward or back), same "read the live count off the Vm" convention
    /// `SceneSetupAddObject`'s `next_index` already uses.
    SceneSetupMoveModifier(LayerId, u32, u32, u32),
    /// per-row "✕" in the Objects section: `(layer_id,
    /// render_scene_node_doc_id, object_index)`. Dispatches the new
    /// `RemoveSceneObjectCommand` — the inverse of `SceneSetupAddObject`.
    SceneSetupRemoveObject(LayerId, u32, u32),
    /// per-row "✕" in the Lights section: `(layer_id,
    /// render_scene_node_doc_id, light_index)`. Dispatches the new
    /// `RemoveSceneLightCommand` — the inverse of `SceneSetupAddLight`.
    SceneSetupRemoveLight(LayerId, u32, u32),
    /// UX-P3a (SCENE_PANEL_UX_DESIGN.md D8, sizing amendment): click on a
    /// scene row's mod button — expose this inner param on the layer's
    /// generator card via the SAME `ToggleNodeParamExposeCommand` the graph
    /// editor's expose glyph uses, one undo unit, named `<object_label> ·
    /// <param_label>`. One-way in P3a: the panel emits this on every click
    /// of a live (non-driven) mod button regardless of its current lit
    /// state — the app dispatch handler is the one that no-ops when the
    /// param is ALREADY exposed, so a second click never un-exposes and
    /// never mints a duplicate binding. Un-exposing a param that may
    /// already carry drivers/envelopes is a footgun from this panel (D8's
    /// own text) — that stays a graph-editor-only affordance.
    SceneSetupExposeParam {
        layer_id: LayerId,
        scope_path: Vec<u32>,
        node_doc_id: u32,
        param_id: String,
        object_label: String,
        param_label: String,
        min: f32,
        max: f32,
        default_value: f32,
        is_angle: bool,
    },
    MidiTriggerModeClicked(LayerId),
    /// "Clear Automation" context-menu item: empties the lane's points,
    /// keeping the (now-empty) lane — `ClearLaneCommand`.
    ContextClearAutomationLane(UiGraphTarget, ParamId),
    /// "Remove Lane" context-menu item: deletes the whole lane —
    /// `RemoveLaneCommand`.
    ContextRemoveAutomationLane(UiGraphTarget, ParamId),
    SetMidiNote(LayerId, i32),              // layer, note (0-127)
    SetMidiChannel(LayerId, i32),           // layer, channel (0-15 internal, displayed 1-16)
    SetMidiDevice(LayerId, Option<String>), // layer, device name (None = any)
    SetMidiTriggerMode(LayerId, MidiTriggerMode),
    SetResolution(usize),           // preset index
    SetDisplayResolution(i32, i32), // direct width, height (no undo, matches Unity)
    SetRenderScale(f32),            // render scale: 1.0 (native), 0.75 (quality), 0.5 (performance)
    SetTonemapCurve(TonemapCurve),
    SetGenType(Option<LayerId>, PresetTypeId), // layer_id, preset type id
}

#[derive(Debug, Clone)]
pub enum BrowserAction {
    /// Right-click on a cell → open its management menu (Rename always;
    /// Duplicate/Reveal only for `MyLibrary`; never shown for `Factory` —
    /// `browser_popup::handle_right_click` already screens those out).
    BrowserCellRightClicked(browser_popup::BrowserPopupMode, String, picker_core::Source),
    /// Rename clicked → opens the shared name-prompt text-input session
    /// (mirrors `SaveToLibrary`/`SaveToProject`); the actual write happens on
    /// commit (`UserLibrary::rename` for `MyLibrary`, an undoable
    /// `RenameEmbeddedPresetCommand` for `Project`).
    BrowserRenamePresetClicked(browser_popup::BrowserPopupMode, String, picker_core::Source),
    /// Duplicate clicked — `MyLibrary` entries only (`UserLibrary::duplicate`).
    BrowserDuplicatePresetClicked(browser_popup::BrowserPopupMode, String),
    /// Delete clicked — a native Yes/No confirm (`crate::alerts::confirm`,
    /// same precedent as `RestoreSnapshot`) gates the actual removal
    /// (`UserLibrary::delete` for `MyLibrary`, an undoable
    /// `DeleteEmbeddedPresetCommand` for `Project`).
    BrowserDeletePresetClicked(browser_popup::BrowserPopupMode, String, picker_core::Source),
    /// Reveal in Finder clicked — `MyLibrary` entries only (`UserLibrary::reveal`).
    BrowserRevealPresetClicked(browser_popup::BrowserPopupMode, String),
}

#[derive(Debug, Clone)]
pub enum ClipAction {
    ClipChromeCollapseToggle,
    ClipBpmClicked,
    /// Audio clip: toggle warp on/off (sets recorded BPM to project tempo / 0).
    ClipWarpToggled,
    /// Audio clip: run per-clip percussion detection on its file.
    ClipDetectClicked,
    /// Audio clip: remove the triggers this clip produced.
    ClipClearTriggersClicked,
    /// Audio clip: replace the source file (file dialog → ReplaceAudioFileCommand).
    /// Keeps detection config/routing, clears cached analysis + generated clips.
    ClipReplaceAudioClicked,
    /// Audio clip: toggle whether instrument N is detected (re-plans from cache).
    ClipDetectInstrumentToggled(usize),
    /// Audio clip: instrument N's sensitivity changed to this 0..1 value (drag
    /// commit — re-plans from cache). Emitted on slider release, not per-tick.
    ClipDetectSensitivityChanged(usize, f32),
    /// Audio clip: onset compensation changed, in milliseconds (drag commit).
    ClipDetectOnsetChanged(f32),
    /// Audio clip: open the quantize-grid dropdown (anchored to the trigger).
    ClipDetectQuantizeClicked,
    /// Audio clip: open instrument N's target-layer dropdown (anchored to trigger).
    ClipDetectLayerClicked(usize),
    /// Audio clip: set the quantize grid (None = off; Some(beats) = on at step).
    ClipDetectSetQuantize(Option<Beats>),
    /// Audio clip: route instrument N to a layer (None = Auto / by-name).
    ClipDetectSetLayer(usize, Option<LayerId>),
    ClipLoopToggle,
}

#[derive(Debug, Clone)]
pub enum ParamsAction {
    /// Audio-layer Gain slider drag begins — snapshot for undo.
    AudioGainSnapshot(LayerId),
    /// Audio-layer Gain slider dragged to a new dB value (layer, dB).
    AudioGainChanged(LayerId, f32),
    /// Audio-layer Gain slider released — commit one undo step.
    AudioGainCommit(LayerId),
    MasterCollapseToggle,
    MasterExitPathClicked,
    /// Set LED exit path index: -1 = after all FX, 0 = before FX, N = after effect N-1.
    SetLedExitIndex(i32),
    MasterOpacitySnapshot,
    MasterOpacityChanged(f32),
    MasterOpacityCommit,
    LedEnabledToggle,
    LedBrightnessSnapshot,
    LedBrightnessChanged(f32),
    LedBrightnessCommit,
    LayerChromeCollapseToggle,
    LayerOpacitySnapshot,
    LayerOpacityChanged(f32),
    LayerOpacityCommit,
    EffectToggle(usize),
    EffectCollapseToggle(usize),
    /// Collapse or expand every effect card in the active inspector column at
    /// once (the collapse-all / expand-all control in the tab strip). The UI
    /// resolves the target state — collapse if any card is currently open, else
    /// expand them all.
    SetAllCardsCollapsed {
        collapsed: bool,
    },
    /// A modulation-config tab was clicked on a param row (the card already
    /// switched its own UI-only active-tab state). Routes to a structural
    /// rebuild so the drawer repaints with the newly-selected config; carries no
    /// payload and mutates no model.
    ModConfigTabChanged,
    /// A card section header was clicked (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md
    /// §2 D5) — the card already flipped its own UI-only `section_folded`
    /// entry in `handle_click`. Routes to a structural rebuild so the folded/
    /// unfolded rows repaint; carries no payload and mutates no model (fold
    /// state is workspace-local, never serialized).
    SectionFoldToggled,
    /// §6b — the global "hide mod settings" (compact) toggle was clicked. The
    /// inspector already flipped its own UI-only flag; this routes to a
    /// structural rebuild so every card's drawers hide/show. No model mutation.
    ModsCompactToggled,
    EffectCardClicked(usize),
    ParamSnapshot(GraphParamTarget, ParamId),
    ParamChanged(GraphParamTarget, ParamId, f32),
    ParamCommit(GraphParamTarget, ParamId),
    /// one atomic enum write (a dropdown pick). Dispatch runs the
    /// existing `ParamSnapshot`/`ParamChanged`/`ParamCommit` trio in
    /// sequence, so the scene id_map interception and the one-undo-unit
    /// granularity come free — no new mutation path.
    ParamEnumSet(GraphParamTarget, ParamId, f32),
    /// Toggle a boolean (`isToggle`) param's ON/OFF button — a 0↔1 flip.
    /// Same `ChangeGraphParamCommand` write path as `ParamChanged`, but
    /// atomic (no snapshot/commit pair — a click isn't a drag).
    ParamToggle(GraphParamTarget, ParamId),
    /// Fire a monotonic `isTrigger` param's "▶" button — increments the
    /// underlying counter by one instead of flipping it.
    ParamFire(GraphParamTarget, ParamId),
    /// Toggle the "3D Shading" header icon (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// D2/P5). Atomic like `ParamToggle` — a click, not a drag.
    RelightToggle(GraphParamTarget),
    /// Press on a D3 relight knob's track — snapshot the pre-drag value for
    /// undo (mirrors `ParamSnapshot`).
    RelightParamSnapshot(GraphParamTarget, UiRelightField),
    /// Live drag of a D3 relight knob (mirrors `ParamChanged`). Always
    /// live even while the toggle is off — the row renders greyed, not
    /// hidden, and must still take effect for when it's switched on.
    RelightParamChanged(GraphParamTarget, UiRelightField, f32),
    /// Release on a D3 relight knob's track — commits one undo entry
    /// (mirrors `ParamCommit`).
    RelightParamCommit(GraphParamTarget, UiRelightField),
    /// D4 "Height From" enum row click (Auto / Luminance / Inverted
    /// Luminance) — atomic like `ParamToggle`.
    RelightHeightFromChanged(GraphParamTarget, UiRelightHeightFrom),
    /// Reorder effect card: move from_index to to_index.
    /// Unity: EffectsListBitmapPanel.onCardReorder.
    EffectReorder(usize, usize),
    /// Reorder multiple effect cards as a group: (sorted source indices, target index).
    EffectReorderGroup(Vec<usize>, usize),
    GenTypeClicked(Option<LayerId>), // layer_id
    GenStringParamClicked(usize), // string_param_index — open text input
    GenStringParamDropdownClicked(usize), // string_param_index — open dropdown selector
    GenStringParamSelected(usize, String), // string_param_index, selected value
    GenCollapseToggle,
    GenCardClicked,
    CopyGenerator,
    PasteGenerator,
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
    /// Save to Library (PRESET_LIBRARY_DESIGN D4): publish the targeted
    /// preset's current effective definition as a new named entry under the
    /// user's library folder. Opens a name-prompt text-input session (NOT a
    /// native file dialog — Export/Import are the only `rfd` users); the
    /// actual write happens on commit.
    SaveToLibrary(GraphParamTarget),
    /// Save to Project (PRESET_LIBRARY_DESIGN D4): publish the targeted
    /// preset's current effective definition as a new `origin: Saved`
    /// project-embedded preset, WITHOUT retargeting the instance that
    /// triggered it (unlike Make Unique / Import). Opens the same
    /// name-prompt text-input session as Save to Library.
    SaveToProject(GraphParamTarget),
    /// Revert to Library (PRESET_LIBRARY_DESIGN D3, P4): clear the targeted
    /// preset's per-instance graph override, going back to tracking its
    /// library entry (undoable). Shown in the card menu only when the card
    /// is diverged (`graph.is_some()`) — reverting an already-tracking
    /// instance would be a no-op.
    RevertToLibrary(GraphParamTarget),
    /// Push to Library (D3, P4): overwrite the targeted preset's tracked
    /// user-library file with its current (diverged) definition, so every
    /// OTHER instance still tracking that id picks it up via the existing
    /// hot-reload watcher. A factory/stock id has no file to overwrite —
    /// the dispatch falls back to Save to Library (as new) instead. Shown
    /// only when diverged, same gate as `RevertToLibrary`.
    PushToLibrary(GraphParamTarget),
    MacrosCollapseToggle,
    MacroSnapshot(usize),
    MacroChanged(usize, f32),
    MacroCommit(usize),
    MacroLabelRename(usize),     // macro_index — opens inline rename input
    ParamLabelRightClick(GraphParamTarget, ParamId),
    MacroReset(usize), // macro_idx — reset to 0 from context menu
    AddEffectClicked(InspectorTab),
    RemoveEffect(usize),
    BrowserSearchClicked,
    PasteEffects,
    AddEffect(InspectorTab, PresetTypeId), // tab, preset type id
}

#[derive(Debug, Clone)]
pub enum ModulationAction {
    DriverToggle(GraphParamTarget, ParamId),
    EnvelopeToggle(GraphParamTarget, ParamId),
    DriverConfig(GraphParamTarget, ParamId, DriverConfigAction),
    /// Arm/disarm audio modulation on a param. Arming assigns the project's
    /// first audio send with a default feature; re-clicking toggles enabled.
    /// No-op when no sends exist (the audio button is inert until the Audio
    /// Setup defines one). See `docs/AUDIO_MODULATION_DESIGN.md`.
    AudioModToggle(GraphParamTarget, ParamId),
    /// Set an audio modulation's source: which send + which feature.
    AudioModSetSource(
        GraphParamTarget,
        ParamId,
        AudioSendId,
        AudioFeature,
    ),
    /// Remove the audio modulation from a param.
    AudioModRemove(GraphParamTarget, ParamId),
    /// Toggle an audio modulation's invert flag (`AudioModShape::invert`) — the
    /// drawer's "Invert" button (loud → low).
    AudioModSetInvert(GraphParamTarget, ParamId),
    /// Toggle an audio modulation's rate-of-change flag
    /// (`AudioModShape::rate_of_change`) — the feature would drive on its
    /// motion rather than its level. No drawer button reaches this anymore
    /// (§7.2 item 2, 2026-07-11 — "Delta" removed from the UI, "not very
    /// useful and adds a lot of clutter"); the variant and the runtime field
    /// and `condition()` arm it drives stay compiled for a possible future
    /// re-wire. Un-suppression trigger for any dead-code warning this
    /// strands: re-wire per AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md
    /// §7.2 item 2.
    AudioModSetRateOfChange(GraphParamTarget, ParamId),
    /// Snapshot an audio mod's shape before a drawer-slider drag (undo start).
    AudioModShapeSnapshot(GraphParamTarget, ParamId),
    /// Live-edit one shape scalar during a drawer-slider drag (no undo entry).
    AudioModShapeParamChanged(
        GraphParamTarget,
        ParamId,
        AudioShapeParam,
        f32,
    ),
    /// Commit a shape-slider drag as one undo step (drag end).
    AudioModShapeCommit(GraphParamTarget, ParamId),
    /// Set a trigger-gate param's fire mode — index into `[ClipEdge,
    /// Transient, Both]` (§9 U3), converted to `TriggerFireMode` at the
    /// dispatch boundary (this crate mirrors core enums rather than
    /// depending on `manifold-core` directly; see `ui_translate.rs`). The
    /// drawer's one trigger-only row, on top of the standard Source/Feature/
    /// Band/Invert/Sensitivity/Attack/Release rows every audio mod has.
    AudioModSetTriggerMode(GraphParamTarget, ParamId, usize),
    /// Set an audio mod's fire ACTION kind (PARAM_STEP_ACTIONS D8) — the
    /// drawer's Action segmented row. Index: 0=Continuous, 1=Step, 2=Random.
    /// Entering Step from a non-Step action seeds `amount`/`wrap` to their
    /// D2 defaults at the dispatch boundary; leaving Step drops them (the
    /// enum has nowhere to keep them outside the `Step` variant — re-entering
    /// reseeds, matching D2's "seeding only" contract).
    AudioModSetActionKind(GraphParamTarget, ParamId, usize),
    /// Snapshot an audio mod's action before a Step-Amount slider drag (undo
    /// start). `amount` lives on `TriggerAction::Step`, not `AudioModShape`,
    /// so it rides its own snapshot/changed/commit trio rather than
    /// `AudioShapeParam`'s.
    AudioModStepAmountSnapshot(GraphParamTarget, ParamId),
    /// Live-edit the Step amount during a drawer-slider drag (no undo entry).
    AudioModStepAmountChanged(GraphParamTarget, ParamId, f32),
    /// Commit a Step-Amount drag as one undo step (drag end).
    AudioModStepAmountCommit(GraphParamTarget, ParamId),
    /// Set a Step action's wrap mode — index into `[Wrap, Bounce, Clamp]`
    /// (D2) — the drawer's Wrap segmented row, shown only while Action=Step.
    AudioModSetWrap(GraphParamTarget, ParamId, usize),
    /// A modulator output sub-range handle moved during a drag. `TrimKind`
    /// selects which modulator (driver / Ableton / audio) — the three formerly
    /// parallel `*TrimChanged` variants are one path now.
    TrimChanged(TrimKind, GraphParamTarget, ParamId, f32, f32),
    /// Snapshot trim state before drag (for undo).
    TrimSnapshot(TrimKind, GraphParamTarget, ParamId),
    /// Commit trim drag (record undo command).
    TrimCommit(TrimKind, GraphParamTarget, ParamId),
    /// Envelope target (orange handle / `target_normalized`) changed.
    TargetChanged(GraphParamTarget, ParamId, f32),
    /// Snapshot target before drag (for undo).
    TargetSnapshot(GraphParamTarget, ParamId),
    /// Commit target drag (record undo command).
    TargetCommit(GraphParamTarget, ParamId),
    /// Envelope decay slider (`decay_beats`) changed.
    EnvDecayChanged(GraphParamTarget, ParamId, f32),
    /// Snapshot decay before drag (for undo).
    EnvDecaySnapshot(GraphParamTarget, ParamId),
    /// Commit decay drag (record undo command).
    EnvDecayCommit(GraphParamTarget, ParamId),
}

#[derive(Debug, Clone)]
pub enum MappingAction {
    MacroLabelRightClick(usize), // macro_index — opens mappings dropdown
    MapParamToMacro(GraphParamTarget, ParamId, usize), // gpt, param_id, macro_idx
    UnmapMacro(usize, usize),                                                  // macro_idx, mapping_idx
    ClearMacroMappings(usize),                                                 // macro_idx
    MapParamToAbleton(
        GraphParamTarget,
        ParamId,
        AbletonMacroAddress,
    ), // gpt, param_id, address
    UnmapParamAbleton(GraphParamTarget, ParamId), // gpt, param_id
    /// Ableton mapping for macro slots.
    MapMacroToAbleton(usize, AbletonMacroAddress),
    UnmapMacroAbleton(usize),
    OpenAbletonPickerForMacro(usize),
    AbletonMacroTrimSnapshot(usize),                                      // slot_idx
    AbletonMacroTrimChanged(usize, f32, f32),                             // slot_idx, min, max
    AbletonMacroTrimCommit(usize),                                        // slot_idx
    AbletonInvertToggle(GraphParamTarget, ParamId),
    AbletonMacroInvertToggle(usize),                             // slot_idx
}

#[derive(Debug, Clone)]
pub enum AudioSetupAction {
    /// Toggle the AUDIO TRIGGERS section's collapse state — UI-local (mirrors
    /// `MacrosCollapseToggle`; no `Project` write, no persistence).
    AudioTriggerSectionToggle,
    /// Expand/collapse one row's drawer — UI-local, same as the section toggle.
    AudioTriggerRowExpandToggle(LayerId, usize),
    /// Append a new clip trigger to the layer: ENABLED, listening to the
    /// first send's kick cell, so one click gives a firing trigger the user
    /// adjusts from there. No-op when no sends exist (mirrors
    /// `AudioModToggle`'s "arm" no-send case).
    AudioTriggerAdd(LayerId),
    /// Remove the clip trigger at `index`.
    AudioTriggerRemove(LayerId, usize),
    /// Flip `enabled` on the clip trigger at `index` — the row's own ON/OFF
    /// button (D4: a clip trigger has no Mode row to arbitrate with, so its
    /// existence isn't its enabled state the way a param audio-mod's is).
    AudioTriggerEnabledToggle(LayerId, usize),
    /// Set a clip trigger's source: which send + which feature (mirrors
    /// `AudioModSetSource`). A chip click and a Source-row click both arrive
    /// as this one action, carrying the full cell.
    AudioTriggerSetSource(LayerId, usize, AudioSendId, AudioFeature),
    /// Snapshot a clip trigger's shape before a drawer-slider drag (undo
    /// start) — mirrors `AudioModShapeSnapshot`.
    AudioTriggerShapeSnapshot(LayerId, usize),
    /// Live-edit one shape scalar during a drawer-slider drag (no undo
    /// entry) — mirrors `AudioModShapeParamChanged`. The clip-trigger drawer
    /// only ever sends `AudioShapeParam::Sensitivity` (its only slider).
    AudioTriggerShapeParamChanged(LayerId, usize, AudioShapeParam, f32),
    /// Commit a shape-slider drag as one undo step — mirrors
    /// `AudioModShapeCommit`.
    AudioTriggerShapeCommit(LayerId, usize),
    /// Set the one-shot fire length (`one_shot_beats`) — the drawer's Length
    /// row (D4/D5), clip triggers only.
    AudioTriggerSetLength(LayerId, usize, f32),
    /// Set (or clear) the capture input device. `None` = system default input.
    AudioSetDevice(Option<AudioDeviceRef>),
    /// Add a new empty send.
    AudioAddSend,
    /// Remove a send by id.
    AudioRemoveSend(AudioSendId),
    /// Rename a send (commit with the new label).
    AudioRenameSend(AudioSendId, String),
    /// Set a send's input channels (downmixed to mono for analysis). The
    /// channel dropdown enumerates stereo pairs AND single channels directly
    /// (§7.2 item 7, P8, 2026-07-11), so this carries any length channel vec
    /// — mono falls out of a one-channel pick, no separate toggle needed.
    /// `AudioSendStereoToggle`, `AudioSendAddLayerClicked`, and
    /// `AudioSendRoutingsClicked` are deleted the same phase (items 6/7):
    /// the St/Mo toggle, the Inputs section's "+ Layer" authoring, and the
    /// Cap chip's click-to-reveal routings popup are all gone outright.
    AudioSetSendChannels(AudioSendId, Vec<u16>),
    /// Step a send's input gain trim by a dB delta (the panel's −/＋ buttons).
    /// The host reads the send's current gain, applies the delta, clamps, and
    /// commits — so the project stays the single source of truth.
    AudioSendGainStep(AudioSendId, f32),
    /// Begin dragging a send's gain value label (D7) — snapshot the pre-drag
    /// gain so the commit records one undo step. Same pattern as
    /// `AudioCrossoverDragBegin`, per-send.
    AudioSendGainDragBegin(AudioSendId),
    /// Live gain change while dragging the value label: the absolute candidate
    /// dB (1 px = 0.1 dB, computed by the panel from pointer movement; the
    /// host clamps to the trim range). Applied immediately via
    /// `MutateProjectLive` — no per-frame undo.
    AudioSendGainDragChanged(AudioSendId, f32),
    /// Commit the gain drag as one undo step (`SetAudioSendGainCommand`).
    AudioSendGainDragCommit(AudioSendId),
    /// P4 type-in commit: set a send's gain to an EXACT typed value, ONE
    /// undo step, NO clamp (`PARAM_RANGE_CONTRACT` P1 — unlike
    /// `AudioSendGainDragChanged`'s live-drag clamp to the trim range).
    AudioSendGainSetTyped(AudioSendId, f32),
    /// Step the selected send's pre-analysis noise floor by a dB delta (the
    /// spectrogram's Floor −/＋). Off ⇄ engaged is handled host-side.
    AudioSendFloorStep(AudioSendId, f32),
    /// Begin dragging a band-divider line on the spectrogram — snapshot the
    /// current crossovers so the commit records one undo step.
    AudioCrossoverDragBegin,
    /// Live crossover change while dragging a divider: which line + its new Hz.
    /// Applied immediately (no per-frame undo) so the line tracks the cursor and
    /// the analysis bands retune live.
    AudioCrossoverChanged(BandDivider, f32),
    /// Commit the band-divider drag as one undo step.
    AudioCrossoverCommit,
}

#[derive(Debug, Clone)]
pub enum RootAction {
    /// Right-click reset of a slider to its default, expressed as the slider's own
    /// value-change trio (same path a drag uses). The app dispatches the three in
    /// order, so undo == a drag to `default`. Replaces the per-panel `*RightClick`
    /// reset actions (BUG-061).
    SliderReset {
        snapshot: Box<PanelAction>,
        changed: Box<PanelAction>, // carries the default value
        commit: Box<PanelAction>,
    },
    /// Open (toggle) the Audio Setup panel — the central place to route audio
    /// in and define named sends. Header button; also bound to ⌘⇧A.
    OpenAudioSetup,
    /// Open (toggle) the Scene Setup panel (`SCENE_SETUP_PANEL_DESIGN.md` D2)
    /// — mutually exclusive with [`Self::OpenAudioSetup`] (the app dispatch
    /// closes the other dock, same either/or toggle policy as that pair).
    /// Header button.
    OpenSceneSetup,
    /// Scene Setup outliner selection moved (D1 of SCENE_PANEL_UX_DESIGN.md).
    /// The panel has already updated its UI-local selection; this action's
    /// only job is to ride the dispatch loop back as `structural_change:
    /// true` so `sync_inspector_data` rebuilds the panel this same frame —
    /// same-frame Properties update, no polling, no per-frame rebuild.
    /// Payload: the layer whose selection moved (the panel key) — the
    /// selection itself stays panel-internal (D7 of SCENE_SETUP_PANEL).
    SceneSetupSelectionChanged(LayerId),
    /// D7 "Open Graph Editor" empty-state action for a generator layer with
    /// no `render_scene` — reuses the existing open-editor action.
    SceneSetupOpenGraphEditor(LayerId),
    /// P2 object-name click: `(layer_id, group_node_id, current_name)` — opens
    /// the shared inline text-input session (same mechanics as
    /// `AudioSendLabelClicked`); commit dispatches `RenameGroupCommand` (the
    /// SCENE_BUILD P3 rename-sweep command, unchanged).
    SceneSetupRenameObjectClicked(LayerId, u32, String),
    /// P5 light-row/properties-header name click: `(layer_id, light_node_id,
    /// current_name)` — same shape as [`Self::SceneSetupRenameObjectClicked`],
    /// opens the shared inline text-input session over the row's name label;
    /// commit dispatches the plain `SetNodeHandleCommand` (no group sweep —
    /// nothing downstream displays light names besides this row).
    SceneSetupRenameLightClicked(LayerId, u32, String),
    /// UX-P2 (D6 of SCENE_PANEL_UX_DESIGN.md): the single "+ Add Modifier"
    /// button click — `(layer_id, group_node_id, button_node_id)`. Replaces
    /// the old 7-chip grid, each of which dispatched `SceneSetupAddModifier`
    /// directly; this button doesn't resolve a choice itself, it opens the
    /// shared `panels::dropdown` overlay (`UIRoot::try_open_dropdown_inner`,
    /// same resolve-at-open convention as `SceneSetupEnumClicked` —
    /// `button_node_id` anchors the overlay since the panel has no
    /// `&UITree` in `handle_event`), listing the SAME
    /// `scene_setup_panel::MESH_MODIFIER_CHOICES` the chips used, each item
    /// dispatching the SAME `SceneSetupAddModifier` — no new mutation path.
    SceneSetupAddModifierClicked(LayerId, u32, crate::node::NodeId),
    /// P4 (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D8): double-click on a dock
    /// numeric value cell opens its type-in box. Carries the row's write
    /// address (mirroring `SceneSetupParamChanged`'s tuple shape), the
    /// cell's own node id (the app resolves its screen rect at open time,
    /// same convention as `SceneSetupRenameObjectClicked`'s
    /// `object_name_rect` lookup — the panel has no `&UITree` in
    /// `handle_event` to resolve bounds itself), the base value to prefill,
    /// and D10's `degrees` flag (the box prefills/parses degrees for the
    /// committed row table, radians everywhere else — conversion lives only
    /// at this boundary). Handled by the app directly (`text_input.begin` +
    /// `SceneNumericParamCtx`), same shape as `BeginParamTextInput`.
    SceneSetupBeginNumericTextInput {
        layer_id: LayerId,
        scope_path: Vec<u32>,
        node_doc_id: u32,
        param_id: String,
        value: f32,
        cell_node_id: crate::node::NodeId,
        degrees: bool,
    },
    /// P4 D9: click on a 3+-label enum value cell opens the shared
    /// `panels::dropdown` overlay, items = the row's label set anchored
    /// under the cell (`cell_node_id`, same resolve-at-open convention as
    /// `SceneSetupBeginNumericTextInput`). Selection routes back through
    /// `SceneSetupParamChanged` (the label's index as the new value) — no
    /// new mutation path.
    SceneSetupEnumClicked {
        layer_id: LayerId,
        scope_path: Vec<u32>,
        node_doc_id: u32,
        param_id: String,
        labels: Vec<&'static str>,
        current_index: u32,
        cell_node_id: crate::node::NodeId,
    },
    SelectInspectorTab(InspectorTab),
    /// Audio-layer Send dropdown clicked — opens the send picker.
    AudioSendClicked(LayerId),
    /// Open the node-graph editor for this effect (cog icon click).
    /// Currently shows a hardcoded test graph regardless of which effect
    /// triggered it; live data sync lands in a future phase.
    OpenGraphEditor(usize),
    /// Open the sideways mapping drawer for an effect user-tail binding
    /// (Author-context card, right-edge chevron). Carries the binding's stable
    /// `param_id`; the host resolves its current range/scale/offset/invert/curve
    /// from the edited effect and anchors the drawer beside the row. Editor-only:
    /// the perform inspector never emits it (the chevron is Author-context).
    OpenCardMapping(ParamId),
    /// click on an enum (`value_labels`) row's value cell with 3+
    /// labels opens the shared `panels::dropdown` overlay — items = the
    /// row's label set anchored under the cell (`cell_node_id`, same
    /// resolve-at-open convention as `SceneSetupEnumClicked`), checked at
    /// `current_index`, each item dispatching [`ParamEnumSet`]. Emitted by
    /// the shared card row core (`enum_value_cell_actions`), so scene rows
    /// (synthesized pids) and inspector card rows (real pids) share the one
    /// path; a 2-label row cycles instead and never emits this.
    ParamEnumDropdown {
        target: GraphParamTarget,
        param_id: ParamId,
        labels: Vec<String>,
        current_index: u32,
        cell_node_id: crate::node::NodeId,
    },
    /// Double-click on a numeric param's value cell → open a type-in box. Carries
    /// the target + id, the value-cell anchor rect, the base value to prefill, the
    /// clamp range, and whether the param rounds to an integer — everything the
    /// app needs to begin the session and commit it.
    BeginParamTextInput {
        target: GraphParamTarget,
        param_id: ParamId,
        anchor: Rect,
        value: f32,
        min: f32,
        max: f32,
        whole_numbers: bool,
    },
    /// Click on the driver drawer's Free field → open a beats type-in for the
    /// LFO's free-running period (free mode). Carries the target + id, the field
    /// anchor rect, and the current period to prefill (the division's beats when
    /// in sync mode, so the box opens at a sensible value).
    BeginDriverPeriodTextInput {
        target: GraphParamTarget,
        param_id: ParamId,
        anchor: Rect,
        value: f32,
    },
    /// Open the input-device dropdown (anchored to the clicked trigger).
    AudioSetupDeviceClicked,
    /// Open a send's input-channel dropdown (anchored to the clicked trigger).
    AudioSendChannelClicked(AudioSendId),
    /// Begin inline editing of a send's label (clicked its name).
    AudioSendLabelClicked(AudioSendId),
    /// P4 (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D8, audio-dock sibling):
    /// double-click on the gain value cell opens its type-in box. Carries
    /// the send id, the current gain (dB) to prefill, and the cell's own
    /// node id (the app resolves its screen rect at open time — same
    /// convention as `SceneSetupBeginNumericTextInput`).
    AudioSendGainBeginTextInput(AudioSendId, f32, crate::node::NodeId),
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
    /// Set the binding's card section (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md
    /// §2 D5). One-shot edit; `None` clears the row back to unsectioned.
    /// Manifest-only per BOUNDARIES D4 — see `BindingMappingEdit::section`.
    EffectMappingSection {
        binding_id: String,
        section: Option<String>,
    },
    /// Set the binding's card-slider invert flag. One-shot edit.
    EffectMappingInvert {
        binding_id: String,
        invert: bool,
    },
    /// Set the binding's response curve. One-shot edit.
    EffectMappingCurve {
        binding_id: String,
        curve: MacroCurve,
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
    /// User clicked the "open graph editor" affordance on the
    /// generator card header. Mirror of
    /// [`PanelAction::OpenGraphEditor`] for effects — the host opens
    /// the graph editor scoped to the currently-selected layer's
    /// generator.
    OpenGeneratorGraphEditor,
    /// Open the Ableton picker popup for a param (effect or generator).
    OpenAbletonPickerForParam(GraphParamTarget, ParamId), // gpt, param_id
    CopyOscAddress(String),
}

// `From<DomainAction> for PanelAction` — wraps a domain value into its sum arm.
// One per domain; lets an emit site read `DomainAction::X(..).into()` where a
// builder returns the domain type naturally, though the sweep prefers explicit
// `PanelAction::Domain(DomainAction::X(..))` wrapping.
impl From<TransportAction> for PanelAction {
    fn from(a: TransportAction) -> Self { PanelAction::Transport(a) }
}
impl From<EditingAction> for PanelAction {
    fn from(a: EditingAction) -> Self { PanelAction::Editing(a) }
}
impl From<LayerAction> for PanelAction {
    fn from(a: LayerAction) -> Self { PanelAction::Layer(a) }
}
impl From<MarkerAction> for PanelAction {
    fn from(a: MarkerAction) -> Self { PanelAction::Marker(a) }
}
impl From<ProjectAction> for PanelAction {
    fn from(a: ProjectAction) -> Self { PanelAction::Project(a) }
}
impl From<BrowserAction> for PanelAction {
    fn from(a: BrowserAction) -> Self { PanelAction::Browser(a) }
}
impl From<ClipAction> for PanelAction {
    fn from(a: ClipAction) -> Self { PanelAction::Clip(a) }
}
impl From<ParamsAction> for PanelAction {
    fn from(a: ParamsAction) -> Self { PanelAction::Params(a) }
}
impl From<ModulationAction> for PanelAction {
    fn from(a: ModulationAction) -> Self { PanelAction::Modulation(a) }
}
impl From<MappingAction> for PanelAction {
    fn from(a: MappingAction) -> Self { PanelAction::Mapping(a) }
}
impl From<AudioSetupAction> for PanelAction {
    fn from(a: AudioSetupAction) -> Self { PanelAction::AudioSetup(a) }
}
impl From<RootAction> for PanelAction {
    fn from(a: RootAction) -> Self { PanelAction::Root(a) }
}
