pub mod transport;
pub mod header;
pub mod footer;
pub mod layer_header;
pub mod master_chrome;
pub mod layer_chrome;
pub mod clip_chrome;
pub mod effect_card;
pub mod gen_param;
pub mod inspector;
pub mod viewport;
pub mod dropdown;

use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::tree::UITree;

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
    ToggleMonitor,

    // Footer
    CycleQuantize,
    ResolutionClicked,
    FpsFieldClicked,

    // Inspector tab
    SelectInspectorTab(InspectorTab),

    // Layer
    ToggleMute(usize),
    ToggleSolo(usize),
    SetBlendMode(usize, String),
    ExpandLayer(usize),
    CollapseLayer(usize),

    // Layer header
    LayerClicked(usize),
    LayerDoubleClicked(usize),
    ChevronClicked(usize),
    BlendModeClicked(usize),
    FolderClicked(usize),
    NewClipClicked(usize),
    AddGenClipClicked(usize),
    MidiInputClicked(usize),
    MidiChannelClicked(usize),
    LayerDragStarted(usize),
    LayerDragMoved(usize, usize),
    LayerDragEnded(usize, usize),

    // Inspector chrome — Master
    MasterCollapseToggle,
    MasterExitPathClicked,
    MasterOpacitySnapshot,
    MasterOpacityChanged(f32),
    MasterOpacityCommit,
    MasterOpacityRightClick,

    // Inspector chrome — Layer
    LayerChromeCollapseToggle,
    LayerOpacitySnapshot,
    LayerOpacityChanged(f32),
    LayerOpacityCommit,

    // Inspector chrome — Clip
    ClipChromeCollapseToggle,
    ClipBpmClicked,
    ClipLoopToggle,
    ClipSlipSnapshot,
    ClipSlipChanged(f32),
    ClipSlipCommit,
    ClipLoopSnapshot,
    ClipLoopChanged(f32),
    ClipLoopCommit,

    // Effect card (effect_index, param_index where applicable)
    EffectToggle(usize),
    EffectCollapseToggle(usize),
    EffectCardClicked(usize),
    EffectParamSnapshot(usize, usize),
    EffectParamChanged(usize, usize, f32),
    EffectParamCommit(usize, usize),
    EffectParamRightClick(usize, usize),
    EffectDriverToggle(usize, usize),
    EffectEnvelopeToggle(usize, usize),
    EffectDriverConfig(usize, usize, DriverConfigAction),
    EffectEnvParamChanged(usize, usize, EnvelopeParam, f32),
    EffectTrimChanged(usize, usize, f32, f32),
    EffectTargetChanged(usize, usize, f32),

    // Generator params
    GenTypeClicked,
    GenParamSnapshot(usize),
    GenParamChanged(usize, f32),
    GenParamCommit(usize),
    GenParamRightClick(usize),
    GenParamToggle(usize),
    GenDriverToggle(usize),
    GenEnvelopeToggle(usize),
    GenDriverConfig(usize, DriverConfigAction),
    GenEnvParamChanged(usize, EnvelopeParam, f32),
    GenTrimChanged(usize, f32, f32),
    GenTargetChanged(usize, f32),

    // Inspector scroll
    InspectorScrolled(f32),
    InspectorSectionClicked(usize),

    // Timeline
    Seek(f32),
    SetInsertCursor(f32),
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
}
