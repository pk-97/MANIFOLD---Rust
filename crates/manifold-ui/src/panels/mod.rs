pub mod ableton_picker;
pub mod actions;
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

// Per-domain intent enums (the FLAT sum decomposition of `PanelAction`, P-D /
// D-D1). Re-exported at the `panels::` level so existing `panels::PanelAction`
// call sites reach the domain enums by the same path.
pub use actions::{
    AudioSetupAction, BrowserAction, ClipAction, EditingAction, LayerAction, MappingAction,
    MarkerAction, ModulationAction, ParamsAction, ProjectAction, RootAction, TransportAction,
};

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
/// exactly as the old `Gen*` arms did). `GeneratorOf` carries an explicit
/// `LayerId` for dispatch sites that must NOT resolve through the active
/// layer — BUG-292: the scene panel's rows edit its own bound layer
/// (`ScenePanel::live_layer_id`), which can legitimately differ from the
/// app's `active_layer`; routing those rows through plain `Generator`
/// silently wrote to the wrong layer. `LayerId` wraps an `Arc<str>` (not
/// `Copy`), so this variant costs the enum its `Copy` impl — every other
/// call site keeps using `Generator`/`Effect` unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphParamTarget {
    Effect(usize),
    Generator,
    GeneratorOf(LayerId),
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
    Transport(TransportAction),
    Editing(EditingAction),
    Layer(LayerAction),
    Marker(MarkerAction),
    Project(ProjectAction),
    Browser(BrowserAction),
    Clip(ClipAction),
    Params(ParamsAction),
    Modulation(ModulationAction),
    Mapping(MappingAction),
    AudioSetup(AudioSetupAction),
    Root(RootAction),
}

impl PanelAction {
    /// Build a [`PanelAction::SliderReset`] from a slider's own value-change
    /// trio — the same three actions a drag would emit, with `changed` carrying
    /// the slider's declared default. Boxing happens here so call sites read as
    /// plain action literals.
    pub fn slider_reset(snapshot: PanelAction, changed: PanelAction, commit: PanelAction) -> PanelAction {
        PanelAction::Root(RootAction::SliderReset {
            snapshot: Box::new(snapshot),
            changed: Box::new(changed),
            commit: Box::new(commit),
        })
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
