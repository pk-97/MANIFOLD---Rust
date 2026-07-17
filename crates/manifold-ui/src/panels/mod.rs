pub mod ableton_picker;
pub mod audio_setup_panel;
pub mod audio_trigger_section;
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
pub mod picker_core;
pub mod param_slider_shared;
pub mod perf_hud;
pub mod scene_setup_panel;
pub mod settings_popup;
pub mod popup_shell;
pub mod toast;
pub mod transport;
pub mod viewport;

use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::{Color32, Rect};
use crate::tree::UITree;
use crate::types::{
    AbletonMacroAddress, AudioDeviceRef, AudioFeature, MacroCurve, MidiTriggerMode,
    PresetTypeId, TonemapCurve,
};
use crate::view::UiGraphTarget;
use manifold_foundation::{AudioSendId, Beats, ClipId, LayerId, ParamId};
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

/// Actions for driver (LFO) configuration sub-panels.
#[derive(Debug, Clone)]
pub enum DriverConfigAction {
    /// Pick a sync beat-division (grid index) — also returns to sync mode.
    BeatDiv(usize),
    /// Pick a waveform shape (index).
    Wave(usize),
    /// Feel = straight (strip dotted/triplet from the division).
    Straight,
    /// Feel = dotted (×1.5 period).
    Dotted,
    /// Feel = triplet (×2/3 period).
    Triplet,
    /// Toggle output-polarity invert (`reversed`).
    Invert,
    /// Set the free-running period in beats (free mode). From the type-in commit.
    SetFreePeriod(f32),
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

/// D3's six "3D Shading" relight knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
/// P5b), identified without depending on `manifold-core`/`manifold-editing`
/// (`ui` depends only on `foundation`) — mirrors
/// `manifold_editing::commands::effects::RelightField` one-for-one;
/// translated at the `ui_translate.rs` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRelightField {
    LightX,
    LightY,
    Relief,
    AoIntensity,
    ShadowSoftness,
    Gain,
}

/// D4's height-origin override, mirroring
/// `manifold_core::effects::RelightHeightFrom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRelightHeightFrom {
    Auto,
    Luminance,
    InvertedLuminance,
}

/// Which scalar of an audio modulation's [`AudioModShape`] a drawer slider drag
/// is editing. The three share one drag path, snapshot slot, and commit command;
/// this records which field the live edit and the commit write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioShapeParam {
    /// `sensitivity` — how hard the feature drives (drawer label "Sensitivity").
    Sensitivity,
    /// `attack_ms` — rise smoothing.
    Attack,
    /// `release_ms` — fall smoothing.
    Release,
}

/// Which band-divider line on the Audio Setup spectrogram a drag is moving. The
/// two crossovers share one drag path, snapshot slot, and commit command (the
/// global [`SetAudioCrossoversCommand`]); this records which frequency the live
/// edit writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandDivider {
    /// The low/mid crossover (`AudioSetup::low_hz`).
    Low,
    /// The mid/high crossover (`AudioSetup::mid_hz`).
    Mid,
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
    /// Right-click reset of a slider to its default, expressed as the slider's own
    /// value-change trio (same path a drag uses). The app dispatches the three in
    /// order, so undo == a drag to `default`. Replaces the per-panel `*RightClick`
    /// reset actions (BUG-061).
    SliderReset {
        snapshot: Box<PanelAction>,
        changed: Box<PanelAction>, // carries the default value
        commit: Box<PanelAction>,
    },

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

    // Automation (P4, docs/AUTOMATION_LANES_DESIGN.md §7) — transport-bar globals.
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

    // File
    NewProject,
    OpenProject,
    OpenRecent,
    SaveProject,
    SaveProjectAs,

    // Export
    ExportVideo,
    ExportFrame,
    ToggleHdr,
    ExportXml,

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
    /// Open (toggle) the Scene Setup panel (`SCENE_SETUP_PANEL_DESIGN.md` D2)
    /// — mutually exclusive with [`Self::OpenAudioSetup`] (the app dispatch
    /// closes the other dock, same either/or toggle policy as that pair).
    /// Header button.
    OpenSceneSetup,
    /// A Scene Setup panel row write: `(layer_id, scope_path, node_doc_id,
    /// param_name, new_value)`. Dispatched through the identical
    /// `SetGraphNodeParamCommand` the graph editor's node face already uses —
    /// the panel's "fourth surface" (D3: every editable row carries its
    /// `(scope_path, node_doc_id, param_id)` write address). `scope_path` is
    /// empty for root-level rows (Environment/Fog, Lights, Camera, object
    /// transforms) and `[group_node_id]` for a P2 Objects material/modifier
    /// row living inside the object's own group.
    SceneSetupParamChanged(LayerId, Vec<u32>, u32, String, f32),
    /// Scene Setup outliner selection moved (D1 of SCENE_PANEL_UX_DESIGN.md).
    /// The panel has already updated its UI-local selection; this action's
    /// only job is to ride the dispatch loop back as `structural_change:
    /// true` so `sync_inspector_data` rebuilds the panel this same frame —
    /// same-frame Properties update, no polling, no per-frame rebuild.
    /// Payload: the layer whose selection moved (the panel key) — the
    /// selection itself stays panel-internal (D7 of SCENE_SETUP_PANEL).
    SceneSetupSelectionChanged(LayerId),
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
    /// D7 "Open Graph Editor" empty-state action for a generator layer with
    /// no `render_scene` — reuses the existing open-editor action.
    SceneSetupOpenGraphEditor(LayerId),
    /// P2 "+ Object" button: `(layer_id, render_scene_node_doc_id,
    /// next_index)`. Dispatches the EXISTING `AddSceneObjectCommand`
    /// (SCENE_BUILD P5) — no new mutation path.
    SceneSetupAddObject(LayerId, u32, u32),
    /// P2 "+ Light" button: `(layer_id, render_scene_node_doc_id,
    /// next_index)`. Dispatches the EXISTING `AddSceneLightCommand`.
    SceneSetupAddLight(LayerId, u32, u32),
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
    /// BUG-193 per-row "✕" in the Objects section: `(layer_id,
    /// render_scene_node_doc_id, object_index)`. Dispatches the new
    /// `RemoveSceneObjectCommand` — the inverse of `SceneSetupAddObject`.
    SceneSetupRemoveObject(LayerId, u32, u32),
    /// BUG-193 per-row "✕" in the Lights section: `(layer_id,
    /// render_scene_node_doc_id, light_index)`. Dispatches the new
    /// `RemoveSceneLightCommand` — the inverse of `SceneSetupAddLight`.
    SceneSetupRemoveLight(LayerId, u32, u32),
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

    // Footer
    CycleQuantize,
    ResolutionClicked,
    FpsFieldClicked,

    // Inspector tab
    SelectInspectorTab(InspectorTab),

    // Layer
    //
    // These carry a stable `LayerId`, not a positional index: the panel
    // resolves the row's id at emit time (against its own layer snapshot,
    // the exact list the row was built from) and the bridge looks it up by
    // id. A stale UI snapshot then resolves to the right layer or to a
    // no-op — never the *wrong* layer. (Same reasoning that moved the
    // per-effect-param actions off positional `pi` to `ParamId`.) The
    // `LayerDrag*` variants below stay positional: reordering is inherently
    // about positions and resolves within one gesture against one snapshot.
    ToggleMute(LayerId),
    ToggleSolo(LayerId),
    /// Toggle an audio layer's analysis-only output state (silent to master, still
    /// feeding its send). See LAYER_CONTROLS_DESIGN §5.3.
    ToggleAnalysisOnly(LayerId),
    ToggleLed(LayerId),
    SetBlendMode(LayerId, String),
    ExpandLayer(LayerId),
    CollapseLayer(LayerId),

    // Layer header
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
    MidiTriggerModeClicked(LayerId),
    /// Audio-layer Send dropdown clicked — opens the send picker.
    AudioSendClicked(LayerId),
    /// Route an audio layer to a send (layer, send id). `None` clears the
    /// layer's send routing (reverts the previously-fed send to a capture source).
    SetLayerAudioSend(LayerId, Option<AudioSendId>),
    /// Audio-layer Gain slider drag begins — snapshot for undo.
    AudioGainSnapshot(LayerId),
    /// Audio-layer Gain slider dragged to a new dB value (layer, dB).
    AudioGainChanged(LayerId, f32),
    /// Audio-layer Gain slider released — commit one undo step.
    AudioGainCommit(LayerId),
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

    // Inspector chrome — LED
    LedEnabledToggle,
    LedBrightnessSnapshot,
    LedBrightnessChanged(f32),
    LedBrightnessCommit,

    // Inspector chrome — Layer
    LayerChromeCollapseToggle,
    LayerOpacitySnapshot,
    LayerOpacityChanged(f32),
    LayerOpacityCommit,

    // Inspector chrome — Clip
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

    // Effect card (effect_index, param_index where applicable)
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
    ParamSnapshot(GraphParamTarget, ParamId),
    ParamChanged(GraphParamTarget, ParamId, f32),
    ParamCommit(GraphParamTarget, ParamId),
    /// Toggle a boolean (`isToggle`) param's ON/OFF button — a 0↔1 flip.
    /// Was `GenParamToggle(ParamId)` (generator-only); joined the unified
    /// group (§8.4 P3b) once effect cards gained the same toggle-row
    /// rendering generators already had — see `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md`
    /// §8. Same `ChangeGraphParamCommand` write path as `ParamChanged`, but
    /// atomic (no snapshot/commit pair — a click isn't a drag).
    ParamToggle(GraphParamTarget, ParamId),
    /// Fire a monotonic `isTrigger` param's "▶" button — increments the
    /// underlying counter by one instead of flipping it. Was
    /// `GenParamFire(ParamId)`; unified alongside `ParamToggle` for the same
    /// reason (D5b: `is_trigger` cards now exist on effect chains too).
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
    DriverToggle(GraphParamTarget, ParamId),
    EnvelopeToggle(GraphParamTarget, ParamId),
    DriverConfig(GraphParamTarget, ParamId, DriverConfigAction),
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

    // ── Layer-owned clip triggers (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_
    // UNIFICATION_DESIGN.md D2/D5) — the inspector's "AUDIO TRIGGERS"
    // section. A `LayerClipTrigger` has no `GraphParamTarget`/`ParamId` (it
    // addresses by `LayerId` + its index in `Layer.clip_triggers`), so this
    // is an ADDITIVE family parallel to `AudioMod*` above, not a repurposing
    // of it — `build_audio_mod_drawer` (the ONE shared drawer builder, D5)
    // is parameterized by `AudioModDrawerTarget` to emit whichever family
    // fits its caller.
    /// Toggle the AUDIO TRIGGERS section's collapse state — UI-local (mirrors
    /// `MacrosCollapseToggle`; no `Project` write, no persistence).
    AudioTriggerSectionToggle,
    /// Expand/collapse one row's drawer — UI-local, same as the section toggle.
    AudioTriggerRowExpandToggle(LayerId, usize),
    /// Append a new (disabled) `LayerClipTrigger` to the layer, sourcing the
    /// project's first audio send. No-op when no sends exist (mirrors
    /// `AudioModToggle`'s "arm" no-send case).
    AudioTriggerAdd(LayerId),
    /// Remove the clip trigger at `index`.
    AudioTriggerRemove(LayerId, usize),
    /// Flip `enabled` on the clip trigger at `index` — the row's own ON/OFF
    /// button (D4: a clip trigger has no Mode row to arbitrate with, so its
    /// existence isn't its enabled state the way a param audio-mod's is;
    /// `LayerClipTrigger::new` starts disabled by design — "the user enables
    /// a row once they've tuned it").
    AudioTriggerEnabledToggle(LayerId, usize),
    /// Set a clip trigger's source: which send + which feature (mirrors
    /// `AudioModSetSource`).
    AudioTriggerSetSource(LayerId, usize, AudioSendId, AudioFeature),
    /// Toggle a clip trigger's invert flag (mirrors `AudioModSetInvert`).
    AudioTriggerSetInvert(LayerId, usize),
    /// Toggle a clip trigger's rate-of-change flag (mirrors
    /// `AudioModSetRateOfChange`).
    AudioTriggerSetRateOfChange(LayerId, usize),
    /// Snapshot a clip trigger's shape before a drawer-slider drag (undo
    /// start) — mirrors `AudioModShapeSnapshot`.
    AudioTriggerShapeSnapshot(LayerId, usize),
    /// Live-edit one shape scalar during a drawer-slider drag (no undo
    /// entry) — mirrors `AudioModShapeParamChanged`.
    AudioTriggerShapeParamChanged(LayerId, usize, AudioShapeParam, f32),
    /// Commit a shape-slider drag as one undo step — mirrors
    /// `AudioModShapeCommit`.
    AudioTriggerShapeCommit(LayerId, usize),
    /// Set the one-shot fire length (`one_shot_beats`) — the drawer's Length
    /// row (D4/D5), clip triggers only.
    AudioTriggerSetLength(LayerId, usize, f32),

    // ── Audio Setup panel (project-level send routing) ──
    /// Open the input-device dropdown (anchored to the clicked trigger).
    AudioSetupDeviceClicked,
    /// Open a send's input-channel dropdown (anchored to the clicked trigger).
    AudioSendChannelClicked(AudioSendId),
    /// Set (or clear) the capture input device. `None` = system default input.
    AudioSetDevice(Option<AudioDeviceRef>),
    /// Add a new empty send.
    AudioAddSend,
    /// Remove a send by id.
    AudioRemoveSend(AudioSendId),
    /// Rename a send (commit with the new label).
    AudioRenameSend(AudioSendId, String),
    /// Begin inline editing of a send's label (clicked its name).
    AudioSendLabelClicked(AudioSendId),
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
    /// P4 (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D8, audio-dock sibling):
    /// double-click on the gain value cell opens its type-in box. Carries
    /// the send id, the current gain (dB) to prefill, and the cell's own
    /// node id (the app resolves its screen rect at open time — same
    /// convention as `SceneSetupBeginNumericTextInput`).
    AudioSendGainBeginTextInput(AudioSendId, f32, crate::node::NodeId),
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
    // The Audio Setup Triggers matrix (send-owned band routes) is gone (P3,
    // AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN D2): `AudioTriggerToggled`,
    // `AudioTriggerSensitivityStep`, `AudioSendSensitivityDragBegin/Changed/Commit`,
    // `AudioTriggerLengthStep`, `AudioTriggerLayerClicked`, `AudioTriggerSetLayer`
    // all deleted. Clip triggers are authored on the layer only
    // (`LayerClipTrigger`, P2); the Consumers rows below are the panel's sole
    // remaining (navigational) trigger display.
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
    /// Reorder effect card: move from_index to to_index.
    /// Unity: EffectsListBitmapPanel.onCardReorder.
    EffectReorder(usize, usize),
    /// Reorder multiple effect cards as a group: (sorted source indices, target index).
    EffectReorderGroup(Vec<usize>, usize),

    // Graph-editor node-graph mutations (add/connect/move/group/param/expose…)
    // are no longer PanelAction variants — they live in the focused
    // `crate::graph_edit::GraphEditCommand` (UI Architecture Overhaul Phase 4.3),
    // emitted by the canvas + sidebar and translated to `commands::graph::*` at
    // the app boundary. ToggleNodeParamExpose moved there too.

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

    // (Graph-editor node-graph mutations moved to
    // `crate::graph_edit::GraphEditCommand` — Phase 4.3. See the note above.)

    // ── Generator-only per-param actions ───────────────────────────────
    //
    // The effect/generator mirror pairs (snapshot / changed / commit /
    // right-click, drivers, envelope Amount, trims) collapsed into the
    // target-carrying `ParamSnapshot` … `TargetCommit` variants above. Only
    // these have no effect counterpart and stay generator-only.
    GenTypeClicked(Option<LayerId>), // layer_id

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

    // ── Browser: sources, badges, management (PRESET_LIBRARY_DESIGN P5, D6) ──
    // Right-click on a browser grid cell opens a management menu; `mode`
    // stands in for `PresetKind` (Effect/Generator only — Node mode never
    // reaches these, `browser_popup` screens it out) since this crate
    // mirrors core types rather than depending on `manifold-core`.
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

    // Macros panel collapse
    MacrosCollapseToggle,

    // Macro sliders (macro_index 0-7)
    MacroSnapshot(usize),
    MacroChanged(usize, f32),
    MacroCommit(usize),
    MacroLabelRightClick(usize), // macro_index — opens mappings dropdown
    MacroLabelRename(usize),     // macro_index — opens inline rename input

    // Macro mapping (from context menu on param right-click). Param
    // is addressed by `ParamId`, macro slot by positional index
    // (macros are a fixed 8-slot bank).
    MapParamToMacro(GraphParamTarget, ParamId, usize), // gpt, param_id, macro_idx
    UnmapMacro(usize, usize),                                                  // macro_idx, mapping_idx
    ClearMacroMappings(usize),                                                 // macro_idx

    // Param label right-click → opens "Map to Macro" / "Map from Ableton" context menu
    ParamLabelRightClick(GraphParamTarget, ParamId),

    // Ableton mapping (from context menu on param right-click). Map + unmap
    // both address the param by the unified `GraphParamTarget`; the dispatch
    // resolves the `AbletonMappingTarget` through the one shared helper.
    MapParamToAbleton(
        GraphParamTarget,
        ParamId,
        AbletonMacroAddress,
    ), // gpt, param_id, address
    UnmapParamAbleton(GraphParamTarget, ParamId), // gpt, param_id
    /// Open the Ableton picker popup for a param (effect or generator).
    OpenAbletonPickerForParam(GraphParamTarget, ParamId), // gpt, param_id
    /// Ableton mapping for macro slots.
    MapMacroToAbleton(usize, AbletonMacroAddress),
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
    AbletonInvertToggle(GraphParamTarget, ParamId),
    AbletonMacroInvertToggle(usize),                             // slot_idx

    // Reset macro from context menu (distinct from the macro slider's own
    // right-click reset, now the generic SliderReset trio, to avoid
    // re-triggering the mappings dropdown)
    MacroReset(usize), // macro_idx — reset to 0 from context menu

    // Inspector scroll
    InspectorScrolled(f32),
    InspectorSectionClicked(usize),

    // Timeline
    Seek(f32),
    SetInsertCursor(f32),
    /// Overview strip scrub — normalized [0,1] position. Centers viewport.
    OverviewScrub(f32),
    /// Horizontal scrollbar drag/jump — an absolute scroll-x in beats (§24 5e).
    TimelineScrollbarH(f32),

    // Viewport clip interaction (generated by InteractionOverlay, not panels)
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
    /// "Clear Automation" context-menu item: empties the lane's points,
    /// keeping the (now-empty) lane — `ClearLaneCommand`.
    ContextClearAutomationLane(UiGraphTarget, ParamId),
    /// "Remove Lane" context-menu item: deletes the whole lane —
    /// `RemoveLaneCommand`.
    ContextRemoveAutomationLane(UiGraphTarget, ParamId),

    // Layer management
    AddLayerClicked,
    DeleteLayerClicked(LayerId),

    // Effect management
    AddEffectClicked(InspectorTab),
    RemoveEffect(usize),
    BrowserSearchClicked,
    PasteEffects,

    // OSC — click param label to copy address to system clipboard.
    // Unity: UIElementBuilder.CopyToClipboardLabel.
    CopyOscAddress(String),

    // Dropdown results (context-routed from UIRoot) — layer-keyed by stable
    // `LayerId` (threaded through from the opening `Midi*Clicked` action), not
    // a positional index, for the same reason as the layer-header family above.
    SetMidiNote(LayerId, i32),              // layer, note (0-127)
    SetMidiChannel(LayerId, i32),           // layer, channel (0-15 internal, displayed 1-16)
    SetMidiDevice(LayerId, Option<String>), // layer, device name (None = any)
    SetMidiTriggerMode(LayerId, MidiTriggerMode),
    SetResolution(usize),           // preset index
    SetDisplayResolution(i32, i32), // direct width, height (no undo, matches Unity)
    SetRenderScale(f32),            // render scale: 1.0 (native), 0.75 (quality), 0.5 (performance)
    SetTonemapCurve(TonemapCurve),
    AddEffect(InspectorTab, PresetTypeId), // tab, preset type id
    SetGenType(Option<LayerId>, PresetTypeId), // layer_id, preset type id
    SetMidiClockDevice(i32),            // MIDI device index

    // Layer header right-click
    LayerHeaderRightClicked(LayerId),

    // Context menu results
    ContextSplitAtPlayhead(String),  // clip_id
    ContextDeleteClip(String),       // clip_id
    ContextDuplicateClip(String),    // clip_id
    ContextPasteAtTrack(f32, usize), // beat, layer (track-content click; positional by design — no stable layer identity at that hit-test site)
    // Layer-header context-menu family — LayerId-keyed (BUG-031: index-based
    // addressing let a menu item resolved at open time hit the wrong layer if
    // the list changed before the click). Consumers re-resolve the current
    // index from the id at dispatch time, mirroring `DeleteLayerClicked`.
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

    // Timeline Markers
    MarkerClicked(String, Modifiers), // marker_id, modifiers (Shift for multi-select)
    MarkerDoubleClicked(String),      // marker_id (rename)
    MarkerDragStarted(String),        // marker_id
    MarkerDragMoved(String, f32),     // marker_id, new_beat
    MarkerDragEnded(String, f32),     // marker_id, final_beat
    MarkerRightClicked(String),       // marker_id (context menu)
    DeleteSelectedMarkers,

    // Generic dropdown fallback (should not normally reach dispatch)
    DropdownSelected(usize),
}

impl PanelAction {
    /// Build a [`PanelAction::SliderReset`] from a slider's own value-change
    /// trio — the same three actions a drag would emit, with `changed` carrying
    /// the slider's declared default. Boxing happens here so call sites read as
    /// plain action literals.
    pub fn slider_reset(snapshot: PanelAction, changed: PanelAction, commit: PanelAction) -> PanelAction {
        PanelAction::SliderReset {
            snapshot: Box::new(snapshot),
            changed: Box::new(changed),
            commit: Box::new(commit),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncSource {
    Internal,
    AbletonLink,
    Midi,
}

/// The scope an inspector tab addresses — a rung in the selection's ownership
/// hierarchy (clip → layer → group → master). `Group` is a group *layer*
/// (`LayerType::Group`), so internally it renders through the same column as
/// `Layer`; only the tab label and which layer the app feeds differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Master,
    Layer,
    Group,
    Clip,
}

impl InspectorTab {
    /// Group renders through the layer column (a group is a layer). Used to
    /// fold `Group` into the existing `Layer` section gates.
    pub fn is_layer_scope(self) -> bool {
        matches!(self, InspectorTab::Layer | InspectorTab::Group)
    }
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

    /// Register node-intent dispatch for this panel's discrete gestures
    /// (click / double-click / right-click). Called after `build()` from the
    /// node ids the panel stored during build. Migrated panels override this
    /// and drop the matching arms from `handle_event`; un-migrated panels keep
    /// the default no-op and their existing `handle_event` matching.
    ///
    /// See `docs/NODE_INTENT_DISPATCH.md`.
    fn register_intents(&self, _intents: &mut crate::intent::IntentRegistry) {}

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
