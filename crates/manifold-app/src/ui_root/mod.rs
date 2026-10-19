//! UIRoot — owns the entire UI state for one window.
//!
//! Contains the UITree, UIInputSystem, ScreenLayout, all panels,
//! and the dropdown overlay. The app layer creates one UIRoot per
//! workspace window and forwards winit events through it.

use crate::frame_plan::MainRegion;
use manifold_core::work_count::{record_work, WorkKind};
use manifold_playback::ableton_bridge::AbletonSession;
use manifold_ui::input::{Key, Modifiers, PointerAction, UIEvent};
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::panels::overlay::{
    Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, compute_overlay_rect,
};
use manifold_ui::*;

mod events;
mod export;

mod card_pin;
mod dropdowns;
mod scene_menus;
// `build_picker_session` is called by `ui_bridge/state_sync.rs` at the historical
// `crate::ui_root::build_picker_session` path; the re-export preserves it after
// the fn moved into the `dropdowns` submodule (P-F2a).
pub(crate) use dropdowns::build_picker_session;

mod drag;

mod overlay;

/// The top-level overlays, in bottom→top z-order. The single registry the
/// overlay driver (build / draw / input) iterates — adding an overlay means a
/// field, an `overlay_mut` arm, and a `Z_ORDER` entry, and the exhaustive match
/// then forces the wiring. See `docs/OVERLAY_SYSTEM_DESIGN.md`.
///
/// `pub(crate)`: `text_input::TextSessionOwner` (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`
/// section 3, D2) tags a text session with the id of the overlay hosting it, so the
/// crate needs to name overlays outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayId {
    PerfHud,
    Dropdown,
    // Audio Setup is no longer an overlay — it is a `ScreenLayout` dock column
    // built from the root pass and routed at the docked-panel site
    // (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D1/section 3.5).
    Settings,
    RtQuality,
    BrowserPopup,
    AbletonPicker,
    ExportProgress,
    Toast,
}

impl OverlayId {
    /// Bottom → top: later = higher z (drawn last / on top, first to receive
    /// input). The perf HUD sits at the bottom so a real modal always covers it.
    /// The dropdown sits on top: it's a transient selection surface opened *from*
    /// another overlay (e.g. the Audio Setup modal's device/channel pickers), so
    /// it must render above whatever spawned it. Export progress covers all
    /// authoring overlays until the content thread finishes the run. The toast (D11,
    /// `UI_CRAFT_AND_MOTION_PLAN.md` P2) sits topmost of all — a status message
    /// must stay legible over an open modal/dropdown, not be hidden by one.
    const Z_ORDER: [OverlayId; 8] = [
        OverlayId::PerfHud,
        OverlayId::Settings,
        OverlayId::RtQuality,
        OverlayId::BrowserPopup,
        OverlayId::AbletonPicker,
        OverlayId::Dropdown,
        OverlayId::ExportProgress,
        OverlayId::Toast,
    ];
}

/// Who owns the in-flight pointer drag. Resolved once per gesture (D1) by
/// [`UIRoot::resolve_drag_owner`], cleared by the terminal broadcast (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragOwner {
    /// An open overlay claimed it (modal, or origin inside its rect — section 3.2).
    Overlay(OverlayId),
    /// The timeline tracks surface → stashed to `InteractionOverlay`.
    TimelineTracks,
    /// Inspector slider / effect-card drag (`pressed_target` / card-drag).
    Inspector,
    /// Layer-header reorder or gain drag.
    LayerHeaders,
    /// Timeline ruler scrub (D7 — confirmed kept: `viewport/interaction.rs`'s
    /// `ViewportDragMode::RulerScrub` consumes `Drag` events).
    Ruler,
}


/// What the currently-open dropdown is selecting for.
#[derive(Debug, Clone)]
pub enum DropdownContext {
    LayerContext(manifold_core::LayerId), // survives only for its color swatches (text items are typed)
    /// A row's colour menu: a picked colour is one RGB write to these ids.
    Colour {
        target: manifold_ui::panels::GraphParamTarget,
        param_ids: [manifold_core::effects::ParamId; 3],
    },
}

/// A project-embedded ("forked") preset surfaced into the Add pickers. Carries
/// only what the picker needs: the stable id to select by, the display name,
/// and which picker (effect / generator) it belongs in.
#[derive(Clone)]
pub struct EmbeddedPresetItem {
    pub kind: manifold_core::preset_def::PresetKind,
    pub type_id: String,
    pub display_name: String,
    /// PRESET_LIBRARY_DESIGN P5, D6: `Saved` entries are always listed
    /// ("This Project"); `Snapshot` entries are listed only when their id
    /// resolves nowhere else (badged "missing from library") — the
    /// classification lives in `build_preset_picker_items`, which needs this
    /// to tell the two apart.
    pub origin: manifold_core::project::EmbeddedOrigin,
}

/// Manifest-backed parameter choice for the automation chooser. The action is
/// stored with the candidate at projection time, retaining the exact target
/// and parameter identity through filtering and popup navigation.
pub(crate) struct AutomationChooserCandidate {
    pub layer_id: manifold_core::LayerId,
    pub item: manifold_ui::panels::picker_core::PickerItem,
    pub action: PanelAction,
}

/// Owns all UI state for one window.
pub struct UIRoot {
    // Core
    pub tree: UITree,
    /// Read-only content-clock BPM for the frame-aligned driver readout.
    pub driver_bpm: Option<manifold_core::Bpm>,
    /// Failed effect-chain builds from the content thread, for the inspector.
    pub chain_failures: crate::content_state::ChainFailures,
    pub input: UIInputSystem,
    pub layout: ScreenLayout,
    /// Node-intent dispatch: maps a gesture on a node to a `PanelAction`,
    /// resolved by folding a hit node up its parent chain. Repopulated from
    /// panel-stored node ids only when the tree structurally changes (gated on
    /// `intents_structure_version`) — never per-frame, so no hot-path
    /// allocation. See `docs/NODE_INTENT_DISPATCH.md`.
    intents: manifold_ui::intent::IntentRegistry,
    /// Tree `structure_version` the intent registry was last built against.
    /// `u64::MAX` forces a repopulate on the first `process_events`.
    intents_structure_version: u64,

    // Panels
    pub transport: TransportPanel,
    pub header: HeaderPanel,
    pub footer: FooterPanel,
    pub layer_headers: LayerHeaderPanel,
    pub inspector: InspectorCompositePanel,
    pub viewport: TimelineViewportPanel,
    /// Background-decoded per-clip waveform peaks for audio-layer clips, attached
    /// to each `ViewportClip` on sync. See `docs/AUDIO_LAYER_DESIGN.md`.
    pub audio_waveforms: crate::audio_waveform_cache::AudioWaveformCache,
    pub dropdown: DropdownPanel,
    pub browser_popup: manifold_ui::panels::browser_popup::BrowserPopupPanel,
    pub audio_setup_panel: manifold_ui::panels::audio_setup_panel::AudioSetupPanel,
    pub scene_setup_panel: manifold_ui::panels::scene_setup_panel::ScenePanel,
    pub settings_popup: manifold_ui::panels::settings_popup::SettingsPopup,
    pub rt_quality_panel: manifold_ui::panels::rt_quality_panel::RtQualityPanel,
    pub perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel,
    pub export_progress: manifold_ui::panels::export_progress::ExportProgressPanel,
    /// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2). Fired by
    /// `Application` on `M::Undo`/`M::Redo` (see `app_render.rs`); ticked every
    /// frame in `update()`.
    pub toast: manifold_ui::panels::toast::ToastPanel,
    /// Last undo/redo data version shown in the transient status toast.
    pub last_undo_redo_toast_key: Option<u64>,

    /// Scratch for the panel list handed to a window's cache manager each
    /// painted frame (`panel_cache_info_into`), kept so painting allocates nothing.
    pub(crate) panel_subs_scratch: Vec<(usize, usize)>,
    pub(crate) panel_info_scratch: Vec<manifold_ui_paint::ui_cache_manager::PanelCacheInfo<'static>>,
    /// Last rejected graph-edit diagnostic shown by the UI. Rejections do not
    /// bump `data_version`, so this guard keys directly on the content event's
    /// sequence and remains stable across repeated snapshots.
    pub last_graph_edit_diagnostic_sequence: Option<u64>,
    pub last_trigger_delivery_failure: Option<manifold_playback::engine::trigger_delivery::TriggerDeliveryFailure>,
    pub last_edit_selection_sequence: Option<u64>,
    /// Queued layer reveal, taken by the project projection. Written only
    /// through [`Self::request_layer_reveal`], which moves its version.
    pending_layer_reveal: Option<manifold_core::LayerId>,
    layer_reveal_version: u64,
    pub last_modifier_selection_sequence: Option<u64>,
    pub last_object_modifier_selection_sequence: Option<u64>,

    /// Load-time warmup progress shown as a centered overlay during project
    /// open. `None` when no warmup is active.
    pub warmup: Option<manifold_core::WarmupProgress>,

    /// Project-embedded ("forked") presets surfaced into the Add pickers,
    /// rebuilt when the EmbeddedPresets stamp moves.
    pub embedded_presets: Vec<EmbeddedPresetItem>,

    // State
    built: bool,
    screen_width: f32,
    screen_height: f32,
    time_accumulator: f32,

    /// Context for the currently-open dropdown (set before open, read on selection).
    dropdown_context: Option<DropdownContext>,

    /// Detected display resolutions from winit monitors: (w, h, label).
    /// Set by Application after monitor enumeration.
    display_resolutions: Vec<(u32, u32, String)>,

    /// Cached master effect names for the LED exit path dropdown.
    /// Updated by state_sync when project changes.
    pub master_effect_names: Vec<String>,

    /// MIDI clock device names for the CLK device dropdown: the content
    /// thread's retained Arc, taken when its pointer changes.
    pub midi_device_names: std::sync::Arc<[String]>,

    /// Currently selected audio input device name for live recording.
    pub selected_audio_input_device: Option<String>,

    /// Cached device metadata for the Audio Setup modal (channel names, UID,
    /// liveness, subdevice grouping). Refreshed from the device directory when
    /// the device dropdown opens, and read when a device or channel is chosen.
    audio_setup_devices: Vec<manifold_audio::directory::DeviceInfo>,
    /// Cached tappable-application metadata for the Audio Setup source dropdown,
    /// refreshed alongside `audio_setup_devices`. Empty on OSes without app-audio
    /// tapping.
    audio_setup_apps: Vec<manifold_audio::directory::AppAudioSource>,
    /// Candidate routing layers (id + name) for the audio-clip detection
    /// target-layer dropdowns. Refreshed by `state_sync` when an audio clip is
    /// selected; read when an instrument's layer dropdown opens.
    clip_detect_layers: Vec<(manifold_core::LayerId, String)>,

    // Inspector resize state
    pub inspector_resize_dragging: bool,
    inspector_drag_start_x: f32,
    inspector_drag_start_width: f32,

    // Audio Setup dock resize state (D1) — mirror of the inspector pair.
    pub audio_setup_resize_dragging: bool,
    audio_setup_drag_start_x: f32,
    audio_setup_drag_start_width: f32,

    /// Bumped by `mark_overlay_dirty` (UI_INVALIDATION D6); the open set is
    /// folded in by `overlays_version`.
    overlay_version: u64,

    /// Effect clipboard count (set by app.rs, used by browser popup).
    pub effect_clipboard_count: usize,

    /// Effect clipboard for copy/paste between effect chains.
    pub effect_clipboard: manifold_editing::clipboard::EffectClipboard,
    /// Generator clipboard for copy/paste between generator layers.
    pub gen_clipboard: manifold_editing::clipboard::GeneratorClipboard,
    /// UI-owned immutable scene-modifier snapshot used by copy/paste.
    /// Written only through `set_scene_modifier_clipboard` so the latest
    /// copy wins across both clipboards.
    pub scene_modifier_clipboard: Option<crate::scene_modifier_transfer::ModifierClipboard>,
    pub scene_item_clipboard: Option<crate::scene_item_transfer::SceneItemClipboard>,
    pub object_modifier_clipboard: Option<crate::object_modifier_transfer::ObjectModifierClipboard>,
    pub object_cards_have_focus: bool,

    /// Hover actions produced by continuous cursor movement, drained in process_events.
    cursor_hover_actions: Vec<PanelAction>,

    /// `PanelAction`s produced by keyboard-driven overlay picking outside the
    /// normal per-frame event queue (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`
    /// section 4/section 5 P2 arrow/Enter nav). An active `SearchFilter` text session
    /// intercepts keys in `window_input.rs` before they ever reach
    /// `route_overlay_event`, so the browser popup's `handle_key_nav` is
    /// called directly from there; the resulting action is stashed here and
    /// drained in `process_events`/`route_inspector_events` the same way
    /// `cursor_hover_actions` is, so it reaches the same dispatch pipeline a
    /// mouse-click pick would.
    pub pending_keyboard_actions: Vec<PanelAction>,

    /// Viewport-area events stashed for InteractionOverlay processing by app.rs.
    /// Events in the tracks area that need host trait access are stored here
    /// during process_events() and drained by app.rs to route through the overlay.
    viewport_events: Vec<manifold_ui::input::UIEvent>,

    /// Last right-click screen position, persisted across process_events() so
    /// overlay-generated actions (TrackRightClicked, ClipRightClicked) can anchor
    /// their dropdown menus after the main event loop returns.
    last_right_click_pos: Vec2,
    /// One-shot view request, consumed after lane geometry is projected.
    pub(crate) pending_automation_reveal: Option<(manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId)>,
    pub(crate) automation_chooser_candidates: Vec<AutomationChooserCandidate>,
    pub(crate) trigger_routing: crate::ui_bridge::TriggerRoutingCatalog,
    /// Queued inspector reveals, taken by the inspector projection. Written
    /// only through the `request_*` setters, which move `inspector_reveal_version`.
    pending_trigger_response_reveal: Option<(manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId)>,
    /// Show in Scene tab / a card section title: revealed once the Scene tab
    /// is projected (SCENE_SURFACES_DESIGN.md D10). A section carries all its
    /// rows; the Scene tab picks which to reveal.
    pending_scene_reveal: Option<(manifold_ui::view::UiGraphTarget, Vec<manifold_core::effects::ParamId>)>,
    inspector_reveal_version: u64,
    /// Moves each time the inspector projection rewrote this root's inspector
    /// lists: the graph editor's card lane rebuilds on it (D17).
    inspector_projection_version: u64,
    pub(crate) pinned_automation_lanes: Vec<manifold_ui::ui_state::AutomationLaneKey>,

    /// Cached macro slot labels for context menu display.
    pub macro_labels: [String; manifold_core::MACRO_COUNT],
    /// Cached macro mapping descriptions per slot (for context menu).
    pub macro_mapping_descs: [Vec<String>; manifold_core::MACRO_COUNT],
    /// Whether each macro slot has an Ableton mapping (for "Remove" menu item).
    pub macro_ableton_mapped: [bool; manifold_core::MACRO_COUNT],
    /// The macro slider labels with their Ableton suffix, rewritten in place
    /// by every value push.
    pub(crate) macro_display_labels: [String; manifold_core::MACRO_COUNT],
    /// The LED exit path label, rewritten in place by every value push.
    pub(crate) led_exit_label: String,

    /// Node ID for the video/timeline split handle (color feedback on hover/drag).
    /// From Unity PanelResizeHandle.cs — idle/hover/drag color states.
    split_handle_id: Option<NodeId>,
    /// Node ID for the inspector resize handle (vertical bar at inspector right edge).
    inspector_handle_id: Option<NodeId>,
    /// Node ID for the Audio Setup dock resize handle (vertical bar at the
    /// dock's LEFT edge). `None` while the dock is closed. (D1)
    audio_setup_handle_id: Option<NodeId>,
    /// P2 "panel-split snap-back" (D15): self-tracked elapsed time for
    /// `layout.tick_splits`, same self-contained-`Instant` shape as
    /// `InspectorCompositePanel::update`'s `motion_last_tick`.
    layout_tick_last: std::time::Instant,

    /// Single source of truth for who owns the in-flight pointer drag gesture
    /// (`docs/DRAG_CAPTURE_DESIGN.md` D1). Resolved once at the gesture's
    /// first `DragBegin` (`resolve_drag_owner`), cleared by the terminal
    /// broadcast (`broadcast_gesture_end`) — replaces the old boolean
    /// drag-active latch + `is_event_in_tracks_area`'s positional gate for
    /// Drag/DragEnd.
    drag_owner: Option<DragOwner>,
    /// A press forwarded to the tracks needs its release even before a drag starts.
    tracks_press_active: bool,

    /// Cached Ableton session for the picker popup.
    pub ableton_session: Option<std::sync::Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Two-column Ableton macro picker popup (replaces flat dropdown Ableton section).
    pub ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup,
    /// Which param triggered the picker — resolved when picker returns Selected.
    ableton_picker_context: Option<manifold_ui::panels::ableton_picker::AbletonPickerContext>,
    /// Set when the Ableton picker opens — drained by app_render to send
    /// `AbletonRediscover` on the content thread so the picker shows fresh data.
    pub ableton_rediscovery_needed: bool,

    /// Slot ranges `[start, end)` in the Overlays region of each open overlay,
    /// in z-order, recorded by `build_overlays`. The draw pass renders these at `Depth::OVERLAY` offset
    /// by stack index — so build and draw share one source and cannot drift
    /// (the bug class this system eliminates).
    pub overlay_draw: Vec<(usize, usize)>,
    /// On-screen rect of each OPEN overlay as of the last `build_overlays` —
    /// the same `rect` value passed to that overlay's `build_at`. Read by
    /// `overlay_contains_point` (D5, `docs/DRAG_CAPTURE_DESIGN.md` section 2) so the
    /// window-seam press interceptors in `window_input.rs` can yield to a
    /// floating overlay instead of punching straight through it. Limitation:
    /// `Anchor::SelfManaged` overlays (dropdown, browser popup, Ableton
    /// picker) position themselves inside `build_at` from the raw screen
    /// size, so their entry here is a placeholder `(0,0,w,h)` rect, not their
    /// true footprint — none of the three are relevant to the BUG-059 case
    /// this guards (the docked Audio Setup panel), and the placeholder sits
    /// at the screen origin, away from where the seams live in practice.
    overlay_rects: Vec<(OverlayId, Rect)>,
    /// Overlays whose `is_open()` flipped false since the last drain —
    /// `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` section 3, D2. Populated by
    /// `route_overlay_event` (covers Escape and normal click routing for both
    /// windows) and `note_overlay_closed_if` (covers the graph-editor window's
    /// bespoke browser-popup routing, which bypasses `route_overlay_event`).
    /// Drained once per frame per window by the app pump via
    /// `take_closed_overlays`, which cancels any text session the closed
    /// overlay owned — the same stash-and-drain shape as
    /// `drain_overlay_selections`, just for text-session ownership instead of
    /// dropdown/Ableton-picker selections.
    closed_overlays: smallvec::SmallVec<[OverlayId; 2]>,
}

impl UIRoot {
    /// Queue a timeline layer reveal. Compare-before-bump: re-queueing the
    /// pending one moves nothing.
    pub(crate) fn request_layer_reveal(&mut self, id: manifold_core::LayerId) {
        if self.pending_layer_reveal.as_ref() != Some(&id) {
            self.pending_layer_reveal = Some(id);
            self.layer_reveal_version += 1;
        }
    }

    pub(crate) fn take_layer_reveal(&mut self) -> Option<manifold_core::LayerId> {
        self.pending_layer_reveal.take()
    }

    #[cfg(test)]
    pub(crate) fn pending_layer_reveal(&self) -> Option<&manifold_core::LayerId> {
        self.pending_layer_reveal.as_ref()
    }

    /// `FrameInputs.layer_reveal`.
    pub(crate) fn layer_reveal_version(&self) -> u64 {
        self.layer_reveal_version
    }

    /// Queue a Scene tab reveal of `params` (one row, or a card section's
    /// rows). Compare-before-bump.
    pub(crate) fn request_scene_reveal(
        &mut self,
        target: manifold_ui::view::UiGraphTarget,
        params: Vec<manifold_core::effects::ParamId>,
    ) {
        let next = Some((target, params));
        if self.pending_scene_reveal != next {
            self.pending_scene_reveal = next;
            self.inspector_reveal_version += 1;
        }
    }

    /// Queue a reveal of the card or Scene row that routes this trigger's
    /// response. Compare-before-bump.
    pub(crate) fn request_trigger_response_reveal(
        &mut self,
        target: manifold_ui::view::UiGraphTarget,
        param: manifold_core::effects::ParamId,
    ) {
        let next = Some((target, param));
        if self.pending_trigger_response_reveal != next {
            self.pending_trigger_response_reveal = next;
            self.inspector_reveal_version += 1;
        }
    }

    pub(crate) fn take_scene_reveal(
        &mut self,
    ) -> Option<(manifold_ui::view::UiGraphTarget, Vec<manifold_core::effects::ParamId>)> {
        self.pending_scene_reveal.take()
    }

    pub(crate) fn take_trigger_response_reveal(
        &mut self,
    ) -> Option<(manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId)> {
        self.pending_trigger_response_reveal.take()
    }

    #[cfg(test)]
    pub(crate) fn pending_trigger_response_reveal(
        &self,
    ) -> Option<&(manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId)> {
        self.pending_trigger_response_reveal.as_ref()
    }

    /// `FrameInputs.inspector_reveal`.
    pub(crate) fn inspector_reveal_version(&self) -> u64 {
        self.inspector_reveal_version
    }

    /// The inspector projection rewrote this root's inspector lists.
    pub(crate) fn note_inspector_projected(&mut self) {
        self.inspector_projection_version += 1;
    }

    /// `EditorInputs.inspector_projected`.
    pub(crate) fn inspector_projection_version(&self) -> u64 {
        self.inspector_projection_version
    }

    /// Take the content thread's shared lists by pointer: each is compared
    /// against the Arc this root retains, never element by element
    /// (UI_INVALIDATION_DESIGN.md section 3). True when the Ableton session
    /// changed.
    pub fn intake_shared(&mut self, content_state: &crate::content_state::ContentState) -> bool {
        if !std::sync::Arc::ptr_eq(&self.midi_device_names, &content_state.midi_device_names) {
            self.midi_device_names = std::sync::Arc::clone(&content_state.midi_device_names);
        }
        match (&self.ableton_session, &content_state.ableton_session) {
            (Some(held), Some(sent)) if std::sync::Arc::ptr_eq(held, sent) => false,
            (_, Some(sent)) => {
                self.ableton_session = Some(std::sync::Arc::clone(sent));
                true
            }
            (_, None) => false,
        }
    }

    /// Refresh only when a frame-aligned period changes, not on every clock sample.
    pub fn sync_driver_bpm(&mut self, project: &manifold_core::project::Project, bpm: manifold_core::Bpm) -> bool {
        let previous = self.driver_bpm.replace(bpm).unwrap_or(project.settings.bpm);
        if previous == bpm { return false; }
        record_work(WorkKind::DriverScan);
        let fps = project.settings.frame_rate;
        project.settings.master_effects.iter()
            .chain(project.timeline.layers.iter().flat_map(|l| l.effects.iter().flatten()))
            .chain(project.timeline.layers.iter().filter_map(|l| l.gen_params()))
            .any(|inst| inst.drivers.iter().flatten().any(|d| d.enabled && d.frame_aligned
                && d.effective_period_frames(previous, fps) != d.effective_period_frames(bpm, fps)))
    }

    pub fn new() -> Self {
        Self {
            // Give the build path real glyph-width measurement (size-to-content)
            // instead of the heuristic fallback. CachedTextMeasure is GPU-free, so
            // it installs here at construction; both windows' UIRoots get it.
            tree: {
                let mut tree = UITree::new();
                tree.set_text_measure(Box::new(
                    manifold_ui_paint::native_text::CachedTextMeasure::new(),
                ));
                // Event-time overlay mints (a dropdown opening) and the
                // card-drag ghost land in their own regions (D7).
                tree.set_region_roles(MainRegion::Overlays.id(), MainRegion::Ghosts.id());
                tree
            },
            driver_bpm: None,
            chain_failures: std::sync::Arc::from([]),
            input: UIInputSystem::new(),
            layout: ScreenLayout::new(1280.0, 720.0),
            intents: manifold_ui::intent::IntentRegistry::new(),
            intents_structure_version: u64::MAX,
            transport: TransportPanel::new(),
            header: HeaderPanel::new(),
            footer: FooterPanel::new(),
            layer_headers: LayerHeaderPanel::new(),
            inspector: InspectorCompositePanel::new(),
            viewport: {
                // Lane titles size with the same glyph widths the tree draws.
                let mut viewport = TimelineViewportPanel::new();
                viewport.set_text_measure(Box::new(
                    manifold_ui_paint::native_text::CachedTextMeasure::new(),
                ));
                viewport
            },
            audio_waveforms: crate::audio_waveform_cache::AudioWaveformCache::default(),
            dropdown: DropdownPanel::new(),
            browser_popup: manifold_ui::panels::browser_popup::BrowserPopupPanel::new(),
            audio_setup_panel: {
                let mut p = manifold_ui::panels::audio_setup_panel::AudioSetupPanel::new();
                // The scope's tick-lane legend, from the one lane definition
                // (names, colours, geometry all owned by spectral's scope.rs).
                use manifold_spectral::{LANE_HEIGHT_FRAC, ScopeOnsets};
                let legend = ScopeOnsets::LANE_LABELS
                    .iter()
                    .zip(ScopeOnsets::LANE_COLORS)
                    .map(|(name, [r, g, b])| {
                        let to8 = |c: f32| (c * 255.0).round() as u8;
                        (name.to_string(), manifold_ui::Color32::new(to8(r), to8(g), to8(b), 255))
                    })
                    .collect();
                p.set_scope_lane_legend(legend, LANE_HEIGHT_FRAC);
                p
            },
            scene_setup_panel: manifold_ui::panels::scene_setup_panel::ScenePanel::new(),
            settings_popup: manifold_ui::panels::settings_popup::SettingsPopup::new(),
            rt_quality_panel: manifold_ui::panels::rt_quality_panel::RtQualityPanel::new(),
            perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel::new(),
            toast: manifold_ui::panels::toast::ToastPanel::new(),
            export_progress: manifold_ui::panels::export_progress::ExportProgressPanel::new(),
            last_undo_redo_toast_key: None,
            panel_subs_scratch: Vec::new(),
            panel_info_scratch: Vec::new(),
            last_graph_edit_diagnostic_sequence: None,
            last_trigger_delivery_failure: None,
            last_edit_selection_sequence: None,
            pending_layer_reveal: None,
            layer_reveal_version: 0,
            last_modifier_selection_sequence: None,
            last_object_modifier_selection_sequence: None,
            embedded_presets: Vec::new(),
            built: false,
            screen_width: 1280.0,
            screen_height: 720.0,
            time_accumulator: 0.0,
            dropdown_context: None,
            display_resolutions: Vec::new(),
            master_effect_names: Vec::new(),
            midi_device_names: std::sync::Arc::from([]),
            selected_audio_input_device: None,
            audio_setup_devices: Vec::new(),
            audio_setup_apps: Vec::new(),
            clip_detect_layers: Vec::new(),
            inspector_resize_dragging: false,
            inspector_drag_start_x: 0.0,
            inspector_drag_start_width: 0.0,
            audio_setup_resize_dragging: false,
            audio_setup_drag_start_x: 0.0,
            audio_setup_drag_start_width: 0.0,
            overlay_version: 0,
            effect_clipboard_count: 0,
            effect_clipboard: manifold_editing::clipboard::EffectClipboard::new(),
            gen_clipboard: manifold_editing::clipboard::GeneratorClipboard::new(),
            scene_modifier_clipboard: None,
            object_modifier_clipboard: None,
            scene_item_clipboard: None,
            object_cards_have_focus: false,
            cursor_hover_actions: Vec::new(),
            pending_keyboard_actions: Vec::new(),
            viewport_events: Vec::new(),
            last_right_click_pos: Vec2::new(0.0, 0.0),
            pending_automation_reveal: None,
            automation_chooser_candidates: Vec::new(),
            trigger_routing: Default::default(),
            pending_trigger_response_reveal: None,
            pending_scene_reveal: None,
            inspector_reveal_version: 0,
            inspector_projection_version: 0,
            pinned_automation_lanes: Vec::new(),
            macro_labels: std::array::from_fn(|_| String::new()),
            macro_mapping_descs: std::array::from_fn(|_| Vec::new()),
            macro_ableton_mapped: [false; manifold_core::MACRO_COUNT],
            macro_display_labels: std::array::from_fn(|_| String::new()),
            led_exit_label: String::new(),
            split_handle_id: None,
            inspector_handle_id: None,
            audio_setup_handle_id: None,
            layout_tick_last: std::time::Instant::now(),
            drag_owner: None,
            tracks_press_active: false,
            ableton_session: None,
            ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup::new(),
            ableton_picker_context: None,
            ableton_rediscovery_needed: false,
            overlay_draw: Vec::new(),
            overlay_rects: Vec::new(),
            closed_overlays: smallvec::SmallVec::new(),
            warmup: None,
        }
    }

    /// Single write path for the scene-modifier clipboard. A successful
    /// modifier copy supersedes the effect clipboard: the latest copy wins
    /// across both, in both directions.
    pub fn set_scene_modifier_clipboard(
        &mut self,
        clipboard: Option<crate::scene_modifier_transfer::ModifierClipboard>,
    ) {
        if clipboard.is_some() {
            self.effect_clipboard.clear();
            self.object_modifier_clipboard = None;
            self.scene_item_clipboard = None;
        }
        self.scene_modifier_clipboard = clipboard;
    }

    /// Set detected display resolutions from winit monitors.
    pub fn set_display_resolutions(&mut self, resolutions: Vec<(u32, u32, String)>) {
        self.display_resolutions = resolutions;
    }

    /// One entry per atlas region with nodes, for UICacheManager: the
    /// region, the panel's slot range in that region's arena, and its screen
    /// rect. Each region is its own atlas slot (D7). A panel's range starts
    /// after its region root, as the cache's ancestor-aware scan expects;
    /// the inspector's runs to its arena's end so the docked Scene panel built
    /// after it paints in the same slot.
    ///
    /// `subs` is scratch that holds the inspector's sub-regions for the
    /// entries to borrow; both vectors keep their allocation across frames.
    pub fn panel_cache_info_into<'a>(
        &self,
        subs: &'a mut Vec<(usize, usize)>,
        info: &mut Vec<manifold_ui_paint::ui_cache_manager::PanelCacheInfo<'a>>,
    ) {
        use manifold_ui_paint::ui_cache_manager::{PanelCacheInfo, PanelSlot};

        self.inspector.sub_region_ranges_into(subs);
        let subs: &'a [(usize, usize)] = subs;
        info.clear();

        let len = |r: MainRegion| self.tree.region_len(r.id());
        let entries = [
            (PanelSlot::Transport, MainRegion::Transport, self.transport.node_range(), self.layout.transport_bar()),
            (PanelSlot::Header, MainRegion::Header, self.header.node_range(), self.layout.header()),
            (PanelSlot::Footer, MainRegion::Footer, self.footer.node_range(), self.layout.footer()),
            (
                PanelSlot::Inspector,
                MainRegion::Inspector,
                (self.inspector.first_node(), len(MainRegion::Inspector)),
                self.layout.inspector(),
            ),
            (PanelSlot::AudioDock, MainRegion::AudioDock, (0, len(MainRegion::AudioDock)), self.layout.audio_setup()),
            (PanelSlot::SplitHandles, MainRegion::SplitHandles, (0, len(MainRegion::SplitHandles)), self.layout.split_handle()),
            (PanelSlot::LayerHeaders, MainRegion::LayerHeaders, self.layer_headers.node_range(), self.layout.layer_controls()),
            (PanelSlot::Viewport, MainRegion::Viewport, self.viewport.node_range(), self.layout.timeline_body()),
        ];
        for (slot, region, (start, end), rect) in entries {
            if start < end && end <= len(region) {
                info.push(PanelCacheInfo {
                    slot,
                    region: region.id(),
                    start,
                    end,
                    rect,
                    sub_regions: (slot == PanelSlot::Inspector).then_some(subs),
                });
            }
        }
    }

    /// Each main region's rect, pixel-quantized: a frame-plan input (D7). A
    /// rect that moved translates its region; one that resized rebuilds it.
    pub(crate) fn region_rects(&self) -> crate::frame_plan::RegionRects {
        let quantize = |r: Rect| {
            [r.x.round() as i32, r.y.round() as i32, r.width.round() as i32, r.height.round() as i32]
        };
        let full_screen = Rect::new(0.0, 0.0, self.layout.screen_width, self.layout.screen_height);
        let mut rects = crate::frame_plan::RegionRects::default();
        for region in MainRegion::ALL {
            let rect = match region {
                MainRegion::Transport => self.layout.transport_bar(),
                MainRegion::Header => self.layout.header(),
                MainRegion::Footer => self.layout.footer(),
                MainRegion::Inspector => self.layout.inspector(),
                MainRegion::AudioDock => self.layout.audio_setup(),
                MainRegion::SplitHandles => self.layout.split_handle(),
                MainRegion::LayerHeaders => self.layout.layer_controls(),
                MainRegion::Viewport => self.layout.timeline_body(),
                MainRegion::Overlays => full_screen,
                // The ghost follows the drag, not a rect.
                MainRegion::Ghosts => Rect::ZERO,
            };
            rects.set(region, quantize(rect));
        }
        rects
    }

    /// A translated region's panel takes its new rect, so its next in-place
    /// update lays out where the nodes now are. Returns the exact move from
    /// where the region's nodes sit to the layout's rect: the plan's move is
    /// pixel-quantized, the layout's is not. Only regions the planner
    /// translates have an arm (`frame_plan::plan_regions`).
    pub(crate) fn follow_translate(&mut self, region: MainRegion) -> (f32, f32) {
        let delta = |from: Rect, to: Rect| (to.x - from.x, to.y - from.y);
        match region {
            MainRegion::Transport => {
                let to = self.layout.transport_bar();
                let by = delta(self.transport.rect(), to);
                self.transport.follow_rect(to);
                by
            }
            MainRegion::Header => {
                let to = self.layout.header();
                let by = delta(self.header.rect(), to);
                self.header.follow_rect(to);
                by
            }
            MainRegion::Footer => {
                let to = self.layout.footer();
                let by = delta(self.footer.rect(), to);
                self.footer.follow_rect(to);
                by
            }
            // The split handle keeps its node id; nothing else is held.
            MainRegion::SplitHandles => self
                .split_handle_id
                .map_or((0.0, 0.0), |id| delta(self.tree.get_bounds(id), self.layout.split_handle())),
            MainRegion::AudioDock => {
                let (dx, dy) = delta(self.audio_setup_panel.rect(), self.layout.audio_setup());
                self.audio_setup_panel.follow_translate(dx, dy);
                (dx, dy)
            }
            MainRegion::Inspector
            | MainRegion::LayerHeaders
            | MainRegion::Viewport
            | MainRegion::Overlays
            | MainRegion::Ghosts => {
                debug_assert!(false, "{region:?} rebuilds on a rect change; it never translates");
                (0.0, 0.0)
            }
        }
    }

    /// Give a resized region its new rect in place: a chrome bar takes the
    /// rect and the frame's chrome layout lays it out there (the planner
    /// marks a resized bar in place, and a bar whose shape changed rebuilds
    /// there), the split handle takes it. False for a region with no in-place
    /// path; the caller rebuilds it then. Only regions the planner resizes
    /// have an arm (`frame_plan::plan_regions`).
    pub(crate) fn follow_resize(&mut self, region: MainRegion) -> bool {
        let rect = match region {
            MainRegion::Transport => {
                let rect = self.layout.transport_bar();
                self.transport.follow_rect(rect);
                rect
            }
            MainRegion::Header => {
                let rect = self.layout.header();
                self.header.follow_rect(rect);
                rect
            }
            MainRegion::Footer => {
                let rect = self.layout.footer();
                self.footer.follow_rect(rect);
                rect
            }
            MainRegion::SplitHandles => {
                let Some(id) = self.split_handle_id else { return false };
                let rect = self.layout.split_handle();
                self.tree.set_bounds(id, rect);
                rect
            }
            _ => return false,
        };
        self.tree.set_region_rect(region.id(), rect);
        true
    }

    /// Whether any node of an atlas region is dirty, so the offscreen needs a
    /// repaint. Overlays and the ghost draw every frame through the overlay
    /// pass and are excluded.
    pub fn atlas_dirty(&self) -> bool {
        MainRegion::ALL
            .into_iter()
            .filter(|r| r.panel_slot().is_some())
            .any(|r| self.tree.has_dirty_in_region(r.id()))
    }

    /// Apply saved layout from project settings. Called after project load.
    /// Equivalent to Unity's WorkspaceController.ApplySavedLayout().
    pub fn apply_project_layout(&mut self, settings: &manifold_core::settings::ProjectSettings) {
        if settings.inspector_width > 0.0 {
            self.layout.inspector_width = settings
                .inspector_width
                .clamp(Self::INSPECTOR_MIN_W, Self::INSPECTOR_MAX_W);
        }
        if settings.timeline_height_percent > 0.0 {
            self.layout.timeline_split_ratio = settings.timeline_height_percent.clamp(
                manifold_ui::color::MIN_TIMELINE_SPLIT_RATIO,
                manifold_ui::color::MAX_TIMELINE_SPLIT_RATIO,
            );
        }
        // Restore viewport scroll + zoom
        if settings.viewport_pixels_per_beat > 0.0 {
            self.viewport.set_zoom(settings.viewport_pixels_per_beat);
        }
        self.viewport.set_scroll(
            settings.viewport_scroll_x_beats,
            settings.viewport_scroll_y_px,
        );
        // The header panel reads the viewport's scroll_y_px live at the next
        // build — it no longer keeps its own copy (D2).

        // Restore inspector collapse states
        self.inspector
            .macros_panel_mut()
            .set_collapsed(settings.macros_collapsed);
        self.inspector
            .master_chrome_mut()
            .set_collapsed(settings.master_chrome_collapsed);
        self.inspector
            .layer_chrome_mut()
            .set_collapsed(settings.layer_chrome_collapsed);
        self.inspector
            .clip_chrome_mut()
            .set_collapsed(settings.clip_chrome_collapsed);
        self.inspector
            .audio_trigger_section_mut()
            .set_collapsed(settings.audio_triggers_collapsed);
    }

    /// Build all panels. Call once after creation and after resize.
    ///
    /// Every main region builds into its own arena, in `MainRegion` order
    /// (UI_INVALIDATION_DESIGN.md D7), under a region root that clips it and
    /// carries its stacking tier (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D2).
    /// A region rebuilds alone through [`build_region`](Self::build_region)
    /// after its arena is cleared.
    pub fn build(&mut self) {
        record_work(WorkKind::FullBuild);
        self.tree.clear();
        // Invalidate input state — old node IDs are now stale
        self.input.invalidate_hover();

        self.layout.resize(self.screen_width, self.screen_height);
        for region in MainRegion::ALL {
            self.build_region(region);
        }
        self.built = true;
    }

    /// Whether the window has been built once.
    pub(crate) fn is_built(&self) -> bool {
        self.built
    }

    /// Build one region into its arena, which the caller has cleared (D7).
    /// Each panel's own build code is unchanged: it mints its subtree with
    /// `None` parents inside the region's bracket, and `end_region` sweeps it
    /// under the region root.
    pub(crate) fn build_region(&mut self, region: MainRegion) {
        let id = region.id();
        match region {
            // Chrome tier: the always-visible frame, painted over Base.
            MainRegion::Transport => {
                let token = self.tree.begin_region_in(id, self.layout.transport_bar(), ZTier::Chrome, "transport", UIFlags::empty());
                let start = self.tree.count();
                self.transport.build(&mut self.tree, &self.layout);
                self.tree.end_region(token, start);
            }
            MainRegion::Header => {
                let token = self.tree.begin_region_in(id, self.layout.header(), ZTier::Chrome, "header", UIFlags::empty());
                let start = self.tree.count();
                self.header.build(&mut self.tree, &self.layout);
                self.tree.end_region(token, start);
            }
            MainRegion::Footer => {
                let token = self.tree.begin_region_in(id, self.layout.footer(), ZTier::Chrome, "footer", UIFlags::empty());
                let start = self.tree.count();
                self.footer.build(&mut self.tree, &self.layout);
                self.tree.end_region(token, start);
            }
            MainRegion::Inspector => {
                let token = self.tree.begin_region_in(id, self.layout.inspector(), ZTier::Base, "inspector", UIFlags::empty());
                let start = self.tree.count();
                self.inspector.build(&mut self.tree, &self.layout);
                // The Scene tab's content is the scene panel, built into the
                // column area below the inspector's tab strip (SCENE_SURFACES D16).
                let scene_rect = self.inspector.scene_content_rect();
                if scene_rect.width > 0.0 {
                    self.scene_setup_panel.build_docked(&mut self.tree, scene_rect);
                }
                self.tree.end_region(token, start);
                // Its resize handle, drawn just inside its left edge, moves with
                // it. The preview is an opaque GPU blit over the atlas up to
                // `insp.x`, so a straddling handle would lose its left half;
                // the hit test (`is_near_inspector_edge`) stays centred on the
                // seam.
                let insp = self.layout.inspector();
                self.inspector_handle_id = Some(self.build_resize_handle(region, insp.x, insp.y, 4.0, insp.height));
            }
            // Audio Setup dock (D1) — a Base-tier column between the content
            // area and the inspector. Guard on the WIDTH (the geometry), not
            // the panel's `open` flag, so an open-without-width state can never
            // build a degenerate zero-rect dock.
            MainRegion::AudioDock => {
                if self.layout.audio_setup_width > 0.0 {
                    let dock = self.layout.audio_setup();
                    let token = self.tree.begin_region_in(id, dock, ZTier::Base, "audio_setup", UIFlags::empty());
                    let start = self.tree.count();
                    self.audio_setup_panel.build_docked(&mut self.tree, dock);
                    self.tree.end_region(token, start);
                    self.audio_setup_handle_id = Some(self.build_resize_handle(region, dock.x, dock.y, 4.0, dock.height));
                } else {
                    self.audio_setup_handle_id = None;
                }
            }
            MainRegion::SplitHandles => self.build_split_handles(),
            MainRegion::LayerHeaders => {
                self.viewport.prepare_layout(&self.layout);
                if let Some((target, param)) = self.pending_automation_reveal.as_ref()
                    && self.viewport.has_visible_automation_lane(target, param)
                {
                    self.viewport.reveal_automation_lane(target, param);
                    self.pending_automation_reveal = None;
                }
                let token =
                    self.tree.begin_region_in(id, self.layout.layer_controls(), ZTier::Base, "layer_headers", UIFlags::empty());
                let start = self.tree.count();
                self.layer_headers.build(
                    &mut self.tree,
                    &self.layout,
                    self.viewport.mapper(),
                    self.viewport.scroll_y_px(),
                );
                self.tree.end_region(token, start);
            }
            MainRegion::Viewport => {
                let token = self.tree.begin_region_in(id, self.layout.timeline_body(), ZTier::Base, "viewport", UIFlags::empty());
                let start = self.tree.count();
                self.viewport.build(&mut self.tree, &self.layout);
                self.tree.end_region(token, start);
            }
            // Every top-level overlay (perf HUD, dropdown, modals, toast) via
            // the single overlay driver, then the load-time warmup overlay.
            MainRegion::Overlays => {
                self.build_overlays();
                self.build_warmup_overlay();
            }
            MainRegion::Ghosts => self.inspector.rebuild_card_drag_overlay(&mut self.tree),
        }
    }

    /// The split handle between video and timeline: the SplitHandles region
    /// holds it alone, so a split drag translates it (D7). Chrome tier:
    /// utility chrome that should never be occluded.
    fn build_split_handles(&mut self) {
        let r = self.layout.split_handle();
        self.split_handle_id = Some(self.build_resize_handle(MainRegion::SplitHandles, r.x, r.y, r.width, r.height));
    }

    /// One thin resize affordance as a Chrome-tier sub-region of `region`'s
    /// arena, so it moves and rebuilds with the panel edge it sits on.
    fn build_resize_handle(&mut self, region: MainRegion, x: f32, y: f32, w: f32, h: f32) -> NodeId {
        let rect = Rect::new(x, y, w, h);
        let token = self.tree.begin_region_in(region.id(), rect, ZTier::Chrome, "resize_handle", UIFlags::empty());
        let start = self.tree.count();
        let id = self.tree.add_panel(
            None,
            x,
            y,
            w,
            h,
            manifold_ui::node::UIStyle {
                bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                ..manifold_ui::node::UIStyle::default()
            },
        );
        self.tree.end_region(token, start);
        id
    }

    /// Load-time warmup overlay: centered progress bar + layer label, drawn
    /// above the panels while the content thread warms. A Chrome-tier
    /// sub-region of the Overlays region.
    fn build_warmup_overlay(&mut self) {
        if self.export_progress.is_open() {
            return;
        }
        let Some(ref warmup) = self.warmup else {
            return;
        };
        use manifold_ui::chrome::{materialize, Align, Pad, Sizing, View};
        use manifold_ui::color;
        use manifold_ui::node::Color32;

        const OVERLAY_W: f32 = 360.0;
        const OVERLAY_H: f32 = 80.0;
        const BAR_W: f32 = 280.0;
        const BAR_H: f32 = 10.0;
        const BAR_RADIUS: f32 = 4.0;

        let fill_w = (BAR_W - 2.0) * warmup.fraction();
        let track = View::panel()
            .fixed(BAR_W, BAR_H)
            .bg(color::SLIDER_TRACK_PRESSED_C32)
            .radius(BAR_RADIUS)
            .pad(Pad::all(1.0))
            .child(
                View::panel()
                    .w(Sizing::Fixed(fill_w))
                    .fill_h()
                    .bg(Color32::new(64, 184, 82, 255))
                    .radius(2.0),
            );
        let label = View::label(format!(
            "Warming project — layer {} of {}\n{}",
            warmup.done, warmup.total, warmup.label
        ))
        .font(color::FONT_LABEL)
        .text_color(color::TEXT_PRIMARY_C32)
        .align_text(manifold_ui::node::TextAlign::Center);
        let overlay = View::stack()
            .fixed(OVERLAY_W, OVERLAY_H)
            .main_align(Align::Center)
            .cross_align(Align::Center)
            .child(label)
            .child(track);

        let full = Rect::new(0.0, 0.0, self.layout.screen_width, self.layout.screen_height);
        let token = self.tree.begin_region_in(
            MainRegion::Overlays.id(),
            full,
            ZTier::Chrome,
            "warmup_overlay",
            UIFlags::empty(),
        );
        let start = self.tree.count();
        let _ = materialize(&mut self.tree, &overlay, full);
        self.tree.end_region(token, start);
    }

    /// Build just the inspector column into `region`'s arena at an explicit
    /// `rect`, which the caller has cleared. The graph-editor window hosts the
    /// same inspector column in its card lane this way
    /// (docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md). The card-drag ghost is
    /// the Ghosts arena's, settled by the window's frame.
    pub fn build_inspector_in(&mut self, region: u8, rect: Rect) {
        let token = self.tree.begin_region_in(region, rect, ZTier::Base, "inspector_in_rect", UIFlags::empty());
        let start = self.tree.count();
        self.inspector.build_in_rect(&mut self.tree, rect);
        self.tree.end_region(token, start);
    }

    /// [`build_inspector_in`](Self::build_inspector_in) into the arena
    /// outside any region, for tests that build a bare column.
    #[cfg(test)]
    pub fn build_inspector_in_rect(&mut self, rect: Rect) {
        self.build_inspector_in(manifold_ui::tree::DEFAULT_REGION, rect);
        self.inspector.rebuild_card_drag_overlay(&mut self.tree);
    }

    /// Route pre-drained events through the inspector subset of the main
    /// event path: overlay (dropdown) routing, node-intent dispatch,
    /// `inspector.handle_event`, the slider/card-drag loop, and the
    /// dropdown-open pass. Used by the graph-editor window, which hosts this
    /// window's inspector column in its right lane but doesn't run the full
    /// `process_events` (it has no timeline/transport panels). Shares the
    /// exact routing logic with `process_events`; kept focused so the editor
    /// doesn't drag in main-window-only handling (viewport, layer headers,
    /// ⌘⇧A). Returns the inspector's `PanelAction`s for the caller to dispatch.
    pub fn route_inspector_events(&mut self, events: &[UIEvent]) -> Vec<PanelAction> {
        // Refresh node-intent dispatch on structural change (the editor rebuilds
        // its tree each present, so this repopulates each frame that has events).
        let sv = self.tree.structure_version();
        if sv != self.intents_structure_version {
            self.repopulate_intents();
            self.intents_structure_version = sv;
        }

        let mut actions = Vec::new();
        // Drain keyboard-driven picker actions (arrow/Enter nav) stashed by
        // `window_input.rs`'s text-input-active branch — see
        // `pending_keyboard_actions`'s doc comment. Node-mode picks in this
        // window route through `GraphCanvas::request_add_node_at` instead
        // (a `GraphEditCommand`, not a `PanelAction`), so this is normally
        // empty here; kept for parity with `process_events` in case a future
        // editor overlay picks a `PanelAction` this way.
        actions.append(&mut self.pending_keyboard_actions);
        let mut last_click_node: Option<NodeId> = None;
        for event in events {
            if let UIEvent::Click { node_id, .. } = event {
                last_click_node = Some(*node_id);
            }
            if let UIEvent::RightClick { pos, .. } = event {
                self.last_right_click_pos = *pos;
            }
            // Open overlays (dropdowns opened from the inspector, e.g. blend mode)
            // get first crack, then node-intent dispatch, then the inspector.
            if self.route_overlay_event(event, &mut actions) {
                self.drain_overlay_selections(&mut actions);
                continue;
            }
            if let Some(resolved) = self.resolve_intent(event) {
                actions.extend(resolved);
                continue;
            }
            let mut panel_actions = self.inspector.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);
        }

        // Slider / param drags + effect-card reorder (needs &mut tree). Mirrors
        // the drag loop in `process_events`, inspector-only.
        for event in events {
            match event {
                UIEvent::DragBegin { node_id, .. } => {
                    self.inspector.try_begin_card_drag(*node_id, &mut self.tree);
                }
                UIEvent::Drag { pos, modifiers, .. } => {
                    if self.inspector.is_card_drag_active() {
                        self.inspector.update_card_drag(*pos, &mut self.tree);
                    } else if self.inspector.has_pressed_target() {
                        let mut drag_actions =
                            self.inspector.handle_drag(*pos, &mut self.tree, modifiers.shift);
                        actions.append(&mut drag_actions);
                    }
                }
                UIEvent::DragEnd { pos, .. } | UIEvent::PointerUp { pos, .. } => {
                    if self.inspector.is_card_drag_active() {
                        self.inspector.update_card_drag(*pos, &mut self.tree);
                        let mut reorder = self.inspector.end_card_drag(&mut self.tree);
                        actions.append(&mut reorder);
                    } else if self.inspector.has_pressed_target() {
                        let mut end = self.inspector.handle_drag_end(&mut self.tree);
                        actions.append(&mut end);
                    }
                }
                _ => {}
            }
        }

        // Intercept dropdown / context-menu / picker triggers and open them here
        // (same as `process_events`); the rest flow back for dispatch.
        let mut filtered = Vec::with_capacity(actions.len());
        for action in actions {
            if self.try_open_dropdown(&action, last_click_node) {
                continue;
            }
            filtered.push(action);
        }
        filtered
    }

    /// Scroll the inspector in place (offset its content nodes) instead of
    /// rebuilding the whole tree. Returns true if handled in place; false means
    /// nothing is built yet and the caller should fall back to a full rebuild.
    /// Kept on `UIRoot` so the disjoint `inspector` + `tree` field borrows stay
    /// internal.
    pub fn try_inspector_scroll(&mut self, delta: f32, cursor_x: f32) -> bool {
        self.inspector
            .try_scroll_in_place(delta, cursor_x, &mut self.tree)
    }

    /// Apply a scroll, zoom or relayout to the layer headers and viewport in
    /// place: ruler, markers and export markers on a horizontal move, a zoom
    /// or a marker drag, track and header rows on a vertical one, everything on a
    /// relayout (a new rect or new track heights). A panel in `rebuilding`
    /// is left to its rebuild. Returns false when a count no longer matches,
    /// and the caller rebuilds
    /// [`scroll_rebuild_regions`](Self::scroll_rebuild_regions).
    pub fn scroll_in_place(
        &mut self,
        dirty: crate::frame_plan::ViewportMoves,
        rebuilding: crate::frame_plan::RegionMask,
    ) -> bool {
        if !self.built || !dirty.any() {
            return false;
        }
        if dirty.relayout {
            let ok = (rebuilding.contains(MainRegion::Viewport)
                || self.viewport.try_relayout(&mut self.tree, &self.layout))
                && (rebuilding.contains(MainRegion::LayerHeaders)
                    || self.layer_headers.try_relayout(
                        &mut self.tree,
                        &self.layout,
                        self.viewport.mapper(),
                        self.viewport.scroll_y_px(),
                    ));
            if ok {
                record_work(WorkKind::InPlaceUpdate);
                if !rebuilding.contains(MainRegion::Viewport) {
                    self.tree.set_region_rect(MainRegion::Viewport.id(), self.layout.timeline_body());
                }
                if !rebuilding.contains(MainRegion::LayerHeaders) {
                    self.tree.set_region_rect(MainRegion::LayerHeaders.id(), self.layout.layer_controls());
                }
            }
            return ok;
        }
        let mut ok = true;
        if dirty.scroll_x || dirty.zoom || dirty.markers {
            ok = self.viewport.try_update_horizontal_scroll(&mut self.tree);
        }
        if ok && dirty.scroll_y {
            let scroll_y_px = self.viewport.scroll_y_px();
            ok = self.layer_headers.try_update_vertical_scroll(&mut self.tree, &self.layout, scroll_y_px)
                && self.viewport.try_update_vertical_scroll(&mut self.tree);
        }
        if ok {
            record_work(WorkKind::InPlaceUpdate);
        }
        ok
    }

    /// The regions a scroll or zoom rebuilds when it cannot update in place:
    /// the layer headers when the vertical scroll or the layout moved, and the
    /// viewport. Overlays key on their own version.
    pub(crate) fn scroll_rebuild_regions(dirty: crate::frame_plan::ViewportMoves) -> crate::frame_plan::RegionMask {
        let mut mask = crate::frame_plan::RegionMask::NONE;
        if dirty.scroll_y || dirty.relayout {
            mask.insert(MainRegion::LayerHeaders);
        }
        mask.insert(MainRegion::Viewport);
        mask
    }

    // ── Overlay driver ──────────────────────────────────────────────
    // One enumeration of the top-level overlays for build, draw, and input.
    // The exhaustive `overlay_mut` match is what makes "built but never drawn"
    // unrepresentable. See docs/OVERLAY_SYSTEM_DESIGN.md.

    /// Take a new window size. The layout follows at once; the region rects
    /// it moves are frame-plan inputs, so the planner rebuilds what changed
    /// (D7, INV-5).
    pub fn set_screen_size(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
        self.dropdown.set_screen_size(width, height);
        self.layout.resize(width, height);
    }

    /// Size the window and build it whole: the fixture path of tests and the
    /// headless harness, which build outside a frame plan.
    #[cfg(any(test, feature = "ui-snapshot"))]
    pub fn resize(&mut self, width: f32, height: f32) {
        self.set_screen_size(width, height);
        self.build();
    }

    /// Process a pointer event from winit.
    pub fn pointer_event(&mut self, pos: Vec2, action: PointerAction, time: f32) {
        self.time_accumulator = time;
        self.input
            .process_pointer(&mut self.tree, pos, action, time);

        // On cursor move, perform continuous clip hit-testing in the viewport.
        // HoverEnter/HoverExit only fire on node-level transitions; they cannot
        // detect hover changes within the same node's bounding box (e.g., moving
        // between clips in the same track background). update_hover_at fills that gap.
        // With a modal popup open, the background is blocked — the viewport stays
        // where it was so clips behind the popup don't light up.
        if action == PointerAction::Move && !self.background_input_blocked() {
            let mut hover_actions = self.viewport.update_hover_at(pos);
            self.cursor_hover_actions.append(&mut hover_actions);
            // Waveform/stem button hover is handled by UITree node hover_bg_color.
        }
    }

    /// Process a right-click from winit.
    pub fn right_click(&mut self, pos: Vec2) {
        self.input.process_right_click(&self.tree, pos);
    }

    /// Process a key event from winit.
    pub fn key_event(&mut self, key: Key, modifiers: Modifiers) {
        self.input.process_key(&self.tree, key, modifiers);
    }


    /// Cache the candidate target layers for the audio-clip detection layer
    /// dropdowns. Set by `state_sync` when an audio clip is selected; read when
    /// an instrument's layer dropdown opens.
    pub fn set_clip_detect_layers(
        &mut self,
        layers: Vec<(manifold_core::LayerId, String)>,
    ) {
        self.clip_detect_layers = layers;
    }








    // dropdown_to_action removed (2b.11): every selectable dropdown item now
    // carries its own PanelAction via DropdownItem::with_action and fires
    // DropdownAction::SelectedAction directly. The only surviving DropdownContext
    // is LayerContext (its color swatches, handled below), which doesn't map a
    // positional text Selected(index) either.


    // ── Inspector resize ──────────────────────────────────────────

    const RESIZE_EDGE_PX: f32 = manifold_ui::color::RESIZE_EDGE_PX;
    const INSPECTOR_MIN_W: f32 = manifold_ui::color::MIN_INSPECTOR_WIDTH;
    const INSPECTOR_MAX_W: f32 = manifold_ui::color::MAX_INSPECTOR_WIDTH;

    /// Returns true if pos is near the inspector's LEFT edge (resize handle).
    pub fn is_near_inspector_edge(&self, pos: Vec2) -> bool {
        let insp = self.layout.inspector();
        (pos.x - insp.x).abs() < Self::RESIZE_EDGE_PX
            && pos.y >= insp.y
            && pos.y <= insp.y + insp.height
    }

    /// Begin an inspector resize drag.
    pub fn begin_inspector_resize(&mut self, x: f32) {
        self.inspector_resize_dragging = true;
        self.inspector_drag_start_x = x;
        self.inspector_drag_start_width = self.layout.inspector_width;
    }

    /// Update inspector width during resize drag. Returns true if width changed.
    pub fn update_inspector_resize(&mut self, x: f32) -> bool {
        if !self.inspector_resize_dragging {
            return false;
        }
        // Inspector is anchored to the right edge, so dragging its left handle
        // LEFT (negative delta) widens it.
        let delta = x - self.inspector_drag_start_x;
        let new_width = (self.inspector_drag_start_width - delta)
            .clamp(Self::INSPECTOR_MIN_W, Self::INSPECTOR_MAX_W);
        if (new_width - self.layout.inspector_width).abs() > 1.0 {
            self.layout.inspector_width = new_width;
            true
        } else {
            false
        }
    }

    /// End inspector resize drag.
    pub fn end_inspector_resize(&mut self) {
        self.inspector_resize_dragging = false;
    }

    // ── Audio Setup dock resize (D1) — mirror of the inspector pair ──
    const AUDIO_SETUP_MIN_W: f32 = manifold_ui::color::MIN_AUDIO_SETUP_WIDTH;
    const AUDIO_SETUP_MAX_W: f32 = manifold_ui::color::MAX_AUDIO_SETUP_WIDTH;

    /// True if `pos` is near the Audio Setup dock's LEFT edge (its resize
    /// handle). False when the dock is closed (zero width).
    pub fn is_near_audio_setup_edge(&self, pos: Vec2) -> bool {
        if self.layout.audio_setup_width <= 0.0 {
            return false;
        }
        let dock = self.layout.audio_setup();
        (pos.x - dock.x).abs() < Self::RESIZE_EDGE_PX && pos.y >= dock.y && pos.y <= dock.y + dock.height
    }

    /// Begin an Audio Setup dock resize drag.
    pub fn begin_audio_setup_resize(&mut self, x: f32) {
        self.audio_setup_resize_dragging = true;
        self.audio_setup_drag_start_x = x;
        self.audio_setup_drag_start_width = self.layout.audio_setup_width;
    }

    /// Update dock width during resize. The dock expands LEFTWARD, so dragging
    /// the handle left (negative delta) widens it. Returns true if width moved.
    pub fn update_audio_setup_resize(&mut self, x: f32) -> bool {
        if !self.audio_setup_resize_dragging {
            return false;
        }
        let delta = x - self.audio_setup_drag_start_x;
        let new_width = (self.audio_setup_drag_start_width - delta)
            .clamp(Self::AUDIO_SETUP_MIN_W, Self::AUDIO_SETUP_MAX_W);
        if (new_width - self.layout.audio_setup_width).abs() > 1.0 {
            self.layout.audio_setup_width = new_width;
            true
        } else {
            false
        }
    }

    /// End Audio Setup dock resize drag.
    pub fn end_audio_setup_resize(&mut self) {
        self.audio_setup_resize_dragging = false;
    }

    /// Toggle the Audio Setup dock (D1). The panel's `open` flag and the
    /// layout's `audio_setup_width` are the two halves of "docked" — `open`
    /// gates build/update, the width is the geometry `content_area()`
    /// subtracts. Keep them in lockstep: set the width from the NEW open state
    /// so this is a true toggle regardless of entry state. Both the live app
    /// (`app_render`'s `OpenAudioSetup` arm) and the headless script harness
    /// (`ui_bridge::dispatch`'s arm) call this ONE method so the toggle is
    /// reachable on both paths; the caller schedules the structural rebuild.
    pub fn toggle_audio_dock(&mut self) {
        self.audio_setup_panel.toggle();
        let open = self.audio_setup_panel.is_open();
        self.layout.audio_setup_width = if open {
            manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH
        } else {
            0.0
        };
        self.header.set_audio_dock_open(open);
    }

    /// Audio Setup dock resize-handle colour feedback (idle/hover/drag).
    pub fn set_audio_setup_handle_hover(&mut self) {
        self.set_handle_color(self.audio_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_HOVER);
    }
    pub fn set_audio_setup_handle_drag(&mut self) {
        self.set_handle_color(self.audio_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_DRAG);
    }
    pub fn set_audio_setup_handle_idle(&mut self) {
        self.set_handle_color(self.audio_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_IDLE);
    }

    fn set_handle_color(&mut self, id: Option<NodeId>, color: manifold_ui::node::Color32) {
        if let Some(id) = id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle { bg_color: color, ..manifold_ui::node::UIStyle::default() },
            );
        }
    }

    // ── Split handle color feedback ─────────────────────────────

    /// Update split handle color to hover state.
    pub fn set_split_handle_hover(&mut self) {
        if let Some(id) = self.split_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_HOVER,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    /// Update split handle color to drag state.
    pub fn set_split_handle_drag(&mut self) {
        if let Some(id) = self.split_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_DRAG,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    /// Update split handle color to idle state.
    pub fn set_split_handle_idle(&mut self) {
        if let Some(id) = self.split_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_hover(&mut self) {
        if let Some(id) = self.inspector_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_HOVER,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_drag(&mut self) {
        if let Some(id) = self.inspector_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_DRAG,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_idle(&mut self) {
        if let Some(id) = self.inspector_handle_id {
            self.tree.set_style(
                id,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }





    /// Advance the inspector's per-frame motion (drawer-height tweens,
    /// value-flash, dying-card collapse, tab-ink slide — everything
    /// `InspectorCompositePanel::update` drives). Extracted
    /// (`GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4, D4) so the graph
    /// editor's `UIRoot` — which never sets `built` and so never runs the
    /// rest of `update()` — can still tick its own inspector instance every
    /// frame it presents. Calling this directly bypasses the `!self.built`
    /// early return below by design; do not fold this into a full
    /// `update()` call on the editor root (that flag also gates
    /// main-window-only structure this instance doesn't have).
    pub fn tick_inspector(&mut self) {
        self.inspector.update(&mut self.tree);
    }

    /// Push values for the editor window's perf HUD. The editor root never
    /// runs the main-window `update()` path, so this targeted tick keeps the
    /// shared overlay live without touching main-window-only panels.
    pub fn tick_editor_perf_hud(&mut self) {
        self.perf_hud.push_values(&mut self.tree);
    }

    /// Lay the three chrome bars out at the values they hold, in place
    /// (section 4 region table: the bars' in-place column). The frame plan
    /// runs it only when a shown value moved, never per frame. Returns the
    /// bars whose structure changed (a resize to a new shape); the caller
    /// rebuilds those.
    #[must_use]
    pub fn lay_out_chrome(&mut self) -> crate::frame_plan::RegionMask {
        use manifold_ui::chrome::Reconcile;
        let mut stale = crate::frame_plan::RegionMask::NONE;
        for (region, reconcile) in [
            (MainRegion::Transport, self.transport.relayout(&mut self.tree)),
            (MainRegion::Header, self.header.relayout(&mut self.tree)),
            (MainRegion::Footer, self.footer.relayout(&mut self.tree)),
        ] {
            if reconcile == Reconcile::NeedsRebuild {
                stale.insert(region);
            }
        }
        stale
    }

    /// Per-frame update — push state changes to panels.
    pub fn update(&mut self) {
        if !self.built {
            return;
        }
        // P2 "panel-split snap-back" (D15): advance the two splits'
        // double-click-reset tweens. `min(100.0)` matches
        // `InspectorCompositePanel::update`'s own dt clamp (a stall/debugger
        // pause must not fling the tween in one giant step). The eased
        // ratio/width moves the region rects, so the next plan re-lays out
        // from them.
        let split_dt_ms = (self.layout_tick_last.elapsed().as_secs_f32() * 1000.0).min(100.0);
        self.layout_tick_last = std::time::Instant::now();
        self.layout.tick_splits(split_dt_ms);
        self.layer_headers.update(&mut self.tree);
        self.tick_inspector();
        self.scene_setup_panel.tick_object_cards(&mut self.tree, split_dt_ms);
        self.viewport.update(&mut self.tree);
        self.perf_hud.update(&mut self.tree);
        // D11 toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2): repaints its own alpha
        // in place while showing; a no-op once idle. Runs every frame like the
        // panels above it, not gated on anything overlay-specific.
        self.toast.update(&mut self.tree);
        // D17 "modal/dropdown enter": a no-op once settled or closed (see
        // `DropdownPanel::update`'s own guard) — cheap to call unconditionally.
        self.dropdown.update(&mut self.tree);
        // Same D17 enter, mirrored to the other three popups (P2 batch 2 —
        // `UI_CRAFT_AND_MOTION_PLAN.md` section 5 item 4: "universal popup enter").
        self.ableton_picker.update(&mut self.tree);
        self.settings_popup.update(&mut self.tree);
        self.rt_quality_panel.update(&mut self.tree);
    }

    /// Resize the Audio Setup level meters from live per-send levels. Cheap
    /// in-place node updates each frame while the modal is open — no rebuild.
    pub fn update_audio_meters(&mut self, levels: &[f32]) {
        if !self.audio_setup_panel.is_open() {
            return;
        }
        self.audio_setup_panel.update_meters(&mut self.tree, levels);
    }

    /// D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    /// P3c, BUG-082's fix): push this tick's live shaped-signal levels onto
    /// every open fire-mode drawer's Amount meter in the inspector — in
    /// place, every UI tick, unconditional (unlike `update_audio_meters`
    /// above, this isn't gated on any panel being open — the inspector's own
    /// drawers decide what's visible). `fire_meters` is
    /// `ContentState::fire_meters`; the closure adapts it to the plain
    /// `Fn(u64) -> Option<f32>` `manifold-ui` can call without depending on
    /// `manifold-core` (`docs/UI_LAYERING_INVERSION.md`). `dt` (BUG-109 P5)
    /// is the UI frame delta seconds, for each meter's peak-hold timing.
    pub fn update_fire_meters(
        &mut self,
        fire_meters: &manifold_core::audio_trigger::FireMeterCapture,
        dt: f32,
    ) {
        self.inspector.update_fire_meters(&mut self.tree, &|key| fire_meters.get(key), dt);
        self.scene_setup_panel.update_object_card_fire_meters(&mut self.tree, &|key| fire_meters.get(key), dt);
    }

    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` section 7.2 item 5):
    /// the send whichever fire-mode drawer is currently open in the inspector
    /// is reading, if any. `manifold_core::AudioSendId` is
    /// `manifold_foundation::AudioSendId` re-exported at its historical path
    /// (`docs/UI_LAYERING_INVERSION.md`), so this crosses the boundary for
    /// free.
    pub fn open_fire_mode_drawer_send(&self) -> Option<manifold_core::AudioSendId> {
        self.inspector.open_fire_mode_drawer_send()
    }

    /// The band whichever fire-mode drawer is currently open in the
    /// inspector is reading, if any — pairs with
    /// [`Self::open_fire_mode_drawer_send`] (both read off the same open row).
    pub fn open_fire_mode_drawer_band(&self) -> Option<manifold_ui::types::AudioBand> {
        self.inspector.open_fire_mode_drawer_band()
    }

    /// Update the audio scope's hover readout (freq + dB under the cursor), or
    /// hide it when not hovering. In place, every frame — see `update_meters`.
    pub fn update_audio_scope_readout(&mut self, text: Option<&str>) {
        if !self.audio_setup_panel.is_open() {
            return;
        }
        self.audio_setup_panel.update_scope_readout(&mut self.tree, text);
    }

    /// Push the current crossovers (Hz) + the scope's analysed frequency range to
    /// the Audio Setup panel each frame, so it can hit-test the band-divider
    /// lines for dragging. See `AudioSetupPanel::set_scope_bands`.
    pub fn update_audio_scope_bands(&mut self, low_hz: f32, mid_hz: f32, fmin: f32, fmax: f32) {
        if !self.audio_setup_panel.is_open() {
            return;
        }
        self.audio_setup_panel
            .set_scope_bands(low_hz, mid_hz, fmin, fmax);
    }

    /// Whether a band divider is currently being dragged in the Audio Setup
    /// scope — the app suppresses the hover readout during the drag.
    pub fn audio_band_dragging(&self) -> bool {
        self.audio_setup_panel.is_dragging_band()
    }

    /// Position + fill the scope's per-band level meters from the tapped send's
    /// `[low, mid, high]` amplitudes (0..1), or hide them when `None`. In place,
    /// every frame — see `update_audio_meters`.
    pub fn update_audio_band_meters(&mut self, amps: Option<[f32; 3]>) {
        if !self.audio_setup_panel.is_open() {
            return;
        }
        self.audio_setup_panel.update_band_meters(&mut self.tree, amps);
        self.audio_setup_panel.update_scope_lane_labels(&mut self.tree);
    }

}





/// BUG-058 trace (`MANIFOLD_INPUT_TRACE=1`): the discrete pointer transitions
/// worth a routing line. Never the per-frame Move/Drag stream — a trace run
/// stays readable at gesture granularity.
fn trace_worthy(event: &UIEvent) -> bool {
    matches!(
        event,
        UIEvent::PointerDown { .. }
            | UIEvent::PointerUp { .. }
            | UIEvent::DragBegin { .. }
            | UIEvent::DragEnd { .. }
    )
}

/// Short label for [`trace_worthy`] events in trace lines.
fn trace_kind(event: &UIEvent) -> &'static str {
    match event {
        UIEvent::PointerDown { .. } => "PointerDown",
        UIEvent::PointerUp { .. } => "PointerUp",
        UIEvent::DragBegin { .. } => "DragBegin",
        UIEvent::DragEnd { .. } => "DragEnd",
        _ => "other",
    }
}



#[cfg(test)]
mod region_structural_tests {
    //! `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D4, second enforcement leg,
    //! asserted against the REAL `UIRoot::build()` — the strongest available
    //! proof P1's migration is complete: not a synthetic fixture, the actual
    //! tree the app renders every frame. `mint`'s debug assertion (the first
    //! leg) is `cfg(not(test))`-gated inside `manifold-ui` itself, but
    //! `manifold-ui` is an ordinary (non-test-cfg) dependency from here, so
    //! it's live for every test in this module too — a regression would
    //! panic before either assertion below even runs.
    use super::*;

    const W: f32 = 1536.0;
    const H: f32 = 1216.0;

    /// The static build: transport/header/footer/inspector/split-handles/
    /// layer-headers/viewport, no overlay open. Every root must be a region.
    #[test]
    fn main_window_build_has_no_stray_roots() {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        assert!(ui.built, "resize() must have run build()");
        assert!(
            ui.tree.all_roots_are_regions(),
            "every top-level node in a freshly built main window must be a \
             region root (D1/D4) — found one that isn't"
        );
    }

    /// Same check with an overlay open (settings modal, with its dim scrim)
    /// and a scroll-triggered region rebuild in between — covers
    /// `build_overlays`'s per-overlay regions and `clear_region`'s
    /// sub-region pruning.
    #[test]
    fn main_window_build_with_overlay_and_scroll_rebuild_has_no_stray_roots() {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        ui.settings_popup.open();
        ui.build();
        assert!(
            ui.tree.all_roots_are_regions(),
            "a stray root survived with an overlay open"
        );

        // The scroll fallback rebuilds the layer headers and viewport alone.
        let mut signals = crate::ui_frame::UiFrameSignals {
            rebuild: UIRoot::scroll_rebuild_regions(crate::frame_plan::ViewportMoves {
                scroll_x: false,
                scroll_y: true,
                zoom: true,
                relayout: false,
                markers: false,
            }),
            ..Default::default()
        };
        crate::ui_frame::apply_ui_frame_invalidations(&mut ui, None, &mut signals);
        assert!(
            ui.tree.all_roots_are_regions(),
            "a stray root survived a scroll-triggered partial rebuild"
        );
    }

    /// The editor window's card lane build is likewise fully regioned.
    #[test]
    fn inspector_in_rect_has_no_stray_roots() {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        ui.build_inspector_in(crate::editor_frame::EditorRegion::CardLane.id(), Rect::new(0.0, 0.0, 400.0, H));
        assert!(
            ui.tree.all_roots_are_regions(),
            "the card lane build left a stray root"
        );
    }
}

/// `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 (BUG-160), D4's tick-
/// parity check: the graph editor's own `UIRoot` never sets `built`, so its
/// `update()` early-returns and — before `tick_inspector` was extracted and
/// wired into `present_graph_editor_window` — nothing ever advanced its
/// inspector's per-card motion (drawer-height tweens, collapse, value-flash).
/// A card's frame height flows through that same ticked state
/// (`compute_height` reads `collapse_frac()`, which `tick_drawers` advances),
/// so a never-ticked editor host would sit at a stale height while its rows
/// built at their true size — the mechanism the audit named for BUG-160's
/// overflow.
#[cfg(test)]
mod tick_parity_tests {
    use super::*;
    use manifold_ui::panels::param_card::CardContext;

    /// A minimal one-param effect card config, built from scratch (not
    /// reused from `manifold-ui`'s own private `mk_config` test helper,
    /// which isn't reachable across the crate boundary) — every field is
    /// `pub` on `ParamSurface`, so this fixture is a direct, honest
    /// construction of the same shape `ui_bridge::state_sync` builds from
    /// real project state.
    fn fixture_config(collapsed: bool) -> manifold_ui::ParamSurface {
        manifold_ui::ParamSurface {
            kind: manifold_ui::ParamCardKind::Effect,
            title: "Fixture".into(),
            rows: vec![manifold_ui::ParamRow {
                id: std::borrow::Cow::Borrowed("amount"),
                spec: manifold_ui::RowSpec {
                    tooltip: None,
                    name: "Amount".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    whole_numbers: false,
                    is_angle: false,
                    is_toggle: false,
                    is_trigger: false,
                    is_trigger_gate: false,
                    value_labels: None,
                    unit: None,
                    section: None,
                    material_role: None,
                    inactive_reason: None,
                    scene: None,
                    limit: Default::default(),
                    disabled: None,
                },
                value: manifold_ui::RowValue { base: 0.5, effective: 0.5, exposed: true, driven: false },
                modulation: manifold_ui::RowMod::default(),
                mapping: manifold_ui::RowMapping {
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                scene_addr: None,
                rgb_members: None,
                modulator_stored: false,
                audio: Default::default(),
                clip_trigger: None,
            }],
            string_params: Vec::new(),
            collapsed,
            effect_index: 0,
            effect_id: manifold_core::EffectId::new("bug160-fixture"),
            enabled: true,
            supports_envelopes: true,
            has_graph_mod: false,
            layer_id: None,
            modifier: None,
            audio_sends: Vec::new(),
        }
    }

    /// Drives `UIRoot::tick_inspector` directly on an un-`built` root (the
    /// exact state the editor's `UIRoot` is in permanently — `built` gates
    /// only main-window-only structure, per D4's forbidden-moves list, so
    /// the editor is never expected to flip it) with a live collapse tween —
    /// the same per-card `tick_drawers` rail `drawer_height_anim` rides, and
    /// the same rail this bug's card-frame height derives from
    /// (`compute_height` -> `collapse_frac` -> `collapse_anim`, ticked only
    /// by `tick_drawers`). Fails against the pre-fix code path (calling only
    /// `update()`, which this root's `!built` early-return makes a no-op) —
    /// this is the test that would have caught BUG-160's root mechanism.
    #[test]
    fn editor_tick_advances_inspector_motion() {
        let mut root = UIRoot::new();
        assert!(
            !root.built,
            "the editor UIRoot never sets built — that's D4's precondition"
        );

        root.inspector.set_card_context(CardContext::Author);
        // First configure: nothing to preserve yet, so `sync_collapse_anim`
        // snaps straight to the settled (expanded) height.
        root.inspector.configure_master_effects(&[fixture_config(false)]);
        let height_expanded = root
            .inspector
            .master_effect_mut(0)
            .expect("card configured")
            .compute_height();

        // Second configure with the SAME effect identity: an edit, not a
        // navigation, so the existing card is reused and `sync_collapse_anim`
        // eases from here (D4: identically in both contexts) rather than
        // snapping. The tween is armed but hasn't ticked yet — height is
        // still the expanded value.
        root.inspector.configure_master_effects(&[fixture_config(true)]);
        {
            let card = root.inspector.master_effect_mut(0).expect("card configured");
            assert!(
                card.is_collapse_animating(),
                "flipping collapsed on an already-configured card must arm a tween"
            );
            assert_eq!(
                card.compute_height(),
                height_expanded,
                "the tween is armed but not yet ticked — height must not have moved on its own"
            );
        }

        // Prime `motion_last_tick` (the very first tick after arming a tween
        // always sees dt=0 — no wall time has elapsed yet), then let real
        // time pass and tick again — mirroring the editor's per-present-frame
        // call in `present_graph_editor_window`.
        root.tick_inspector();
        std::thread::sleep(std::time::Duration::from_millis(60));
        root.tick_inspector();

        let height_after_tick = root
            .inspector
            .master_effect_mut(0)
            .expect("card configured")
            .compute_height();
        assert!(
            height_after_tick < height_expanded,
            "tick_inspector on an un-built root must advance the collapse tween \
             (height_after_tick={height_after_tick}, height_expanded={height_expanded}) — \
             this is the exact mechanism BUG-160's card-height overflow came from"
        );
    }

}
