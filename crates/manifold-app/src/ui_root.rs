//! UIRoot — owns the entire UI state for one window.
//!
//! Contains the UITree, UIInputSystem, ScreenLayout, all panels,
//! and the dropdown overlay. The app layer creates one UIRoot per
//! workspace window and forwards winit events through it.

use manifold_playback::ableton_bridge::AbletonSession;
use manifold_ui::input::{Key, Modifiers, PointerAction, UIEvent};
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::panels::overlay::{
    Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, compute_overlay_rect,
};
use manifold_ui::*;

/// The top-level overlays, in bottom→top z-order. The single registry the
/// overlay driver (build / draw / input) iterates — adding an overlay means a
/// field, an `overlay_mut` arm, and a `Z_ORDER` entry, and the exhaustive match
/// then forces the wiring. See `docs/OVERLAY_SYSTEM_DESIGN.md`.
///
/// `pub(crate)`: `text_input::TextSessionOwner` (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`
/// §3, D2) tags a text session with the id of the overlay hosting it, so the
/// crate needs to name overlays outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayId {
    PerfHud,
    Dropdown,
    // Audio Setup is no longer an overlay — it is a `ScreenLayout` dock column
    // built from the root pass and routed at the docked-panel site
    // (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D1/§3.5).
    Settings,
    BrowserPopup,
    AbletonPicker,
    Toast,
}

impl OverlayId {
    /// Bottom → top: later = higher z (drawn last / on top, first to receive
    /// input). The perf HUD sits at the bottom so a real modal always covers it.
    /// The dropdown sits on top: it's a transient selection surface opened *from*
    /// another overlay (e.g. the Audio Setup modal's device/channel pickers), so
    /// it must render above whatever spawned it. The toast (D11,
    /// `UI_CRAFT_AND_MOTION_PLAN.md` P2) sits topmost of all — a status message
    /// must stay legible over an open modal/dropdown, not be hidden by one.
    const Z_ORDER: [OverlayId; 6] = [
        OverlayId::PerfHud,
        OverlayId::Settings,
        OverlayId::BrowserPopup,
        OverlayId::AbletonPicker,
        OverlayId::Dropdown,
        OverlayId::Toast,
    ];
}

/// Who owns the in-flight pointer drag. Resolved once per gesture (D1) by
/// [`UIRoot::resolve_drag_owner`], cleared by the terminal broadcast (D2).
/// `docs/DRAG_CAPTURE_DESIGN.md` §3.1 — replaces the old boolean drag-active latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragOwner {
    /// An open overlay claimed it (modal, or origin inside its rect — §3.2).
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

/// Convert an AbletonSession into the picker's thin data struct.
pub(crate) fn build_picker_session(
    session: &AbletonSession,
) -> manifold_ui::panels::ableton_picker::AbletonPickerSession {
    use manifold_ui::panels::ableton_picker::{
        AbletonPickerSession, PickerDevice, PickerMacro, PickerTrack,
    };

    const RACK_CLASSES: &[&str] = &[
        "InstrumentGroupDevice",
        "DrumGroupDevice",
        "AudioEffectGroupDevice",
        "MidiEffectGroupDevice",
    ];

    let rack_tracks = session
        .tracks
        .iter()
        .filter_map(|track| {
            let devices: Vec<PickerDevice> = track
                .devices
                .iter()
                .filter(|d| RACK_CLASSES.contains(&d.class_name.as_str()) && !d.macros.is_empty())
                .map(|d| PickerDevice {
                    device_id: d.device_id,
                    device_name: d.name.clone(),
                    device_class_name: d.class_name.clone(),
                    macros: d
                        .macros
                        .iter()
                        .map(|m| PickerMacro {
                            param_id: m.param_id,
                            name: m.name.clone(),
                        })
                        .collect(),
                })
                .collect();
            if devices.is_empty() {
                None
            } else {
                Some(PickerTrack {
                    track_id: track.track_id,
                    track_name: track.name.clone(),
                    devices,
                })
            }
        })
        .collect();

    AbletonPickerSession { rack_tracks }
}

/// What the currently-open dropdown is selecting for.
#[derive(Debug, Clone)]
pub enum DropdownContext {
    // Most menus now use typed DropdownItem::with_action, needing no context /
    // index→action map (2b.11). Retired: BlendMode, MidiNote, MidiChannel,
    // MidiDevice, Resolution, ClkDevice, ClipContext, TrackContext, AudioInputDevice,
    // LayerAudioSend, ClipDetectQuantize, ClipDetectLayer, AudioTriggerLayer,
    // AudioSetupDevice, MasterExitPath, CardContext, ParamContext, MacroSlotContext,
    // GenStringParamDropdown, AudioSendRoutings (§7.2 item 7, P8, 2026-07-11 —
    // the Cap chip that opened it is gone, its content lives in the Inputs
    // section's read-only routing display now).
    LayerContext(manifold_core::LayerId), // survives only for its color swatches (text items are typed)
}

/// Fine-grained tracking of what scroll-related state changed.
/// Enables skipping expensive rebuilds when only horizontal scroll moved.
#[derive(Default, Clone, Copy)]
pub struct ScrollDirty {
    pub scroll_x: bool,
    pub scroll_y: bool,
    pub zoom: bool,
    /// Non-axis visual changes: hover, selection, overlay state.
    pub visual: bool,
}

impl ScrollDirty {
    pub fn any(&self) -> bool {
        self.scroll_x || self.scroll_y || self.zoom || self.visual
    }

    /// Layer headers only depend on scroll_y, zoom, or visual changes — not scroll_x.
    pub fn needs_layer_headers(&self) -> bool {
        self.scroll_y || self.zoom || self.visual
    }

    /// True if only scroll axes changed (no zoom, no visual) — enables update-in-place.
    pub fn is_scroll_only(&self) -> bool {
        (self.scroll_x || self.scroll_y) && !self.zoom && !self.visual
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
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

/// Owns all UI state for one window.
pub struct UIRoot {
    // Core
    pub tree: UITree,
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
    pub perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel,
    /// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2). Fired by
    /// `Application` on `M::Undo`/`M::Redo` (see `app_render.rs`); ticked every
    /// frame in `update()`.
    pub toast: manifold_ui::panels::toast::ToastPanel,
    /// D17 "export-complete green sweep" one-shot guard: `content_state` is a
    /// cached snapshot re-pushed every UI frame (`push_state` in
    /// `ui_bridge/state_sync.rs`), not an edge-triggered event, so without this
    /// the toast would re-fire on every frame the last-received snapshot still
    /// carries the same `ExportFinishedEvent` (it can outlive a single UI frame
    /// under load). Keyed on `(success, message, output_path)` — distinct
    /// enough that two different real exports are never conflated, cheap
    /// enough to just compare as a string. `None` = no export toast shown yet.
    pub last_export_toast_key: Option<String>,

    /// Same re-fire guard as `last_export_toast_key`, for the D11 undo/redo
    /// toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2). Keyed on
    /// `content_state.data_version` (undo/redo always bumps it, so each real
    /// undo/redo gets a distinct key even when the description repeats).
    pub last_undo_redo_toast_key: Option<u64>,

    /// Project-embedded ("forked") presets surfaced into the Add pickers, kept
    /// in sync with the content snapshot. Change-gated by
    /// `embedded_presets_fingerprint` so the Vec rebuilds only when the embedded
    /// set actually changes, not every frame.
    pub embedded_presets: Vec<EmbeddedPresetItem>,
    embedded_presets_fingerprint: u64,

    // State
    built: bool,
    screen_width: f32,
    screen_height: f32,
    time_accumulator: f32,
    /// Tree index where scroll-affected panels begin (layer_headers, viewport, perf_hud).
    /// Everything before this index is "static" (transport, header, footer, inspector)
    /// and preserved during scroll-only rebuilds via tree.truncate_from().
    scroll_panels_start: usize,
    /// Tree index where viewport panels begin (after layer_headers).
    /// On horizontal-only scroll, truncate from here to skip layer header rebuild.
    viewport_panels_start: usize,

    /// Context for the currently-open dropdown (set before open, read on selection).
    dropdown_context: Option<DropdownContext>,

    /// Detected display resolutions from winit monitors: (w, h, label).
    /// Set by Application after monitor enumeration.
    display_resolutions: Vec<(u32, u32, String)>,

    /// Cached master effect names for the LED exit path dropdown.
    /// Updated by state_sync when project changes.
    pub master_effect_names: Vec<String>,

    /// Cached MIDI clock device names for the CLK device dropdown.
    /// Updated from ContentState each frame.
    pub midi_device_names: Vec<String>,

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
    // `audio_trigger_layers` (the matrix's target-layer dropdown cache) is
    // deleted with the matrix (P3, D2).
    // `audio_layers` (Inputs section "+ Layer" dropdown candidates) deleted
    // with the section's authoring (§7.2 item 7, P8, 2026-07-11).

    // Inspector resize state
    pub inspector_resize_dragging: bool,
    inspector_drag_start_x: f32,
    inspector_drag_start_width: f32,

    // Audio Setup dock resize state (D1) — mirror of the inspector pair.
    pub audio_setup_resize_dragging: bool,
    audio_setup_drag_start_x: f32,
    audio_setup_drag_start_width: f32,
    pub scene_setup_resize_dragging: bool,
    scene_setup_drag_start_x: f32,
    scene_setup_drag_start_width: f32,

    /// Set when overlay state changes (popup open/close, scroll, category change).
    /// Consumed by app.rs to trigger rebuild_scroll_panels.
    pub overlay_dirty: bool,

    /// Effect clipboard count (set by app.rs, used by browser popup).
    pub effect_clipboard_count: usize,

    /// Generator clipboard for copy/paste between generator layers.
    pub gen_clipboard: manifold_editing::clipboard::GeneratorClipboard,

    /// Hover actions produced by continuous cursor movement, drained in process_events.
    cursor_hover_actions: Vec<PanelAction>,

    /// `PanelAction`s produced by keyboard-driven overlay picking outside the
    /// normal per-frame event queue (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`
    /// §4/§5 P2 arrow/Enter nav). An active `SearchFilter` text session
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

    /// Cached macro slot labels for context menu display.
    pub macro_labels: [String; manifold_core::MACRO_COUNT],
    /// Cached macro mapping descriptions per slot (for context menu).
    pub macro_mapping_descs: [Vec<String>; manifold_core::MACRO_COUNT],
    /// Whether each macro slot has an Ableton mapping (for "Remove" menu item).
    pub macro_ableton_mapped: [bool; manifold_core::MACRO_COUNT],

    /// Node ID for the video/timeline split handle (color feedback on hover/drag).
    /// From Unity PanelResizeHandle.cs — idle/hover/drag color states.
    split_handle_id: Option<NodeId>,
    /// Node ID for the inspector resize handle (vertical bar at inspector right edge).
    inspector_handle_id: Option<NodeId>,
    /// Node ID for the Audio Setup dock resize handle (vertical bar at the
    /// dock's LEFT edge). `None` while the dock is closed. (D1)
    audio_setup_handle_id: Option<NodeId>,
    /// Node ID for the Scene Setup dock resize handle — cloned from
    /// `audio_setup_handle_id` above (SCENE_SETUP_PANEL_DESIGN D2).
    scene_setup_handle_id: Option<NodeId>,
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

    /// Cached Ableton session for the picker popup.
    pub ableton_session: Option<std::sync::Arc<manifold_playback::ableton_bridge::AbletonSession>>,
    /// Two-column Ableton macro picker popup (replaces flat dropdown Ableton section).
    pub ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup,
    /// Which param triggered the picker — resolved when picker returns Selected.
    ableton_picker_context: Option<manifold_ui::panels::ableton_picker::AbletonPickerContext>,
    /// Set when the Ableton picker opens — drained by app_render to send
    /// `AbletonRediscover` on the content thread so the picker shows fresh data.
    pub ableton_rediscovery_needed: bool,

    /// Node ranges `[start, end)` of each open overlay, in z-order, recorded by
    /// `build_overlays`. The draw pass renders these at `Depth::OVERLAY` offset
    /// by stack index — so build and draw share one source and cannot drift
    /// (the bug class this system eliminates).
    pub overlay_draw: Vec<(usize, usize)>,
    /// On-screen rect of each OPEN overlay as of the last `build_overlays` —
    /// the same `rect` value passed to that overlay's `build_at`. Read by
    /// `overlay_contains_point` (D5, `docs/DRAG_CAPTURE_DESIGN.md` §2) so the
    /// window-seam press interceptors in `window_input.rs` can yield to a
    /// floating overlay instead of punching straight through it. Limitation:
    /// `Anchor::SelfManaged` overlays (dropdown, browser popup, Ableton
    /// picker) position themselves inside `build_at` from the raw screen
    /// size, so their entry here is a placeholder `(0,0,w,h)` rect, not their
    /// true footprint — none of the three are relevant to the BUG-059 case
    /// this guards (the docked Audio Setup panel), and the placeholder sits
    /// at the screen origin, away from where the seams live in practice.
    overlay_rects: Vec<(OverlayId, Rect)>,
    /// Tree index where the overlay region begins (after all scroll panels).
    /// The waveform/stem-lane overlay render uses this as its upper bound.
    pub overlay_region_start: usize,
    /// Open-state of every overlay (one bit per `OverlayId::Z_ORDER` slot) as of
    /// the last `build_overlays`. The driver compares this against the live
    /// open-set each frame ([`detect_overlay_open_change`]); any difference — an
    /// open OR a close, including programmatic `close()` paths that never route
    /// through the event-driven `overlay_dirty` flag — schedules a rebuild of the
    /// overlay region. This makes "the overlay region matches the open-set" an
    /// invariant the driver owns, so no individual close site has to remember to
    /// dirty the tree (the leaked-ghost-node bug class).
    ///
    /// [`detect_overlay_open_change`]: UIRoot::detect_overlay_open_change
    overlay_open_snapshot: u8,
    /// Overlays whose `is_open()` flipped false since the last drain —
    /// `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3, D2. Populated by
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
    pub fn new() -> Self {
        Self {
            // Give the build path real glyph-width measurement (size-to-content)
            // instead of the heuristic fallback. CoreTextMeasure is GPU-free, so
            // it installs here at construction; both windows' UIRoots get it.
            tree: {
                let mut tree = UITree::new();
                tree.set_text_measure(Box::new(
                    manifold_renderer::native_text::CoreTextMeasure::new(),
                ));
                tree
            },
            input: UIInputSystem::new(),
            layout: ScreenLayout::new(1280.0, 720.0),
            intents: manifold_ui::intent::IntentRegistry::new(),
            intents_structure_version: u64::MAX,
            transport: TransportPanel::new(),
            header: HeaderPanel::new(),
            footer: FooterPanel::new(),
            layer_headers: LayerHeaderPanel::new(),
            inspector: InspectorCompositePanel::new(),
            viewport: TimelineViewportPanel::new(),
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
            perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel::new(),
            toast: manifold_ui::panels::toast::ToastPanel::new(),
            last_export_toast_key: None,
            last_undo_redo_toast_key: None,
            embedded_presets: Vec::new(),
            embedded_presets_fingerprint: 0,
            built: false,
            screen_width: 1280.0,
            screen_height: 720.0,
            time_accumulator: 0.0,
            scroll_panels_start: 0,
            viewport_panels_start: 0,
            dropdown_context: None,
            display_resolutions: Vec::new(),
            master_effect_names: Vec::new(),
            midi_device_names: Vec::new(),
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
            scene_setup_resize_dragging: false,
            scene_setup_drag_start_x: 0.0,
            scene_setup_drag_start_width: 0.0,
            overlay_dirty: false,
            effect_clipboard_count: 0,
            gen_clipboard: manifold_editing::clipboard::GeneratorClipboard::new(),
            cursor_hover_actions: Vec::new(),
            pending_keyboard_actions: Vec::new(),
            viewport_events: Vec::new(),
            last_right_click_pos: Vec2::new(0.0, 0.0),
            macro_labels: std::array::from_fn(|_| String::new()),
            macro_mapping_descs: std::array::from_fn(|_| Vec::new()),
            macro_ableton_mapped: [false; manifold_core::MACRO_COUNT],
            split_handle_id: None,
            inspector_handle_id: None,
            audio_setup_handle_id: None,
            scene_setup_handle_id: None,
            layout_tick_last: std::time::Instant::now(),
            drag_owner: None,
            ableton_session: None,
            ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup::new(),
            ableton_picker_context: None,
            ableton_rediscovery_needed: false,
            overlay_draw: Vec::new(),
            overlay_rects: Vec::new(),
            overlay_region_start: 0,
            overlay_open_snapshot: 0,
            closed_overlays: smallvec::SmallVec::new(),
        }
    }

    /// Set detected display resolutions from winit monitors.
    pub fn set_display_resolutions(&mut self, resolutions: Vec<(u32, u32, String)>) {
        self.display_resolutions = resolutions;
    }

    /// Build panel cache info for UICacheManager.
    /// Returns one entry per cacheable panel with its node range and screen rect.
    pub fn panel_cache_info(&self) -> Vec<manifold_renderer::ui_cache_manager::PanelCacheInfo> {
        use manifold_renderer::ui_cache_manager::{PanelCacheInfo, PanelSlot};

        let mut info = Vec::with_capacity(7);

        // Transport
        let (start, end) = self.transport.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::Transport,
                node_start: start,
                node_end: end,
                rect: self.layout.transport_bar(),
                sub_regions: None,
            });
        }

        // Header
        let (start, end) = self.header.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::Header,
                node_start: start,
                node_end: end,
                rect: self.layout.header(),
                sub_regions: None,
            });
        }

        // Footer
        let (start, end) = self.footer.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::Footer,
                node_start: start,
                node_end: end,
                rect: self.layout.footer(),
                sub_regions: None,
            });
        }

        // Inspector (with per-card sub-regions for incremental re-rendering)
        let (start, end) = self.inspector.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::Inspector,
                node_start: start,
                node_end: end,
                rect: self.layout.inspector(),
                sub_regions: Some(self.inspector.sub_region_ranges()),
            });
        }

        // SplitHandles — nodes between inspector end and scroll_panels_start
        let inspector_end = self
            .inspector
            .first_node()
            .saturating_add(self.inspector.node_count());
        if inspector_end < self.scroll_panels_start && self.inspector.first_node() != usize::MAX {
            info.push(PanelCacheInfo {
                slot: PanelSlot::SplitHandles,
                node_start: inspector_end,
                node_end: self.scroll_panels_start,
                rect: Rect::new(
                    0.0,
                    0.0,
                    self.layout.screen_width,
                    self.layout.screen_height,
                ),
                sub_regions: None,
            });
        }

        // LayerHeaders
        let (start, end) = self.layer_headers.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::LayerHeaders,
                node_start: start,
                node_end: end,
                rect: self.layout.layer_controls(),
                sub_regions: None,
            });
        }

        // Viewport (timeline body)
        let (start, end) = self.viewport.node_range();
        if start < end {
            info.push(PanelCacheInfo {
                slot: PanelSlot::Viewport,
                node_start: start,
                node_end: end,
                rect: self.layout.timeline_body(),
                sub_regions: None,
            });
        }

        info
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
    }

    /// Build all panels. Call once after creation and after resize.
    ///
    /// `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D2: every top-level panel
    /// builds under a region (`begin_region` .. `end_region`) instead of
    /// rooting itself at the tree directly — the region mints `CLIPS_CHILDREN`
    /// by construction and carries a stacking tier, so a panel can never
    /// paint outside its own rect and Chrome always wins over Base
    /// regardless of build order. Each panel's own build code is UNCHANGED
    /// (still mints its own subtree with `None` parents internally); the
    /// wrap here mechanically sweeps that subtree under the region via
    /// `end_region`, the same idiom the inspector's own pre-existing
    /// `ClipRegion` already used before this design generalized it.
    pub fn build(&mut self) {
        self.tree.clear();
        // Invalidate input state — old node IDs are now stale
        self.input.invalidate_hover();

        self.layout.resize(self.screen_width, self.screen_height);

        // Static panels — preserved during scroll-only rebuilds.
        // Order: transport, header, footer, inspector (non-scroll-affected).
        // Chrome tier (transport/header/footer): the always-visible frame —
        // painted after (on top of) Base regardless of this build order.
        let region =
            self.tree
                .begin_region(self.layout.transport_bar(), ZTier::Chrome, "transport", UIFlags::empty());
        let start = self.tree.count();
        self.transport.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        let region = self.tree.begin_region(self.layout.header(), ZTier::Chrome, "header", UIFlags::empty());
        let start = self.tree.count();
        self.header.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        let region = self.tree.begin_region(self.layout.footer(), ZTier::Chrome, "footer", UIFlags::empty());
        let start = self.tree.count();
        self.footer.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        // Base tier: main content surfaces.
        let region = self.tree.begin_region(self.layout.inspector(), ZTier::Base, "inspector", UIFlags::empty());
        let start = self.tree.count();
        self.inspector.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        // Audio Setup dock (D1) — a Base-tier column between the content area
        // and the inspector, built from the root pass when open. It's no longer
        // an overlay; opening/closing changes `audio_setup_width`, which shrinks
        // the content area, so a full rebuild lands here at the new geometry.
        // Guard on the WIDTH (the geometry), not the panel's `open` flag, so an
        // open-without-width state can never build a degenerate zero-rect dock.
        if self.layout.audio_setup_width > 0.0 {
            let dock = self.layout.audio_setup();
            let region = self.tree.begin_region(dock, ZTier::Base, "audio_setup", UIFlags::empty());
            let start = self.tree.count();
            self.audio_setup_panel.build_docked(&mut self.tree, dock);
            self.tree.end_region(region, start);
        }

        // Scene Setup dock (SCENE_SETUP_PANEL_DESIGN D2) — cloned from the
        // Audio Setup dock above. Mutually exclusive with it at the toggle
        // call site (`toggle_scene_dock`/`toggle_audio_dock`), so in practice
        // at most one of these two blocks builds a non-empty region per frame.
        if self.layout.scene_setup_width > 0.0 {
            let dock = self.layout.scene_setup();
            let region = self.tree.begin_region(dock, ZTier::Base, "scene_setup", UIFlags::empty());
            let start = self.tree.count();
            self.scene_setup_panel.build_docked(&mut self.tree, dock);
            self.tree.end_region(region, start);
        }

        // Split handle + inspector resize handle — thin drag affordances at
        // panel seams. Chrome tier: utility chrome that should never be
        // occluded, same reasoning as transport/header/footer. One shared
        // region (mirrors `panel_cache_info`'s existing `SplitHandles` slot,
        // which already treats these two ad hoc nodes as one unit) at a
        // full-screen rect — the clip is a no-op (both handles are well
        // inside the screen) so this is purely stacking/registration, not
        // containment.
        let full_screen = Rect::new(0.0, 0.0, self.layout.screen_width, self.layout.screen_height);
        let region = self.tree.begin_region(full_screen, ZTier::Chrome, "split_handles", UIFlags::empty());
        let start = self.tree.count();
        // Split handle — thin bar between video and timeline areas.
        // From Unity PanelResizeHandle.cs: idle (transparent), hover, drag color states.
        {
            let r = self.layout.split_handle();
            self.split_handle_id = Some(self.tree.add_panel(
                None,
                r.x,
                r.y,
                r.width,
                r.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ));
        }

        // Inspector resize handle — thin vertical bar at the inspector's LEFT
        // edge (the inspector now sits on the right, preview to its left).
        // Drawn just *inside* the inspector (not straddling the edge): the
        // preview is an opaque GPU blit on top of the UI atlas and fills the
        // video area up to `insp.x`, so a straddling handle would have its left
        // half painted over. The hit test (`is_near_inspector_edge`) stays
        // centered on the seam, so the grab zone is unchanged.
        {
            let insp = self.layout.inspector();
            let edge_x = insp.x;
            self.inspector_handle_id = Some(self.tree.add_panel(
                None,
                edge_x,
                insp.y,
                4.0,
                insp.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ));
        }

        // Audio Setup dock resize handle — thin vertical bar at the dock's LEFT
        // edge (D1). Dragging it LEFT widens the dock (it expands leftward,
        // pushing the content). Only present while the dock is open.
        self.audio_setup_handle_id = None;
        if self.layout.audio_setup_width > 0.0 {
            let dock = self.layout.audio_setup();
            self.audio_setup_handle_id = Some(self.tree.add_panel(
                None,
                dock.x,
                dock.y,
                4.0,
                dock.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ));
        }

        // Scene Setup dock resize handle — cloned from the Audio Setup one
        // above (SCENE_SETUP_PANEL_DESIGN D2).
        self.scene_setup_handle_id = None;
        if self.layout.scene_setup_width > 0.0 {
            let dock = self.layout.scene_setup();
            self.scene_setup_handle_id = Some(self.tree.add_panel(
                None,
                dock.x,
                dock.y,
                4.0,
                dock.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ));
        }
        self.tree.end_region(region, start);

        // Mark boundary — everything after this is rebuilt on scroll.
        self.scroll_panels_start = self.tree.count();

        // Scroll-affected panels — rebuilt on scroll/zoom changes.
        self.build_scroll_panels();

        self.built = true;
    }

    /// Build just the inspector column into an explicit `rect` (the disjoint
    /// `inspector` + `tree` field split kept internal, like `build`). The
    /// graph-editor window calls this to host the same inspector column in its
    /// right lane (`dock.right`) — see docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md.
    /// Base tier region, same as the main window's own inspector build (D1/D5
    /// — the mechanism is window-agnostic; only the editor window's OWN
    /// panels, out of P1 scope, remain unmigrated).
    pub fn build_inspector_in_rect(&mut self, rect: Rect) {
        let region = self.tree.begin_region(rect, ZTier::Base, "inspector_in_rect", UIFlags::empty());
        let start = self.tree.count();
        self.inspector.build_in_rect(&mut self.tree, rect);
        self.tree.end_region(region, start);
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
            if let Some(action) = self.resolve_intent(event) {
                actions.push(action);
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
                UIEvent::Drag { pos, .. } => {
                    if self.inspector.is_card_drag_active() {
                        self.inspector.update_card_drag(*pos, &mut self.tree);
                    } else if self.inspector.has_pressed_target() {
                        let mut drag_actions = self.inspector.handle_drag(*pos, &mut self.tree);
                        actions.append(&mut drag_actions);
                    }
                }
                UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                    if self.inspector.is_card_drag_active() {
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

    /// Rebuild only scroll-affected panels (layer_headers, viewport, perf_hud).
    /// Static panels (transport, header, footer, inspector) keep their tree nodes.
    ///
    /// Uses `dirty` flags to skip layer header rebuild on horizontal-only scroll.
    /// Scroll the inspector in place (offset its content nodes) instead of
    /// rebuilding the whole tree. Returns true if handled in place; false means
    /// nothing is built yet and the caller should fall back to a full rebuild.
    /// Kept on `UIRoot` so the disjoint `inspector` + `tree` field borrows stay
    /// internal.
    pub fn try_inspector_scroll(&mut self, delta: f32, cursor_x: f32) -> bool {
        self.inspector
            .try_scroll_in_place(delta, cursor_x, &mut self.tree)
    }

    pub fn rebuild_scroll_panels(&mut self, dirty: ScrollDirty) {
        if !self.built {
            return self.build();
        }

        // Fast-path: scroll-only (no zoom, no visual) — update positions in-place.
        // No tree truncation, no hover invalidation, no node recreation.
        if dirty.is_scroll_only() {
            let mut ok = true;

            // Horizontal: update ruler ticks, markers, export markers
            if dirty.scroll_x {
                ok = self.viewport.try_update_horizontal_scroll(&mut self.tree);
            }

            // Vertical: update track bg Y positions + layer header Y positions
            if ok && dirty.scroll_y {
                let scroll_y_px = self.viewport.scroll_y_px();
                ok = self.layer_headers.try_update_vertical_scroll(
                    &mut self.tree,
                    &self.layout,
                    scroll_y_px,
                ) && self.viewport.try_update_vertical_scroll(&mut self.tree);
            }

            if ok {
                return;
            }
            // Fallback: count mismatch or never-built — do normal rebuild below.
        }

        if dirty.needs_layer_headers() {
            // Full scroll rebuild — includes layer headers
            self.tree.truncate_from(self.scroll_panels_start);
            self.input.invalidate_hover();
            self.build_scroll_panels();
        } else {
            // Horizontal-only (fallback) — skip layer headers, rebuild viewport + rest
            self.tree.truncate_from(self.viewport_panels_start);
            self.input.invalidate_hover();
            self.build_viewport_panels();
        }
    }

    /// Internal: build the scroll-affected panel group. Base tier, same as
    /// the static Base panels above (D1/D2) — `layer_headers`/`viewport`
    /// rebuild far more often than transport/header/footer/inspector, but
    /// the region mechanism costs the same one wrapper node per rebuild
    /// either way.
    fn build_scroll_panels(&mut self) {
        let region =
            self.tree
                .begin_region(self.layout.layer_controls(), ZTier::Base, "layer_headers", UIFlags::empty());
        let start = self.tree.count();
        self.layer_headers.build(
            &mut self.tree,
            &self.layout,
            self.viewport.mapper(),
            self.viewport.scroll_y_px(),
        );
        self.tree.end_region(region, start);

        // Record boundary between layer headers and viewport panels — now the
        // viewport region's own root index (captured before `begin_region`
        // mints it), so `truncate_from(viewport_panels_start)` still cleanly
        // drops the whole viewport region, not just its content.
        self.viewport_panels_start = self.tree.count();
        let region =
            self.tree
                .begin_region(self.layout.timeline_body(), ZTier::Base, "viewport", UIFlags::empty());
        let start = self.tree.count();
        self.viewport.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        // All top-level overlays (perf HUD + dropdown + modals) build at the
        // tail of the tree via the single overlay driver — one enumeration for
        // build, draw, and input. See build_overlays / route_overlay_event.
        self.build_overlays();
    }

    /// Internal: build viewport + remaining scroll panels (skip layer headers).
    /// Used on horizontal-only scroll where layer headers don't change.
    fn build_viewport_panels(&mut self) {
        let region =
            self.tree
                .begin_region(self.layout.timeline_body(), ZTier::Base, "viewport", UIFlags::empty());
        let start = self.tree.count();
        self.viewport.build(&mut self.tree, &self.layout);
        self.tree.end_region(region, start);

        // Overlays build at the tail of the tree via the single driver.
        self.build_overlays();
    }

    // ── Overlay driver ──────────────────────────────────────────────
    // One enumeration of the top-level overlays for build, draw, and input.
    // The exhaustive `overlay_mut` match is what makes "built but never drawn"
    // unrepresentable. See docs/OVERLAY_SYSTEM_DESIGN.md.

    /// The overlay for an id. The exhaustive match forces every new overlay to
    /// be wired into the driver.
    fn overlay_mut(&mut self, id: OverlayId) -> &mut dyn Overlay {
        match id {
            OverlayId::PerfHud => &mut self.perf_hud,
            OverlayId::Dropdown => &mut self.dropdown,
            OverlayId::Settings => &mut self.settings_popup,
            OverlayId::BrowserPopup => &mut self.browser_popup,
            OverlayId::AbletonPicker => &mut self.ableton_picker,
            OverlayId::Toast => &mut self.toast,
        }
    }

    /// Whether the overlay for `id` is currently open. Immutable mirror of
    /// `overlay_mut(id).is_open()` — the exhaustive match keeps it in lockstep
    /// with the driver registry. `pub(crate)`: used by `note_overlay_closed_if`
    /// callers outside this module to read the post-close state.
    pub(crate) fn overlay_is_open(&self, id: OverlayId) -> bool {
        match id {
            OverlayId::PerfHud => self.perf_hud.is_open(),
            OverlayId::Dropdown => self.dropdown.is_open(),
            OverlayId::Settings => self.settings_popup.is_open(),
            OverlayId::BrowserPopup => self.browser_popup.is_open(),
            OverlayId::AbletonPicker => self.ableton_picker.is_open(),
            OverlayId::Toast => self.toast.is_open(),
        }
    }

    /// Live open-set as a bitmask, bit `i` = `OverlayId::Z_ORDER[i]` is open.
    /// Seven overlays today, so a `u8` has room to spare.
    fn current_overlay_open_mask(&self) -> u8 {
        let mut mask = 0u8;
        for (i, id) in OverlayId::Z_ORDER.iter().enumerate() {
            if self.overlay_is_open(*id) {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// True if the live overlay open-set differs from what `build_overlays` last
    /// recorded — i.e. an overlay opened or closed (event-driven OR programmatic)
    /// and the overlay region in the tree is now stale. The app calls this once
    /// per frame and, on `true`, schedules a visual rebuild so the overlay region
    /// is re-recorded into `overlay_draw` and the offscreen recomposites. Read
    /// only; the snapshot updates when `build_overlays` actually runs.
    pub fn detect_overlay_open_change(&self) -> bool {
        self.built && self.current_overlay_open_mask() != self.overlay_open_snapshot
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6: the redraw keepalive as ONE
    /// aggregate predicate, OR-ed into each window's own `offscreen_dirty` by
    /// its caller (never a per-window keepalive list). Replaces the old
    /// pattern of each animation source hand-wiring its own poll into the
    /// main tick — a missed wire there was a frozen animation in exactly one
    /// window, the redraw-side sibling of BUG-151.
    ///
    /// Membership re-derived at P2 impl via `rg "is_animating|tick\(" crates/
    /// manifold-ui/src/panels/`, not assumed from the design doc's original
    /// "toast timers and any remaining overlay tween" guess: the popup
    /// professional pass already stubbed `browser_popup`/`ableton_picker`/
    /// `settings_popup`'s `is_animating()` to a hardcoded `false` (their own
    /// doc comments say so — "no tween to settle"/"no tween to advance"), and
    /// `dropdown` never had one. Calling those permanently-false stubs here
    /// would be dead weight this predicate can never observe going `true` —
    /// so they're deliberately NOT OR-ed in; reviving any of their tweens is
    /// a one-line addition to this function, not a design change. The one
    /// live member today is the toast: its `Transient` keeps progressing
    /// through enter/hold/fade after `show()` fires and needs a tick each
    /// frame to detect completion — exactly the "hand-wired keepalive" this
    /// predicate exists to centralize.
    pub fn overlay_redraw_needed(&self) -> bool {
        self.toast.is_animating()
    }

    /// Build every open overlay into the tree, bottom→top, recording each one's
    /// node range for the draw pass. A modal that requests a dim background gets
    /// a full-screen scrim node first (and a click on it dismisses the modal,
    /// since the scrim is not one of the modal's own nodes).
    ///
    /// D1/D2: each open overlay gets its OWN region — `Overlay` tier for
    /// popups/modals, `Ghost` for the toast (a status message must stay
    /// legible over an open modal/dropdown, the same reason `Z_ORDER`
    /// already placed it last/topmost — D2/D3's "drag ghost/toast paths").
    /// `Z_ORDER`'s bottom→top loop order becomes insertion order WITHIN
    /// each tier, so relative overlay stacking is unchanged. The region's
    /// own root index is deliberately kept OUT of the `(start, end)` range
    /// this records into `overlay_draw` — `app_render.rs`'s shadow-peek
    /// heuristic (skip a leading full-screen scrim) reads `tree.id_at(start)`
    /// expecting the first REAL overlay node, not a region wrapper — so
    /// `app_render.rs` renders these ranges via `render_sub_region` (ancestor-
    /// aware: it walks the parent chain from `start` and picks up the
    /// region's `CLIPS_CHILDREN` even though the region root itself sits
    /// one index before `start`), not `render_tree_range`.
    fn build_overlays(&mut self) {
        let screen = Vec2::new(self.screen_width, self.screen_height);
        // Take the tree out so `overlay_mut` (which borrows all of self) can run
        // alongside tree writes — standard disjoint-borrow split.
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let region_start = tree.count();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut rects: Vec<(OverlayId, Rect)> = Vec::new();
        let full_screen = Rect::new(0.0, 0.0, screen.x, screen.y);
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if !ov.is_open() {
                continue;
            }
            let tier = if id == OverlayId::Toast { ZTier::Ghost } else { ZTier::Overlay };
            let region = tree.begin_region(full_screen, tier, "overlay", UIFlags::empty());
            let start = tree.count();
            if let Modality::Modal {
                dim_background: true,
            } = ov.modality()
            {
                tree.add_panel(
                    None,
                    0.0,
                    0.0,
                    screen.x,
                    screen.y,
                    manifold_ui::node::UIStyle {
                        bg_color: manifold_ui::node::Color32::new(0, 0, 0, 120),
                        ..manifold_ui::node::UIStyle::default()
                    },
                );
            }
            let anchor = ov.anchor();
            // Resolve the overlay's size policy against the screen (content-sized
            // by default; viewport-relative overlays scale here) before centering.
            let size = ov.size_policy().resolve(screen, ov.desired_size());
            let node_rect = if let Anchor::ToNode(nid) = anchor {
                Some(tree.get_bounds(nid))
            } else {
                None
            };
            let rect = compute_overlay_rect(&anchor, size, screen, node_rect);
            ov.build_at(&mut tree, OverlayPlacement { rect, screen });
            tree.end_region(region, start);
            ranges.push((start, tree.count()));
            rects.push((id, rect));
        }
        self.tree = tree;
        self.overlay_region_start = region_start;
        self.overlay_draw = ranges;
        self.overlay_rects = rects;
        // The tree's overlay region now matches the live open-set — record it so
        // `detect_overlay_open_change` only fires on the next genuine open/close.
        self.overlay_open_snapshot = self.current_overlay_open_mask();
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1 (D2 precondition, fix-shape
    /// spec 2026-07-14): explicit-size entry point onto `build_overlays` for
    /// windows that never call `build()`. The graph editor's `Workspace::
    /// ui_root` is built via plain `UIRoot::new()` (`workspace.rs`) and never
    /// `.build()` — that method clears the tree and lays out the WHOLE
    /// main-window panel set (transport/header/footer/inspector-at-main-
    /// layout/audio-setup dock/timeline viewport) via `self.layout`, which
    /// would stomp the editor's own per-frame tree build and inject
    /// main-window UI into the editor. `build_overlays` itself is safe
    /// standalone: it only reads `screen_width`/`screen_height` and appends
    /// to the tree tail based on which overlays are open on THIS instance —
    /// no main-window-only state. The explicit size is load-bearing: the
    /// editor's `UIRoot` never receives `resize()` either (only `self.ws`
    /// does, and `resize()` itself calls `build()` — never usable for the
    /// editor), so `screen_width`/`screen_height` would otherwise be stuck at
    /// their `UIRoot::new()` default and `build_overlays`' full-screen region
    /// (`CLIPS_CHILDREN`) would clip the popup to nothing.
    pub(crate) fn build_overlays_for_screen(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
        self.build_overlays();
    }

    /// Route one event to the open overlays, top→bottom. Returns true if an
    /// overlay consumed it (or a modal captured it), so the caller skips the
    /// lower panels. Stashed selections are lowered by `drain_overlay_selections`.
    /// Also records into `closed_overlays` any overlay whose `on_event` flipped
    /// it shut (self-close on Escape / backdrop / cell pick) — §3, D2.
    fn route_overlay_event(&mut self, event: &UIEvent, actions: &mut Vec<PanelAction>) -> bool {
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let mut consumed = false;
        for id in OverlayId::Z_ORDER.iter().rev() {
            let ov = self.overlay_mut(*id);
            if !ov.is_open() {
                continue;
            }
            let response = ov.on_event(event, &mut tree);
            let still_open = ov.is_open();
            let is_modal = matches!(ov.modality(), Modality::Modal { .. });
            if !still_open {
                self.closed_overlays.push(*id);
            }
            match response {
                OverlayResponse::Consumed(acts) => {
                    actions.extend(acts);
                    consumed = true;
                    if manifold_ui::input::input_trace_enabled() && trace_worthy(event) {
                        eprintln!(
                            "[input-trace] ui_root: {} CONSUMED by overlay {id:?}",
                            trace_kind(event)
                        );
                    }
                    break;
                }
                OverlayResponse::Ignored => {
                    if is_modal {
                        // A modal captures everything — no fall-through below it.
                        consumed = true;
                        if manifold_ui::input::input_trace_enabled() && trace_worthy(event) {
                            eprintln!(
                                "[input-trace] ui_root: {} CAPTURED by modal {id:?} (ignored \
                                 but not passed through)",
                                trace_kind(event)
                            );
                        }
                        break;
                    }
                }
            }
        }
        self.tree = tree;
        if consumed {
            self.overlay_dirty = true;
        }
        consumed
    }

    /// D5 — does an OPEN overlay's on-screen rect contain `pos`? Walks
    /// `Z_ORDER` top-down, same open-check `route_overlay_event` uses, over
    /// the rects `build_overlays` last recorded (`overlay_rects`) — the same
    /// rect an overlay was actually placed at, so this agrees with what's on
    /// screen. Used by `window_input`'s split-handle / inspector-edge press
    /// checks so a seam visually UNDER a floating overlay (the Audio Setup
    /// panel docked over the timeline, BUG-059) doesn't steal the press.
    /// `overlay_rects`' doc comment names the one known gap (`SelfManaged`
    /// overlays).
    pub(crate) fn overlay_contains_point(&self, pos: Vec2) -> bool {
        for id in OverlayId::Z_ORDER.iter().rev() {
            if !self.overlay_is_open(*id) {
                continue;
            }
            if let Some((_, rect)) = self.overlay_rects.iter().find(|(oid, _)| oid == id)
                && rect.contains(pos)
            {
                return true;
            }
        }
        false
    }

    /// D1 — resolve who owns an in-flight drag gesture, once, at the
    /// gesture's first `DragBegin`. Fixed order, first claim wins (§3.2):
    /// open overlays z-top-down → inspector → layer headers → ruler →
    /// timeline tracks → nobody. `node_id` is accepted for signature parity
    /// with the design's committed call (`docs/DRAG_CAPTURE_DESIGN.md` §3.2)
    /// — no resolution step needs it today; every claim is origin/state based.
    fn resolve_drag_owner(&mut self, origin: Vec2, _node_id: Option<NodeId>) -> Option<DragOwner> {
        // 1. Open overlays, z-top-down — same walk as `route_overlay_event`,
        // but this pass only reads `is_open`/`modality`/`claims_drag`; it
        // never delivers the event (that still happens through the normal
        // gauntlet). A modal claims unconditionally (D4). A modeless overlay
        // claims iff `claims_drag(origin)` says so (P1: no overlay overrides
        // the default `false` yet — the audio panel's override lands P2).
        // The dropdown specifically: an open dropdown that does NOT claim is
        // dismissed here as a side effect, WITHOUT consuming (D3) — same UX
        // as today's outside-click dismiss, minus the BUG-058 eat-arm.
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let mut owner = None;
        for id in OverlayId::Z_ORDER.iter().rev() {
            let ov = self.overlay_mut(*id);
            if !ov.is_open() {
                continue;
            }
            if matches!(ov.modality(), Modality::Modal { .. }) {
                owner = Some(DragOwner::Overlay(*id));
                break;
            }
            if ov.claims_drag(origin) {
                owner = Some(DragOwner::Overlay(*id));
                break;
            }
            if *id == OverlayId::Dropdown {
                self.dropdown.close(&mut tree);
                self.closed_overlays.push(OverlayId::Dropdown);
                self.overlay_dirty = true;
            }
        }
        self.tree = tree;
        if owner.is_some() {
            return owner;
        }

        // 2. Inspector — slider drag (`pressed_target`, armed on PointerDown)
        // or effect-card reorder (`card_drag_active`, armed on DragBegin by
        // the caller just before this resolution runs).
        if self.inspector.has_pressed_target() || self.inspector.is_card_drag_active() {
            return Some(DragOwner::Inspector);
        }
        // 3. Layer headers — reorder or gain drag (same arm-then-resolve
        // ordering as the inspector).
        if self.layer_headers.is_dragging() || self.layer_headers.is_gain_dragging() {
            return Some(DragOwner::LayerHeaders);
        }
        // 4. Ruler (D7 — confirmed kept, `viewport/interaction.rs` scrub is
        // Drag-based).
        if self.viewport.ruler_rect().contains(origin) {
            return Some(DragOwner::Ruler);
        }
        // 5. TimelineTracks — the fallback today's stash gate approximated.
        if self.viewport.tracks_rect().contains(origin) {
            return Some(DragOwner::TimelineTracks);
        }
        // 6. Nobody.
        None
    }

    /// Fire the end-of-gesture hook on every OPEN overlay (idempotent
    /// `gesture_ended` clears; default no-op, the audio panel overrides).
    /// Does NOT touch `drag_owner` — split out of `broadcast_gesture_end`
    /// so the terminal-event path can fire the hooks while `drag_owner` is
    /// still live for the stash classification, then clear the owner as the
    /// last step of the terminal iteration (see `process_events`). Clearing
    /// the owner one line too early was BUG-075: it nulled the owner before
    /// `should_stash_for_tracks` read it, so a timeline gesture's terminal
    /// `DragEnd` was never stashed and `on_end_drag` never ran (trim /
    /// marquee never finalized).
    fn fire_gesture_end_hooks(&mut self) {
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if ov.is_open() {
                ov.gesture_ended();
            }
        }
        // The Audio Setup dock is no longer an overlay (D1) but keeps the same
        // idempotent end-of-gesture clear for its band/calibration drags.
        if self.audio_setup_panel.is_open() {
            self.audio_setup_panel.gesture_ended();
        }
    }

    /// D2/§3.3 — the terminal broadcast every gesture that began gets,
    /// exactly once, no matter who owned it or what ate the routed event.
    /// The fused form (hooks + `drag_owner` clear) is the self-heal on the
    /// next `PointerDown` when a stale owner survived (a lost OS release —
    /// `docs/DRAG_CAPTURE_DESIGN.md` §3.3 failure story). The normal terminal
    /// path does NOT use this — it calls `fire_gesture_end_hooks` and defers
    /// the clear past the stash read (see `process_events`); the two must not
    /// be re-fused (BUG-075).
    fn broadcast_gesture_end(&mut self) {
        self.fire_gesture_end_hooks();
        self.drag_owner = None;
    }

    /// D6/§3.4 (`docs/DRAG_CAPTURE_DESIGN.md`) — after a `PointerDown` is
    /// consumed by `route_overlay_event`, does any OPEN overlay want
    /// immediate-drag armed for this press? `route_overlay_event` returns
    /// only whether an overlay consumed the event, not which one — but only
    /// the overlay that actually consumed THIS `PointerDown` could have just
    /// armed anything (every other open overlay never saw the event, and its
    /// per-press state is cleared every gesture end by `gesture_ended`), so
    /// polling the whole open set (same `Z_ORDER` walk `broadcast_gesture_end`
    /// uses) identifies the same overlay without threading its identity back
    /// out of `route_overlay_event`.
    fn any_overlay_wants_immediate_drag(&mut self) -> bool {
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if ov.is_open() && ov.wants_immediate_drag() {
                return true;
            }
        }
        false
    }

    /// Record `id` as closed if it was open before some out-of-band close
    /// attempt and isn't now — for close paths that don't go through
    /// `route_overlay_event`. The graph-editor window's browser popup is the
    /// live example: while it's open, the editor routes clicks straight to
    /// `browser_popup.handle_click`/`handle_escape` (bypassing the overlay
    /// driver entirely — see `app_render.rs`'s `browser_popup.is_open()`
    /// branch), so no `route_overlay_event` call ever observes its
    /// open→closed transition. The caller snapshots `was_open` immediately
    /// before the bespoke call and passes it here immediately after.
    pub(crate) fn note_overlay_closed_if(&mut self, id: OverlayId, was_open: bool) {
        if was_open && !self.overlay_is_open(id) {
            self.closed_overlays.push(id);
        }
    }

    /// Overlays whose `is_open()` flipped false since the last drain (via
    /// `route_overlay_event` or `note_overlay_closed_if`). Drained once per
    /// frame per window by the app pump, which maps each id to a
    /// `TextSessionOwner` and calls `cancel_if_owned_by` — closing the
    /// orphaned-search-session bug for every current and future
    /// overlay-hosted text field, not just the browser search
    /// (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3).
    pub fn take_closed_overlays(&mut self) -> smallvec::SmallVec<[OverlayId; 2]> {
        std::mem::take(&mut self.closed_overlays)
    }

    /// Lower any selection an overlay stashed during `route_overlay_event` into
    /// a `PanelAction`. The dropdown and Ableton picker can't form their actions
    /// themselves — the resolving context lives on `UIRoot` (the dropdown also
    /// needs cached device / resolution lists).
    fn drain_overlay_selections(&mut self, actions: &mut Vec<PanelAction>) {
        if let Some(dd_action) = self.dropdown.take_pending_action() {
            match dd_action {
                // Typed item — carries its own action, no index→meaning map (2b.11).
                DropdownAction::SelectedAction(action) => {
                    self.dropdown_context = None;
                    actions.push(action);
                }
                DropdownAction::Selected(_) => {
                    // 2b.11: every selectable item is typed and fires SelectedAction
                    // above, so a positional Selected can only be a non-action item.
                    // Nothing to map — just drop any stale context once closed.
                    if !self.dropdown.is_open() {
                        self.dropdown_context = None;
                    }
                }
                DropdownAction::ColorSelected(color_idx) => {
                    if let Some(ctx) = self.dropdown_context.take()
                        && let Some(action) = self.dropdown_color_to_action(ctx, color_idx)
                    {
                        actions.push(action);
                    }
                }
                DropdownAction::Dismissed => {
                    // Disabled-item clicks send Dismissed but keep the dropdown
                    // open — only clear context once it actually closed.
                    if !self.dropdown.is_open() {
                        self.dropdown_context = None;
                    }
                }
            }
        }
        if let Some(addr) = self.ableton_picker.take_pending_selection()
            && let Some(ctx) = self.ableton_picker_context.take()
        {
            use manifold_ui::panels::ableton_picker::AbletonPickerContext;
            actions.push(match ctx {
                AbletonPickerContext::Param { gpt, param_id } => {
                    PanelAction::MapParamToAbleton(gpt, param_id, addr)
                }
                AbletonPickerContext::MacroSlot { slot_idx } => {
                    PanelAction::MapMacroToAbleton(slot_idx, addr)
                }
            });
        }
    }

    /// Route an Escape through the overlay driver. The keyboard path consumes
    /// Escape before it reaches `process_events`, so the input-handler escape
    /// chain calls this. Returns true if an open, dismissable overlay handled it
    /// — the perf HUD (modeless, never-consuming) does not, so Escape falls
    /// through to selection clearing when only the HUD is up.
    pub fn escape_overlays(&mut self) -> bool {
        let event = UIEvent::KeyDown {
            node_id: NodeId::PLACEHOLDER,
            key: Key::Escape,
            modifiers: Modifiers::default(),
        };
        let mut actions = Vec::new();
        let consumed = self.route_overlay_event(&event, &mut actions);
        if consumed {
            self.drain_overlay_selections(&mut actions);
        }
        consumed
    }

    /// Handle a resize event. Rebuilds all panels.
    pub fn resize(&mut self, width: f32, height: f32) {
        let same_size =
            (width - self.screen_width).abs() < 1.0 && (height - self.screen_height).abs() < 1.0;
        if same_size && self.built {
            return;
        }
        self.screen_width = width;
        self.screen_height = height;
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
        if action == PointerAction::Move {
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

    // `open_dropdown_at` (generic DropdownContext-carrying opener) deleted
    // (§7.2 item 7, P8, 2026-07-11) — its only caller was
    // `AudioSendRoutingsClicked`. The one surviving `DropdownContext`
    // (`LayerContext`, the layer-color swatches) sets `dropdown_context`
    // directly at its own call site instead.

    /// Open a dropdown whose items carry their own actions (2b.11). No
    /// `DropdownContext` is stored — each item returns
    /// `DropdownAction::SelectedAction`, which the drain fires directly, so there
    /// is no positional index→meaning map to keep in sync.
    pub(crate) fn open_dropdown_typed(&mut self, items: Vec<DropdownItem>, trigger: Rect) {
        self.dropdown_context = None;
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
        self.overlay_dirty = true;
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

    // `set_audio_trigger_layers` (the matrix's target-layer dropdown cache
    // setter) is deleted with the matrix (P3, D2). `set_audio_layers`
    // (Inputs section "+ Layer" candidates) is deleted with the section's
    // authoring (§7.2 item 7, P8, 2026-07-11).

    /// Refresh the embedded-preset list surfaced into the Add pickers from the
    /// project snapshot. Change-gated by the embedded-preset fingerprint so the
    /// Vec rebuilds only when a fork / import / remove actually changed the set,
    /// not every frame. Called from the per-frame UI sync before event routing.
    pub fn sync_embedded_presets(&mut self, project: &manifold_core::project::Project) {
        let fp = crate::project_io::embedded_presets_fingerprint(project);
        if fp == self.embedded_presets_fingerprint {
            return;
        }
        self.embedded_presets_fingerprint = fp;
        self.embedded_presets = project
            .embedded_presets
            .iter()
            .filter_map(|ep| {
                let meta = ep.def.preset_metadata.as_ref()?;
                Some(EmbeddedPresetItem {
                    kind: ep.kind,
                    type_id: meta.id.as_str().to_string(),
                    display_name: meta.display_name.to_string(),
                    origin: ep.origin,
                })
            })
            .collect();
    }

    /// Classify one `kind`'s Add-picker items by source
    /// (PRESET_LIBRARY_DESIGN P5, D6) — the single place this rule lives, so
    /// `AddEffectClicked` and `GenTypeClicked` can't drift apart:
    /// - **Factory** / **My Library**: every id `preset_type_registry`
    ///   resolves, split by `UserLibrary::is_user_entry` (a file under the
    ///   user root vs. not).
    /// - **This Project**: every `origin: Saved` embedded preset, always
    ///   listed; an `origin: Snapshot` embedded preset ONLY when its id
    ///   resolves nowhere in the registry (its library file is gone) —
    ///   badged "missing from library" rather than "Project" so the browser
    ///   reads as plumbing, not a real project preset.
    ///
    /// `tag_project_category` sets `category: Some("Project")` on the
    /// This-Project items (Effect mode's existing "Project" chip grouping);
    /// Generator mode passes `false`, matching its pre-P5 behavior of never
    /// tagging generator items by category (it renders no category chips).
    fn build_preset_picker_items(
        &self,
        kind: manifold_core::preset_def::PresetKind,
        tag_project_category: bool,
    ) -> Vec<manifold_ui::panels::picker_core::PickerItem> {
        use manifold_core::preset_type_registry;
        use manifold_ui::panels::picker_core::{PickerItem, Source};

        let lib = crate::user_library::UserLibrary::new();
        let available = preset_type_registry::available_of_kind(kind);
        let mut seen_ids: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(available.len());

        let mut items: Vec<PickerItem> = available
            .iter()
            .map(|reg| {
                let is_user = lib.is_user_entry(kind, &reg.id);
                let id = reg.id.as_str().to_string();
                seen_ids.insert(id.clone());
                // PRESET_LIBRARY_DESIGN P6, D7: a My-Library entry's PNG
                // sits beside its JSON (`UserLibrary::thumbnail_path`); a
                // Factory entry's comes from the committed one-shot bin
                // output. `None` (no `Path::is_file` check needed further)
                // when the file simply doesn't exist yet — clean text
                // fallback, never a browse-time render.
                let thumbnail = if is_user {
                    let p = lib.thumbnail_path(kind, reg.id.as_str());
                    p.is_file().then(|| p.to_string_lossy().into_owned())
                } else {
                    manifold_renderer::preset_thumbnail::factory_thumbnail_path(kind, reg.id.as_str())
                        .filter(|p| p.is_file())
                        .map(|p| p.to_string_lossy().into_owned())
                };
                PickerItem {
                    label: reg.display_name.to_string(),
                    type_id: id,
                    category: if tag_project_category {
                        reg.category.map(|c| c.to_string())
                    } else {
                        None
                    },
                    search_text: None,
                    badge: Some(if is_user { "My Library" } else { "Factory" }.to_string()),
                    source: Some(if is_user { Source::MyLibrary } else { Source::Factory }),
                    missing_from_library: false,
                    thumbnail,
                }
            })
            .collect();

        for e in self.embedded_presets.iter().filter(|e| e.kind == kind) {
            use manifold_core::project::EmbeddedOrigin;
            let missing = match e.origin {
                EmbeddedOrigin::Saved => false,
                // A Snapshot whose id already resolves elsewhere (disk file
                // still there) is already represented via `available` above
                // — skip it entirely rather than list it twice.
                EmbeddedOrigin::Snapshot => {
                    if seen_ids.contains(&e.type_id) {
                        continue;
                    }
                    true
                }
            };
            items.push(PickerItem {
                label: e.display_name.clone(),
                type_id: e.type_id.clone(),
                category: if tag_project_category { Some("Project".to_string()) } else { None },
                search_text: None,
                badge: Some(
                    if missing { "missing from library" } else { "Project" }.to_string(),
                ),
                source: Some(Source::Project),
                missing_from_library: missing,
                // This-Project entries never get a thumbnail (D7 only
                // covers Save to Library + the factory bin) — text fallback.
                thumbnail: None,
            });
        }

        items
    }

    /// Clear and repopulate node-intent dispatch from every panel's currently
    /// stored node ids. A full rebuild each call keeps the registry consistent
    /// with partial tree rebuilds (truncate_from) without per-range bookkeeping
    /// — panels register against whatever ids they hold now.
    fn repopulate_intents(&mut self) {
        use manifold_ui::panels::Panel;
        self.intents.clear();
        self.transport.register_intents(&mut self.intents);
        self.header.register_intents(&mut self.intents);
        self.footer.register_intents(&mut self.intents);
        self.layer_headers.register_intents(&mut self.intents);
        self.inspector.register_intents(&mut self.intents);
        self.viewport.register_intents(&mut self.intents);
    }

    /// Resolve a discrete-gesture event through node-intent dispatch. Returns
    /// the registered `PanelAction` for the nearest intent-bearing ancestor of
    /// the hit node, or None for non-gesture events / un-registered surfaces.
    fn resolve_intent(&self, event: &UIEvent) -> Option<PanelAction> {
        use manifold_ui::intent::Gesture;
        let (node_id, gesture) = match event {
            UIEvent::Click { node_id, .. } => (Some(*node_id), Gesture::Click),
            UIEvent::DoubleClick { node_id, .. } => (Some(*node_id), Gesture::DoubleClick),
            UIEvent::RightClick { node_id, .. } => (*node_id, Gesture::RightClick),
            _ => return None,
        };
        self.intents.resolve(&self.tree, node_id, gesture)
    }

    /// Drain events from the input system and route to panels.
    /// Returns all panel actions for the app layer to dispatch.
    pub fn process_events(&mut self) -> Vec<PanelAction> {
        if !self.built {
            return Vec::new();
        }

        // Refresh node-intent dispatch only when the tree structurally changed
        // (gated on the tree's structure_version) — never per-frame, so the
        // registry's per-entry boxing stays off the hot path. Set-only frames
        // (hover, value sync) leave node ids intact and skip this entirely.
        let sv = self.tree.structure_version();
        if sv != self.intents_structure_version {
            self.repopulate_intents();
            self.intents_structure_version = sv;
        }

        let events = self.input.drain_events();
        let mut actions = Vec::new();

        // Drain continuous hover actions accumulated from cursor movement.
        actions.append(&mut self.cursor_hover_actions);
        // Drain keyboard-driven picker actions (arrow/Enter nav) stashed by
        // `window_input.rs`'s text-input-active branch — see
        // `pending_keyboard_actions`'s doc comment.
        actions.append(&mut self.pending_keyboard_actions);

        let mut last_click_node: Option<NodeId> = None;
        for event in &events {
            // Track which node was clicked (for dropdown anchoring).
            if let UIEvent::Click { node_id, .. } = event {
                last_click_node = Some(*node_id);
            }
            if let UIEvent::RightClick { pos, .. } = event {
                self.last_right_click_pos = *pos;
            }

            // Self-heal (§3.3 failure story (b)): a stale owner can only mean
            // the previous gesture's terminal event never reached the
            // broadcast (a lost OS release at the window seam — BUG-028
            // precedent). The next PointerDown clears it, firing the same
            // unconditional broadcast a normal terminal event would.
            if matches!(event, UIEvent::PointerDown { .. }) && self.drag_owner.is_some() {
                self.broadcast_gesture_end();
            }

            // Global: ⌘⇧A toggles the Audio Setup panel. Emit the same action the
            // "audio" button does so the single app-side handler owns the toggle
            // plus its one-shot data sync — rather than toggling here and leaving
            // the panel's device/send list unpopulated. Handled before overlay
            // routing so an open modal can't capture the keystroke and block it
            // from toggling shut.
            if let UIEvent::KeyDown { key: Key::A, modifiers, .. } = event
                && modifiers.command
                && modifiers.shift
            {
                actions.push(PanelAction::OpenAudioSetup);
                continue;
            }

            // All open overlays (dropdown, modals, perf HUD) get first crack at
            // the event through the single driver. If one consumes it (or a modal
            // captures it), lower any stashed selection and skip the panels below.
            if self.route_overlay_event(event, &mut actions) {
                // D6/§3.4 (`docs/DRAG_CAPTURE_DESIGN.md`): the consuming overlay
                // may have just armed a precision-drag surface (audio panel's
                // band divider) on this exact PointerDown — request zero-
                // threshold drag for the current press so the very next Move
                // begins the drag instead of waiting for the 4px threshold.
                if matches!(event, UIEvent::PointerDown { .. })
                    && self.any_overlay_wants_immediate_drag()
                {
                    self.input.request_immediate_drag();
                }
                self.drain_overlay_selections(&mut actions);
                continue;
            }

            // Escape closes the Audio Setup dock (D1/§3.5) — the ONE key path,
            // handled AFTER overlays (a dropdown/settings opened over the app
            // gets Escape first) and routed through the same `OpenAudioSetup`
            // toggle the header button and the × use.
            if self.audio_setup_panel.is_open()
                && matches!(event, UIEvent::KeyDown { key: Key::Escape, .. })
            {
                actions.push(PanelAction::OpenAudioSetup);
                continue;
            }

            // Escape closes the Scene Setup dock — same mirrored path
            // (SCENE_SETUP_PANEL_DESIGN D2).
            if self.scene_setup_panel.is_open()
                && matches!(event, UIEvent::KeyDown { key: Key::Escape, .. })
            {
                actions.push(PanelAction::OpenSceneSetup);
                continue;
            }

            // Audio Setup dock (D1) — a docked panel routed here, not an
            // overlay. It handles its own clicks + band/calibration drags and
            // consumes them so they don't fall through to the panels beneath.
            // A `PointerDown` that armed a band-divider grab requests immediate
            // drag so a 1px move begins the drag (no 4px threshold wait).
            if self.audio_setup_panel.is_open() {
                let (consumed, mut acts) = self.audio_setup_panel.handle_event(event);
                actions.append(&mut acts);
                if consumed {
                    if matches!(event, UIEvent::PointerDown { .. })
                        && self.audio_setup_panel.wants_immediate_drag()
                    {
                        self.input.request_immediate_drag();
                    }
                    continue;
                }
            }

            // Scene Setup dock — mirror of the Audio Setup routing above.
            if self.scene_setup_panel.is_open() {
                let (consumed, mut acts) = self.scene_setup_panel.handle_event(event);
                actions.append(&mut acts);
                if consumed {
                    continue;
                }
            }

            // Node-intent dispatch: discrete gestures (click / double-click /
            // right-click) resolve by folding the hit node up its parent chain
            // to the nearest ancestor carrying intent. Migrated panels register
            // intent in `build`/`register_intents` and drop their `handle_event`
            // arms; for un-migrated surfaces `resolve` returns None and the
            // event flows to the per-panel handlers below unchanged. A resolved
            // gesture is consumed here — it would otherwise double-fire.
            if let Some(action) = self.resolve_intent(event) {
                actions.push(action);
                continue;
            }

            // Route to panels. Transport, header, and footer are fully
            // intent-dispatched (see `resolve_intent` above) — their clicks
            // resolve and `continue` before reaching here, so they have no
            // panel-side click handler to call.
            let mut panel_actions = self.layer_headers.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.inspector.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            // Viewport: ruler events handled by viewport panel (Seek/scrub).
            // Tracks-area events stashed for InteractionOverlay in app.rs.
            panel_actions = self.viewport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);
        }

        // Route Drag/DragEnd/PointerUp to inspector directly (needs &mut tree for
        // slider feedback). Separate from the panel event loop because
        // Panel::handle_event takes &UITree, but slider drag updates need &mut UITree.
        //
        // PointerUp handling: Unity's OnPointerUp ALWAYS fires on mouse release.
        // If the user clicked a slider without crossing the 4px DRAG_THRESHOLD,
        // no DragEnd fires — but PointerUp still does. We route PointerUp through
        // handle_drag_end so the sub-panel's dragging state is cleared and the
        // undo snapshot is committed. handle_drag_end is idempotent: if DragEnd
        // already cleared pressed_target, PointerUp is a no-op.
        //
        // D1/D2 (`docs/DRAG_CAPTURE_DESIGN.md` §3.2/§3.3): `DragBegin` still arms
        // the inspector/layer-header drag state unconditionally, exactly as
        // before — that arming is what `resolve_drag_owner` reads immediately
        // after, to fix `drag_owner` for the rest of the gesture. `Drag`
        // continuation is gated on the now-fixed owner instead of re-checking
        // the private flags directly. `DragEnd`/`PointerUp` keep the existing
        // unconditional, idempotent end-calls (the broadcast precedent this
        // design generalizes), then `fire_gesture_end_hooks` runs every other
        // overlay's `gesture_ended` hook. The `drag_owner` clear is deferred
        // to the END of the terminal iteration — past the stash read — so the
        // timeline's terminal `DragEnd` is still routed to it (BUG-075).
        for event in &events {
            match event {
                UIEvent::DragBegin { node_id, origin, .. } => {
                    // Effect card drag handle — try to start card reorder drag
                    self.inspector.try_begin_card_drag(*node_id, &mut self.tree);
                    // Layer header drag handle — needs &mut tree for dim/indicator
                    let mut lh_actions = self
                        .layer_headers
                        .handle_drag_begin(&mut self.tree, *node_id);
                    actions.append(&mut lh_actions);
                    self.drag_owner = self.resolve_drag_owner(*origin, *node_id);
                }
                UIEvent::Drag { pos, .. } => {
                    if self.drag_owner == Some(DragOwner::Inspector) {
                        if self.inspector.is_card_drag_active() {
                            self.inspector.update_card_drag(*pos, &mut self.tree);
                        } else if self.inspector.has_pressed_target() {
                            let mut drag_actions =
                                self.inspector.handle_drag(*pos, &mut self.tree);
                            actions.append(&mut drag_actions);
                        }
                    }
                    if self.drag_owner == Some(DragOwner::LayerHeaders) {
                        if self.layer_headers.is_dragging() {
                            let mut lh_actions = self.layer_headers.handle_drag(
                                &mut self.tree,
                                *pos,
                                self.viewport.mapper(),
                            );
                            actions.append(&mut lh_actions);
                        }
                        if self.layer_headers.is_gain_dragging() {
                            let mut g_actions =
                                self.layer_headers.handle_gain_drag(&mut self.tree, pos.x);
                            actions.append(&mut g_actions);
                        }
                    }
                }
                UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                    if self.inspector.is_card_drag_active() {
                        let mut reorder_actions = self.inspector.end_card_drag(&mut self.tree);
                        actions.append(&mut reorder_actions);
                    } else if self.inspector.has_pressed_target() {
                        let mut end_actions = self.inspector.handle_drag_end(&mut self.tree);
                        actions.append(&mut end_actions);
                    }
                    if self.layer_headers.is_dragging() {
                        let mut lh_actions = self.layer_headers.handle_drag_end(&mut self.tree);
                        actions.append(&mut lh_actions);
                    }
                    if self.layer_headers.is_gain_dragging() {
                        let mut g_actions = self.layer_headers.handle_gain_drag_end();
                        actions.append(&mut g_actions);
                    }
                    // Fire the overlay end hooks now, but leave `drag_owner`
                    // set — the stash classification below (`should_stash_for
                    // _tracks`) needs it to route this terminal `DragEnd` to
                    // the timeline. The owner is cleared as the last step of
                    // this iteration, past the stash read (BUG-075).
                    self.fire_gesture_end_hooks();
                }
                _ => {}
            }

            // Stash for `InteractionOverlay` (tracks-area events).
            let stash = self.should_stash_for_tracks(event);
            if manifold_ui::input::input_trace_enabled()
                && matches!(event, UIEvent::DragBegin { .. } | UIEvent::DragEnd { .. })
            {
                eprintln!(
                    "[input-trace] ui_root: {} {} for timeline overlay (drag_owner={:?})",
                    trace_kind(event),
                    if stash { "STASHED" } else { "NOT stashed" },
                    self.drag_owner
                );
            }
            if stash {
                self.viewport_events.push(event.clone());
            }

            // Owner lifetime (BUG-075 / D2/§3.3): the stash read just above
            // still needs `drag_owner`, so the terminal clear happens HERE —
            // after both the fire-hooks (in the match arm) and the stash
            // classification — never earlier. This is the fix's whole point:
            // fire hooks, stash by owner, then clear. Re-folding the clear
            // into `fire_gesture_end_hooks` would reintroduce BUG-075.
            if matches!(event, UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. }) {
                self.drag_owner = None;
            }
        }

        // Intercept dropdown-triggering actions and open dropdowns here
        // (where we have access to the tree for node bounds).
        let popup_open_before = self.browser_popup.is_open();
        let mut filtered = Vec::with_capacity(actions.len());
        for action in actions {
            if self.try_open_dropdown(&action, last_click_node) {
                // Consumed — don't forward to dispatch.
                continue;
            }
            filtered.push(action);
        }

        // If a popup was just opened, flag for rebuild so nodes appear this frame.
        if !popup_open_before && (self.browser_popup.is_open() || self.ableton_picker.is_open()) {
            self.overlay_dirty = true;
        }

        filtered
    }

    /// If the action is a dropdown / context-menu / picker trigger, open the
    /// overlay anchored appropriately and return true (action consumed).
    /// Otherwise return false.
    ///
    /// Single source of truth for "an overlay just opened → mark `overlay_dirty`".
    /// Every open path (dropdowns, right-click context menus, the browser popup,
    /// the Ableton picker) flows through here, so flagging the dirty bit once on a
    /// `true` return guarantees the next build re-records the overlay into
    /// `overlay_draw` and it actually paints this interaction. The bare
    /// `open_context` arms used to forget this individually, which is exactly why
    /// right-click context menus were flaky: they drew only when some *unrelated*
    /// state change happened to trigger a rebuild that same frame.
    fn try_open_dropdown(&mut self, action: &PanelAction, click_node: Option<NodeId>) -> bool {
        let opened = self.try_open_dropdown_inner(action, click_node);
        if opened {
            self.overlay_dirty = true;
        }
        opened
    }

    fn try_open_dropdown_inner(&mut self, action: &PanelAction, click_node: Option<NodeId>) -> bool {
        let right_click_pos = self.last_right_click_pos;
        let trigger = if let Some(node) = click_node {
            self.tree.get_bounds(node)
        } else {
            Rect::new(100.0, 100.0, 80.0, 24.0)
        };

        match action {
            PanelAction::BlendModeClicked(idx) => {
                use manifold_core::types::BlendMode;
                // Typed dropdown (2b.11): each item carries its own SetBlendMode
                // action, so selection fires it directly — no DropdownContext /
                // index→meaning map for blend modes.
                let items: Vec<DropdownItem> = BlendMode::ALL
                    .iter()
                    .map(|m| {
                        // The label is the display name; the action carries the
                        // Debug form, exactly as the old index→action map did
                        // (`format!("{:?}", BlendMode::from_index(i))`).
                        DropdownItem::new(m.display_name())
                            .with_action(PanelAction::SetBlendMode(idx.clone(), format!("{:?}", m)))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::ClipDetectQuantizeClicked => {
                // Typed (2b.11): each grid option carries its quantize step.
                let items: Vec<DropdownItem> =
                    manifold_core::audio_clip_detection::quantize_grid_options()
                        .iter()
                        .map(|(label, step)| {
                            DropdownItem::new(label)
                                .with_action(PanelAction::ClipDetectSetQuantize(*step))
                        })
                        .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::ClipDetectLayerClicked(idx) => {
                // "Auto" (route by trigger name) first, then every candidate
                // layer cached by state_sync — each carries its target layer.
                let mut items = Vec::with_capacity(self.clip_detect_layers.len() + 1);
                items.push(
                    DropdownItem::new("Auto")
                        .with_action(PanelAction::ClipDetectSetLayer(*idx, None)),
                );
                for (id, name) in &self.clip_detect_layers {
                    items.push(DropdownItem::new(name).with_action(
                        PanelAction::ClipDetectSetLayer(*idx, Some(id.clone())),
                    ));
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            // The Audio Setup Triggers matrix's target-layer dropdown
            // (`AudioTriggerLayerClicked` → `AudioTriggerSetLayer`) is deleted
            // with the matrix (P3, D2).
            PanelAction::AudioSendClicked(idx) => {
                // "No source" first, then every named send from Audio Setup so the
                // layer dropdown and the setup panel can never disagree — each
                // carries its SetLayerAudioSend directly.
                let sends = self.audio_setup_panel.send_options();
                let mut items = Vec::with_capacity(sends.len() + 1);
                items.push(
                    DropdownItem::new("No source")
                        .with_action(PanelAction::SetLayerAudioSend(idx.clone(), None)),
                );
                for (id, label) in sends {
                    items.push(
                        DropdownItem::new(&label)
                            .with_action(PanelAction::SetLayerAudioSend(idx.clone(), Some(id))),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            // `AudioSendAddLayerClicked` (Inputs section "+ Layer") is deleted
            // with the section's authoring (§7.2 item 7, P8, 2026-07-11) — the
            // layer header's own Send dropdown (`AudioSendClicked` above) is
            // the one surviving path to `SetLayerAudioSend`.
            PanelAction::AddEffectClicked(tab) => {
                use manifold_core::{preset_def::PresetKind, preset_type_registry};
                use manifold_ui::panels::browser_popup::*;

                // Effect mode keeps its existing "Project" category chip
                // grouping (`tag_project_category: true`).
                let mut items = self.build_preset_picker_items(PresetKind::Effect, true);
                let has_project_items =
                    items.iter().any(|it| it.category.as_deref() == Some("Project"));
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                // Unique category names (+ "Project" when embedded effects exist).
                let mut cat_names: Vec<String> = preset_type_registry::ALL_CATEGORIES
                    .iter()
                    .map(|&c| c.to_string())
                    .collect();
                if has_project_items {
                    cat_names.push("Project".to_string());
                }

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Effect,
                    tab: *tab,
                    layer_id: None,
                    items,
                    category_names: cat_names,
                    spawn_graph_pos: None,
                    paste_count: self.effect_clipboard_count,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::GenTypeClicked(layer_id) => {
                use manifold_core::preset_def::PresetKind;
                use manifold_ui::panels::browser_popup::*;

                // Generator mode has never rendered category chips (no
                // `category_names` below) — `tag_project_category: false`
                // keeps that unchanged; only the source classification is new.
                let mut items = self.build_preset_picker_items(PresetKind::Generator, false);
                items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Generator,
                    tab: InspectorTab::Layer,
                    layer_id: layer_id.clone(),
                    items,
                    category_names: Vec::new(),
                    spawn_graph_pos: None,
                    paste_count: 0,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::BrowserCellRightClicked(mode, type_id, source) => {
                use manifold_ui::panels::picker_core::Source;

                let mut items = Vec::new();
                items.push(
                    DropdownItem::new("Rename…").with_action(PanelAction::BrowserRenamePresetClicked(
                        *mode,
                        type_id.clone(),
                        *source,
                    )),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(
                        DropdownItem::new("Duplicate").with_action(
                            PanelAction::BrowserDuplicatePresetClicked(*mode, type_id.clone()),
                        ),
                    );
                }
                items.push(
                    DropdownItem::new("Delete…").with_action(PanelAction::BrowserDeletePresetClicked(
                        *mode,
                        type_id.clone(),
                        *source,
                    )),
                );
                if matches!(source, Source::MyLibrary) {
                    items.push(
                        DropdownItem::new("Reveal in Finder").with_action(
                            PanelAction::BrowserRevealPresetClicked(*mode, type_id.clone()),
                        ),
                    );
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::SelectAudioInputDevice => {
                // Enumerate audio input devices on demand; each item carries its
                // SetAudioInputDevice action ("" = none/video-only).
                let device_names: Vec<String> =
                    manifold_audio::capture::AudioCaptureDevice::list_devices()
                        .into_iter()
                        .map(|d| d.name)
                        .collect();
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("None (video only)")
                        .with_action(PanelAction::SetAudioInputDevice(String::new())),
                ];
                items.extend(device_names.into_iter().map(|name| {
                    DropdownItem::new(&name).with_action(PanelAction::SetAudioInputDevice(name))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            // `AudioSendRoutingsClicked` (the Cap chip's click-to-reveal
            // routings popup) is deleted (§7.2 item 7, P8, 2026-07-11) — its
            // content (device + feeding layers) lives in the Inputs
            // section's read-only routing display now, always visible, no
            // click needed.
            PanelAction::AudioSetupDeviceClicked => {
                // Enumerate input devices + tappable sources on demand for the
                // Audio Setup modal. The list is three sections: the default, the
                // hardware/virtual input devices, and the output taps (system +
                // running apps). A parallel choice map records what each row is so
                // selection doesn't depend on position.
                let dir = manifold_audio::directory::system_directory();
                self.audio_setup_devices = dir.list_input_devices();
                let caps = dir.tap_capabilities();
                self.audio_setup_apps =
                    if caps.app_audio { dir.list_audio_apps() } else { Vec::new() };

                // Typed (2b.11): each source row carries its AudioSetDevice action
                // built from the cached metadata; headers stay non-selectable.
                let mut items: Vec<DropdownItem> = Vec::new();

                items.push(
                    DropdownItem::new("System Default")
                        .with_action(PanelAction::AudioSetDevice(None)),
                );

                if !self.audio_setup_devices.is_empty() {
                    items.push(DropdownItem::disabled("Input Devices"));
                    for d in &self.audio_setup_devices {
                        // Mark an offline device so a stale routing reads clearly.
                        let label = if d.is_alive {
                            d.name.clone()
                        } else {
                            format!("{} (offline)", d.name)
                        };
                        // Store stable UID + display name from the cached metadata.
                        let action = PanelAction::AudioSetDevice(Some(
                            manifold_ui::AudioDeviceRef::new(d.uid.clone(), d.name.clone()),
                        ));
                        items.push(DropdownItem::new(&label).with_action(action));
                    }
                }

                if caps.system_audio || caps.app_audio {
                    items.push(DropdownItem::disabled("Capture Output"));
                    if caps.system_audio {
                        items.push(DropdownItem::new("System Audio").with_action(
                            PanelAction::AudioSetDevice(Some(
                                manifold_ui::AudioDeviceRef::system_audio(),
                            )),
                        ));
                    }
                    for app in &self.audio_setup_apps {
                        // A backgrounded/idle app is still selectable; it just
                        // produces silence until it plays. Persist the stable bundle
                        // id + display name; the runtime re-resolves at capture time.
                        let label = if app.is_alive {
                            app.name.clone()
                        } else {
                            format!("{} (idle)", app.name)
                        };
                        let action = PanelAction::AudioSetDevice(Some(
                            manifold_ui::AudioDeviceRef::app(
                                app.bundle_id.clone(),
                                app.name.clone(),
                            ),
                        ));
                        items.push(DropdownItem::new(&label).with_action(action));
                    }
                }

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::AudioSendChannelClicked(send_id) => {
                // A tap source (system / app output) is a fixed stereo mixdown, so
                // it has no hardware channel layout — present Left/Right. A device
                // source builds its true layout, grouped by subdevice, with
                // platform channel names. Each row carries its typed channel action
                // (2b.11) — the list itself enumerates stereo pairs AND single
                // channels (§7.2 item 7, P8), so mono is just picking one.
                let items = if self
                    .audio_setup_panel
                    .current_device()
                    .is_some_and(|d| d.is_tap())
                {
                    build_tap_channel_dropdown(send_id)
                } else {
                    let dir = manifold_audio::directory::system_directory();
                    let device = match self.audio_setup_panel.current_device() {
                        Some(dev_ref) => dir.resolve(dev_ref.uid_opt(), Some(&dev_ref.name)),
                        // No explicit device → the system default input.
                        None => dir.list_input_devices().into_iter().find(|d| d.is_default),
                    };
                    build_channel_dropdown(device.as_ref(), send_id)
                };
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::SelectClkDevice => {
                if self.midi_device_names.is_empty() {
                    log::info!("[UIRoot] No MIDI devices available for CLK selection");
                    return false;
                }
                // Typed (2b.11): item i carries SetMidiClockDevice(i).
                let items: Vec<DropdownItem> = self
                    .midi_device_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        DropdownItem::new(name)
                            .with_action(PanelAction::SetMidiClockDevice(i as i32))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MidiInputClicked(idx) => {
                // Typed dropdown (2b.11): item n carries SetMidiNote(idx, n).
                let items: Vec<DropdownItem> = (0..128)
                    .map(|n| {
                        DropdownItem::new(&manifold_core::midi::note_number_to_name(n))
                            .with_action(PanelAction::SetMidiNote(idx.clone(), n))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MidiChannelClicked(idx) => {
                // "All" (-1) then channels 0..15 (displayed 1..16).
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All").with_action(PanelAction::SetMidiChannel(idx.clone(), -1)),
                ];
                items.extend((1..=16).map(|ch| {
                    DropdownItem::new(&format!("Ch {}", ch))
                        .with_action(PanelAction::SetMidiChannel(idx.clone(), ch - 1))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MidiDeviceClicked(idx) => {
                // "All Devices" (None) then each named device.
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All Devices")
                        .with_action(PanelAction::SetMidiDevice(idx.clone(), None)),
                ];
                items.extend(self.midi_device_names.iter().map(|name| {
                    DropdownItem::new(name)
                        .with_action(PanelAction::SetMidiDevice(idx.clone(), Some(name.clone())))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::ResolutionClicked => {
                use manifold_core::types::ResolutionPreset;
                let has_displays = !self.display_resolutions.is_empty();

                // Typed dropdown (2b.11): each item carries its own action.
                let mut items: Vec<DropdownItem> = ResolutionPreset::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        DropdownItem::new(&r.dropdown_label())
                            .with_action(PanelAction::SetResolution(i))
                    })
                    .collect();

                // Add display resolutions below presets (Unity: Footer.CollectDisplayResolutions)
                if has_displays {
                    // Separator label (disabled, non-selectable) — matches Unity format
                    items.push(DropdownItem::disabled("---  Displays  ---"));
                    for (w, h, label) in &self.display_resolutions {
                        items.push(
                            DropdownItem::new(&format!("{}  ({}x{})", label, w, h)).with_action(
                                PanelAction::SetDisplayResolution(*w as i32, *h as i32),
                            ),
                        );
                    }
                }

                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MasterExitPathClicked => {
                // Typed (2b.11): "After All FX" → exit -1, "Before FX" → 0, then each
                // effect → its 1-based exit index.
                let mut items = vec![
                    DropdownItem::new("After All FX")
                        .with_action(PanelAction::SetLedExitIndex(-1)),
                    DropdownItem::new("Before FX").with_action(PanelAction::SetLedExitIndex(0)),
                ];
                for (e, name) in self.master_effect_names.iter().enumerate() {
                    items.push(
                        DropdownItem::new(&format!("After {}", name))
                            .with_action(PanelAction::SetLedExitIndex(e as i32 + 1)),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::ClipRightClicked(clip_id) => {
                // Typed (2b.11): each item carries its clip action.
                let items = vec![
                    DropdownItem::new("Split at Playhead")
                        .with_action(PanelAction::ContextSplitAtPlayhead(clip_id.clone())),
                    DropdownItem::new("Delete")
                        .with_action(PanelAction::ContextDeleteClip(clip_id.clone())),
                    DropdownItem::new("Duplicate")
                        .with_action(PanelAction::ContextDuplicateClip(clip_id.clone())),
                ];
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::TrackRightClicked(beat, layer) => {
                // Typed (2b.11): each item carries its track action. Paste stays
                // index-based (`ContextPasteAtTrack` targets the clicked track
                // slot, not a specific layer's identity) but the Context*Layer
                // family is LayerId-keyed (BUG-031) — resolve the row-under-
                // cursor's id once, synchronously, same as the layer-header menu.
                let mut items = vec![
                    DropdownItem::new("Paste")
                        .with_action(PanelAction::ContextPasteAtTrack(*beat, *layer)),
                ];
                if let Some(layer_id) = self
                    .layer_headers
                    .layer_info(*layer)
                    .map(|info| manifold_core::LayerId::new(&info.layer_id))
                {
                    items.push(
                        DropdownItem::new("Import MIDI File")
                            .with_action(PanelAction::ContextImportMidi(layer_id.clone())),
                    );
                    items.push(
                        DropdownItem::new("Insert Video Layer")
                            .with_action(PanelAction::ContextAddVideoLayer(layer_id.clone())),
                    );
                    items.push(
                        DropdownItem::new("Insert Generator Layer")
                            .with_action(PanelAction::ContextAddGeneratorLayer(layer_id.clone())),
                    );
                    items.push(
                        DropdownItem::new("Insert Audio Layer")
                            .with_action(PanelAction::ContextAddAudioLayer(layer_id)),
                    );
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            // BUG-184: `ClearLaneCommand`/`RemoveLaneCommand` had no UI
            // callers. Two-item menu, same typed with_action shape as
            // ClipRightClicked/TrackRightClicked above.
            PanelAction::AutomationLaneRightClicked(target, param_id) => {
                let items = vec![
                    DropdownItem::new("Clear Automation").with_action(
                        PanelAction::ContextClearAutomationLane(target.clone(), param_id.clone()),
                    ),
                    DropdownItem::new("Remove Lane").with_action(
                        PanelAction::ContextRemoveAutomationLane(target.clone(), param_id.clone()),
                    ),
                ];
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::LayerHeaderRightClicked(layer_id) => {
                // The action carries a stable LayerId; every context-menu item
                // below carries that same id (not a resolved row index), so a
                // layer-list change between menu-open and item-click can't make
                // an item address the wrong layer — see BUG-031. `li` is used
                // only for the synchronous, read-only display decisions below
                // (is_group / can_group), which are inherently open-time snapshots.
                let Some(li) = self.layer_headers.index_of_layer(layer_id) else {
                    return true;
                };
                let layer_info = self.layer_headers.layer_info(li);
                let is_group = layer_info.is_some_and(|l| l.is_group);
                let mut items = vec![
                    DropdownItem::new("Paste")
                        .with_action(PanelAction::ContextPasteAtLayer(layer_id.clone())),
                ];
                if !is_group {
                    items.push(
                        DropdownItem::new("Import MIDI File")
                            .with_action(PanelAction::ContextImportMidi(layer_id.clone())),
                    );
                }
                items.push(
                    DropdownItem::new("Insert Video Layer")
                        .with_action(PanelAction::ContextAddVideoLayer(layer_id.clone())),
                );
                items.push(
                    DropdownItem::new("Insert Generator Layer")
                        .with_action(PanelAction::ContextAddGeneratorLayer(layer_id.clone())),
                );
                items.push(
                    DropdownItem::new("Insert Audio Layer")
                        .with_action(PanelAction::ContextAddAudioLayer(layer_id.clone())),
                );
                items.push(
                    DropdownItem::new("Duplicate Layer")
                        .with_action(PanelAction::ContextDuplicateLayer(layer_id.clone())),
                );
                // "Group" only when 2+ non-group, non-nested layers are selected
                let can_group = self.layer_headers.layer_count() >= 2 && !is_group;
                if can_group {
                    items.push(
                        DropdownItem::new("Group Selected Layers")
                            .with_action(PanelAction::ContextGroupSelectedLayers),
                    );
                }
                if is_group {
                    items.push(
                        DropdownItem::new("Ungroup")
                            .with_action(PanelAction::ContextUngroup(layer_id.clone())),
                    );
                }
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(
                        DropdownItem::new("Delete Layer")
                            .with_action(PanelAction::ContextDeleteLayer(layer_id.clone())),
                    );
                }
                // Last text item gets a separator before the color grid
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                self.dropdown_context = Some(DropdownContext::LayerContext(layer_id.clone()));
                self.dropdown.open_context_with_colors(
                    items,
                    manifold_ui::color::COLOR_GRID.to_vec(),
                    manifold_ui::color::COLOR_GRID_COLS,
                    right_click_pos,
                    &mut self.tree,
                );
                true
            }
            PanelAction::ParamLabelRightClick(gpt, param_id) => {
                let mut items = Vec::with_capacity(manifold_core::MACRO_COUNT + 3);
                for i in 0..manifold_core::MACRO_COUNT {
                    let label = {
                        let slot = &self.macro_labels[i];
                        if slot.is_empty() {
                            format!("Map to Macro {}", i + 1)
                        } else {
                            format!("Map to Macro {} ({})", i + 1, slot)
                        }
                    };
                    // Typed (2b.11): item i maps the param to macro i.
                    items.push(DropdownItem::new(&label).with_action(
                        PanelAction::MapParamToMacro(*gpt, param_id.clone(), i),
                    ));
                }
                // Ableton picker entry
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                let ableton_connected = self.ableton_session.as_ref().is_some_and(|s| s.connected);
                if ableton_connected {
                    items.push(DropdownItem::new("Map to Ableton Macro…").with_action(
                        PanelAction::OpenAbletonPickerForParam(*gpt, param_id.clone()),
                    ));
                } else {
                    items.push(DropdownItem::disabled("Ableton not connected"));
                }
                // "Remove Ableton Mapping" when param is already mapped — the
                // only kind-specific read; the menu + context are unified.
                let is_ableton_mapped = match gpt {
                    GraphParamTarget::Effect(fx_idx) => self.inspector.is_effect_ableton_mapped(
                        self.inspector.last_effect_tab(),
                        *fx_idx,
                        param_id.as_ref(),
                    ),
                    GraphParamTarget::Generator => {
                        self.inspector.is_gen_ableton_mapped(param_id.as_ref())
                    }
                };
                if is_ableton_mapped {
                    items.push(DropdownItem::new("Remove Ableton Mapping").with_action(
                        PanelAction::UnmapParamAbleton(*gpt, param_id.clone()),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::MacroLabelRightClick(macro_idx) => {
                // Typed (2b.11): rename, each mapping (unmap), Clear All, and the
                // Ableton entries each carry their own action.
                let descs = &self.macro_mapping_descs[*macro_idx];
                let rename = DropdownItem::new("Rename")
                    .with_action(PanelAction::MacroLabelRename(*macro_idx))
                    .with_separator();
                let mut items = vec![rename];
                if descs.is_empty() {
                    let mut item = DropdownItem::new("No mappings");
                    item.enabled = false;
                    items.push(item);
                } else {
                    for (i, desc) in descs.iter().enumerate() {
                        items.push(
                            DropdownItem::new(desc)
                                .with_action(PanelAction::UnmapMacro(*macro_idx, i)),
                        );
                    }
                    if descs.len() > 1 {
                        if let Some(last) = items.last_mut() {
                            last.separator_after = true;
                        }
                        items.push(
                            DropdownItem::new("Clear All")
                                .with_action(PanelAction::ClearMacroMappings(*macro_idx)),
                        );
                    }
                }
                // Ableton section — same pattern as effect/gen param dropdowns
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                if self.ableton_session.is_some() {
                    items.push(DropdownItem::new("Map to Ableton Macro\u{2026}").with_action(
                        PanelAction::OpenAbletonPickerForMacro(*macro_idx),
                    ));
                } else {
                    let mut item = DropdownItem::new("Ableton not connected");
                    item.enabled = false;
                    items.push(item);
                }
                // "Remove Ableton Mapping" if this macro is mapped
                if self.macro_ableton_mapped[*macro_idx] {
                    items.push(DropdownItem::new("Remove Ableton Mapping").with_action(
                        PanelAction::UnmapMacroAbleton(*macro_idx),
                    ));
                }
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::CardRightClicked(gpt) => {
                // Generators carry Copy/Paste (their own clipboard); both kinds
                // share Make Unique / Export / Import. The menu CONTENTS differ
                // per kind by design (the legitimately-divergent shell); the
                // fork actions + their dispatch are one path keyed by `gpt`.
                // Typed (2b.11): each item carries its action keyed by the card's
                // target, so the dispatch runs one path for effects + generators.
                let mut items = Vec::new();
                if matches!(gpt, GraphParamTarget::Generator) {
                    items.push(
                        DropdownItem::new("Copy Generator")
                            .with_action(PanelAction::CopyGenerator),
                    );
                    if self.gen_clipboard.has_content() {
                        items.push(
                            DropdownItem::new("Paste Generator")
                                .with_action(PanelAction::PasteGenerator),
                        );
                    }
                }
                items.push(
                    DropdownItem::new("Make Unique")
                        .with_action(PanelAction::MakePresetUnique(*gpt)),
                );
                // Divergence actions (PRESET_LIBRARY_DESIGN D3, P4): only
                // meaningful once the instance has diverged from its library
                // entry (`graph.is_some()`) — reuse the retained card's own
                // `has_graph_mod` bit (the exact source the MOD badge reads),
                // same tab-resolution `is_effect_ableton_mapped` above uses,
                // so there's one source of truth for "is this card diverged"
                // rather than a second computation.
                let has_graph_mod = match gpt {
                    GraphParamTarget::Effect(fx_idx) => {
                        self.inspector.effect_has_graph_mod(self.inspector.last_effect_tab(), *fx_idx)
                    }
                    GraphParamTarget::Generator => self.inspector.gen_has_graph_mod(),
                };
                if has_graph_mod {
                    items.push(
                        DropdownItem::new("Revert to Library")
                            .with_action(PanelAction::RevertToLibrary(*gpt)),
                    );
                    // Wording states the blast radius WITHOUT computing it
                    // (PRESET_LIBRARY_DESIGN §4/§6: counting how many
                    // instances track an id is the forbidden machinery this
                    // design deletes) — "instances", not a computed N.
                    items.push(
                        DropdownItem::new("Push to Library — updates instances tracking this preset")
                            .with_action(PanelAction::PushToLibrary(*gpt)),
                    );
                }
                // Library doors (PRESET_LIBRARY_DESIGN D4) — explicit "publish a
                // copy" actions, distinct from Make Unique's divergence/retarget.
                items.push(
                    DropdownItem::new("Save to Library…")
                        .with_action(PanelAction::SaveToLibrary(*gpt)),
                );
                items.push(
                    DropdownItem::new("Save to Project…")
                        .with_action(PanelAction::SaveToProject(*gpt)),
                );
                items.push(
                    DropdownItem::new("Export Preset…").with_action(PanelAction::ExportPreset(*gpt)),
                );
                items.push(
                    DropdownItem::new("Import Preset…").with_action(PanelAction::ImportPreset(*gpt)),
                );
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::OpenAbletonPickerForParam(gpt, param_id) => {
                use manifold_ui::panels::ableton_picker::AbletonPickerContext;
                if let Some(session) = &self.ableton_session {
                    // Carry the unified target straight through. The mapping
                    // target + inspector tab are resolved at dispatch time, the
                    // same path the unmap action uses — no kind fork here.
                    self.ableton_picker_context = Some(AbletonPickerContext::Param {
                        gpt: *gpt,
                        param_id: param_id.clone(),
                    });
                    self.ableton_picker
                        .open(build_picker_session(session), right_click_pos);
                    self.overlay_dirty = true;
                    self.ableton_rediscovery_needed = true;
                }
                true
            }
            PanelAction::OpenAbletonPickerForMacro(slot_idx) => {
                use manifold_ui::panels::ableton_picker::AbletonPickerContext;
                if let Some(session) = &self.ableton_session {
                    self.ableton_picker_context = Some(AbletonPickerContext::MacroSlot {
                        slot_idx: *slot_idx,
                    });
                    self.ableton_picker
                        .open(build_picker_session(session), right_click_pos);
                    self.overlay_dirty = true;
                    self.ableton_rediscovery_needed = true;
                }
                true
            }
            // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P4, D9: click on a 3+-label
            // enum value cell in the Scene Setup dock opens the shared
            // dropdown — items = the row's label set, checked at the current
            // index, each carrying the SAME `SceneSetupParamChanged` write
            // the dock's steppers already dispatch (no new mutation path).
            // `cell_node_id` resolves the anchor directly (the panel has no
            // `&UITree` in `handle_event` to compute it itself).
            PanelAction::SceneSetupEnumClicked {
                layer_id,
                scope_path,
                node_doc_id,
                param_id,
                labels,
                current_index,
                cell_node_id,
            } => {
                let cell_trigger = self.tree.get_bounds(*cell_node_id);
                let items: Vec<DropdownItem> = labels
                    .iter()
                    .enumerate()
                    .map(|(i, label)| {
                        DropdownItem::new(label)
                            .with_check(i as u32 == *current_index)
                            .with_action(PanelAction::SceneSetupParamChanged(
                                layer_id.clone(),
                                scope_path.clone(),
                                *node_doc_id,
                                param_id.clone(),
                                i as f32,
                            ))
                    })
                    .collect();
                self.open_dropdown_typed(items, cell_trigger);
                true
            }
            // SCENE_PANEL_UX_DESIGN.md UX-P2, D6: the "+ Add Modifier"
            // button opens the shared dropdown listing the SAME curated
            // vocabulary the old 7-chip grid did — each item dispatches the
            // SAME `SceneSetupAddModifier` action the chips fired directly.
            // `button_node_id` resolves the anchor directly, same
            // resolve-at-open convention as `SceneSetupEnumClicked` above.
            PanelAction::SceneSetupAddModifierClicked(layer_id, group_node_id, button_node_id) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                let items: Vec<DropdownItem> = manifold_ui::panels::scene_setup_panel::MESH_MODIFIER_CHOICES
                    .iter()
                    .map(|(label, type_id)| {
                        DropdownItem::new(label).with_action(PanelAction::SceneSetupAddModifier(
                            layer_id.clone(),
                            *group_node_id,
                            (*type_id).to_string(),
                        ))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            _ => false,
        }
    }

    // dropdown_to_action removed (2b.11): every selectable dropdown item now
    // carries its own PanelAction via DropdownItem::with_action and fires
    // DropdownAction::SelectedAction directly. The only surviving DropdownContext
    // is LayerContext (its color swatches, handled below), which doesn't map a
    // positional text Selected(index) either.

    /// Convert a color swatch selection into the appropriate PanelAction.
    fn dropdown_color_to_action(
        &self,
        ctx: DropdownContext,
        color_idx: usize,
    ) -> Option<PanelAction> {
        match ctx {
            DropdownContext::LayerContext(layer_id) => {
                let color = manifold_ui::color::COLOR_GRID.get(color_idx)?;
                Some(PanelAction::ContextSetLayerColor(layer_id, *color))
            }
        }
    }

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
    ///
    /// Mutually exclusive with the Scene Setup dock (`SCENE_SETUP_PANEL_DESIGN`
    /// D2): opening this one always closes that one first — a plain either/or
    /// toggle, both docks' header buttons stay present regardless.
    pub fn toggle_audio_dock(&mut self) {
        self.audio_setup_panel.toggle();
        let open = self.audio_setup_panel.is_open();
        self.layout.audio_setup_width = if open {
            manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH
        } else {
            0.0
        };
        if open && self.scene_setup_panel.is_open() {
            self.scene_setup_panel.close();
            self.layout.scene_setup_width = 0.0;
        }
        self.header.set_dock_toggle_state(open, self.scene_setup_panel.is_open());
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

    // ── Scene Setup dock resize (SCENE_SETUP_PANEL_DESIGN D2) — mirror of
    // the Audio Setup pair above ──
    const SCENE_SETUP_MIN_W: f32 = manifold_ui::color::MIN_SCENE_SETUP_WIDTH;
    const SCENE_SETUP_MAX_W: f32 = manifold_ui::color::MAX_SCENE_SETUP_WIDTH;

    /// True if `pos` is near the Scene Setup dock's LEFT edge. False when the
    /// dock is closed (zero width).
    pub fn is_near_scene_setup_edge(&self, pos: Vec2) -> bool {
        if self.layout.scene_setup_width <= 0.0 {
            return false;
        }
        let dock = self.layout.scene_setup();
        (pos.x - dock.x).abs() < Self::RESIZE_EDGE_PX && pos.y >= dock.y && pos.y <= dock.y + dock.height
    }

    /// Begin a Scene Setup dock resize drag.
    pub fn begin_scene_setup_resize(&mut self, x: f32) {
        self.scene_setup_resize_dragging = true;
        self.scene_setup_drag_start_x = x;
        self.scene_setup_drag_start_width = self.layout.scene_setup_width;
    }

    /// Update dock width during resize. Returns true if width moved.
    pub fn update_scene_setup_resize(&mut self, x: f32) -> bool {
        if !self.scene_setup_resize_dragging {
            return false;
        }
        let delta = x - self.scene_setup_drag_start_x;
        let new_width = (self.scene_setup_drag_start_width - delta)
            .clamp(Self::SCENE_SETUP_MIN_W, Self::SCENE_SETUP_MAX_W);
        if (new_width - self.layout.scene_setup_width).abs() > 1.0 {
            self.layout.scene_setup_width = new_width;
            true
        } else {
            false
        }
    }

    /// End Scene Setup dock resize drag.
    pub fn end_scene_setup_resize(&mut self) {
        self.scene_setup_resize_dragging = false;
    }

    /// Toggle the Scene Setup dock — mirror of [`Self::toggle_audio_dock`],
    /// including the mutual-exclusion write-back.
    pub fn toggle_scene_dock(&mut self) {
        self.scene_setup_panel.toggle();
        let open = self.scene_setup_panel.is_open();
        self.layout.scene_setup_width =
            if open { manifold_ui::color::DEFAULT_SCENE_SETUP_WIDTH } else { 0.0 };
        if open && self.audio_setup_panel.is_open() {
            self.audio_setup_panel.close();
            self.layout.audio_setup_width = 0.0;
        }
        self.header.set_dock_toggle_state(self.audio_setup_panel.is_open(), open);
    }

    /// Scene Setup dock resize-handle colour feedback (idle/hover/drag).
    pub fn set_scene_setup_handle_hover(&mut self) {
        self.set_handle_color(self.scene_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_HOVER);
    }
    pub fn set_scene_setup_handle_drag(&mut self) {
        self.set_handle_color(self.scene_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_DRAG);
    }
    pub fn set_scene_setup_handle_idle(&mut self) {
        self.set_handle_color(self.scene_setup_handle_id, manifold_ui::color::RESIZE_HANDLE_IDLE);
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

    /// Drain viewport-area events stashed during process_events().
    /// App.rs routes these through the InteractionOverlay with a host trait.
    pub fn drain_viewport_events(&mut self) -> Vec<manifold_ui::input::UIEvent> {
        std::mem::take(&mut self.viewport_events)
    }

    /// Filter overlay-generated actions (TrackRightClicked, ClipRightClicked)
    /// through the dropdown system. Called by app.rs after the overlay processes
    /// viewport events — these actions are generated AFTER process_events()
    /// returns, so they need a second pass through try_open_dropdown.
    pub fn intercept_overlay_actions(&mut self, actions: &mut Vec<PanelAction>) {
        actions.retain(|action| !self.try_open_dropdown(action, None));
    }

    /// Whether `event` should stash into `viewport_events` for
    /// `InteractionOverlay` (`docs/DRAG_CAPTURE_DESIGN.md` §3.2). The drag
    /// family (`DragBegin`/`Drag`/`DragEnd`) stashes by OWNERSHIP,
    /// unconditionally, no position check — `resolve_drag_owner` fixes
    /// `drag_owner` on `DragBegin` (see the `process_events` drag loop), and
    /// it persists across frames for `Drag`/`DragEnd`. This is what makes a
    /// timeline drag released outside `tracks_rect` (e.g. over the inspector)
    /// still reach `InteractionOverlay::on_end_drag` — today's positional
    /// gate would have dropped it. Every other event kind keeps the
    /// positional classification (`is_event_in_tracks_area`).
    fn should_stash_for_tracks(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        match event {
            UIEvent::DragBegin { .. } | UIEvent::Drag { .. } | UIEvent::DragEnd { .. } => {
                self.drag_owner == Some(DragOwner::TimelineTracks)
            }
            _ => self.is_event_in_tracks_area(event),
        }
    }

    /// Check if a UI event's position falls within the tracks area. The drag
    /// family (`DragBegin`/`Drag`/`DragEnd`) no longer classifies here — it
    /// stashes by ownership instead (`should_stash_for_tracks`). This keeps
    /// only the positional classification for discrete/non-drag events.
    fn is_event_in_tracks_area(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        let pos = match event {
            UIEvent::Click { pos, .. } => *pos,
            UIEvent::DoubleClick { pos, .. } => *pos,
            UIEvent::RightClick { pos, .. } => *pos,
            UIEvent::HoverEnter { pos, .. } => *pos,
            UIEvent::PointerDown { pos, .. } => *pos,
            _ => return false,
        };
        self.viewport.tracks_rect().contains(pos)
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

    /// Per-frame update — push state changes to panels.
    pub fn update(&mut self) {
        if !self.built {
            return;
        }
        // P2 "panel-split snap-back" (D15): advance the two splits'
        // double-click-reset tweens. `min(100.0)` matches
        // `InspectorCompositePanel::update`'s own dt clamp (a stall/debugger
        // pause must not fling the tween in one giant step). The app layer
        // (`app_render.rs`, mirroring its `drawer_anim_active` poll) reads
        // `layout.is_split_reset_animating()` after this call and forces the
        // rebuild that re-lays-out every panel from the eased ratio/width.
        let split_dt_ms = (self.layout_tick_last.elapsed().as_secs_f32() * 1000.0).min(100.0);
        self.layout_tick_last = std::time::Instant::now();
        self.layout.tick_splits(split_dt_ms);
        self.transport.update(&mut self.tree);
        self.header.update(&mut self.tree);
        self.footer.update(&mut self.tree);
        self.layer_headers.update(&mut self.tree);
        self.tick_inspector();
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
        // `UI_CRAFT_AND_MOTION_PLAN.md` §5 item 4: "universal popup enter").
        self.ableton_picker.update(&mut self.tree);
        self.browser_popup.update(&mut self.tree);
        self.settings_popup.update(&mut self.tree);
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
    }

    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 5):
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

    // `update_audio_trigger_levels` (the matrix's per-row trigger meter
    // driver) is deleted with the matrix (P3, D2). The D6 fire meter that
    // replaces it lives in the audio-mod drawer — deferred to a follow-up
    // phase (see this phase's landing notes).
}

/// The `AudioSetSendChannels` action for an explicit channel set (§7.2 item 7,
/// P8, 2026-07-11: the channel dropdown carries stereo pairing directly now —
/// no separate St/Mo toggle, mono falls out of picking a single channel).
fn send_channels_action(send_id: &manifold_core::AudioSendId, channels: Vec<u16>) -> PanelAction {
    PanelAction::AudioSetSendChannels(send_id.clone(), channels)
}

/// Push one channel run's rows: a "A + B" stereo-pair item for each adjacent
/// pair, immediately followed by each channel's own single-channel item — so
/// "Left + Right", "Left", "Right" (or "Ch 3+4", "Ch 3", "Ch 4" for unnamed
/// channels) read as one group. An odd channel out at the end of the run gets
/// only its single item (no pair to offer). Shared by the tap and device
/// dropdown builders so the pairing convention can't drift between them.
fn push_channel_pair_rows(
    items: &mut Vec<DropdownItem>,
    send_id: &manifold_core::AudioSendId,
    chans: &[manifold_audio::directory::ChannelInfo],
) {
    let mut i = 0;
    while i < chans.len() {
        if i + 1 < chans.len() {
            let (a, b) = (&chans[i], &chans[i + 1]);
            items.push(
                DropdownItem::new(&format!("{} + {}", a.display_name(), b.display_name()))
                    .with_action(send_channels_action(send_id, vec![a.index, b.index])),
            );
            items.push(
                DropdownItem::new(&a.display_name())
                    .with_action(send_channels_action(send_id, vec![a.index])),
            );
            items.push(
                DropdownItem::new(&b.display_name())
                    .with_action(send_channels_action(send_id, vec![b.index])),
            );
            i += 2;
        } else {
            let a = &chans[i];
            items.push(
                DropdownItem::new(&a.display_name())
                    .with_action(send_channels_action(send_id, vec![a.index])),
            );
            i += 1;
        }
    }
}

/// Channel dropdown for a tap source. Output taps are a fixed stereo mixdown —
/// "Left + Right", "Left", "Right".
fn build_tap_channel_dropdown(send_id: &manifold_core::AudioSendId) -> Vec<DropdownItem> {
    let chans = [
        manifold_audio::directory::ChannelInfo { index: 0, name: Some("Left".into()) },
        manifold_audio::directory::ChannelInfo { index: 1, name: Some("Right".into()) },
    ];
    let mut items = Vec::new();
    push_channel_pair_rows(&mut items, send_id, &chans);
    items
}

/// Build the send-channel dropdown for `device`, grouped by subdevice with
/// platform channel names; each subdevice's channels get stereo-pair rows
/// ("A + B") followed by their single-channel rows, non-selectable headers
/// between groups. Falls back to a single mono entry when no device metadata
/// is available.
fn build_channel_dropdown(
    device: Option<&manifold_audio::directory::DeviceInfo>,
    send_id: &manifold_core::AudioSendId,
) -> Vec<DropdownItem> {
    let fallback = || {
        vec![DropdownItem::new("Channel 1").with_action(send_channels_action(send_id, vec![0]))]
    };
    let Some(device) = device else {
        return fallback();
    };
    if device.channels.is_empty() {
        return fallback();
    }

    let mut items = Vec::new();
    if device.subdevices.is_empty() {
        push_channel_pair_rows(&mut items, send_id, &device.channels);
    } else {
        for group in &device.subdevices {
            items.push(DropdownItem::disabled(&group.name));
            let end = group.channel_start.saturating_add(group.channel_count) as usize;
            let start = group.channel_start as usize;
            if let Some(chans) = device.channels.get(start..end.min(device.channels.len())) {
                push_channel_pair_rows(&mut items, send_id, chans);
            }
        }
    }
    items
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

/// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P2: `UIRoot::overlay_redraw_needed()`
/// unit tests — proves the aggregate's wiring to its one live member (the
/// toast; see that function's doc comment for why the popup professional
/// pass's now-permanently-`false` `is_animating()` stubs aren't OR-ed in).
#[cfg(test)]
mod overlay_redraw_needed_tests {
    use super::*;

    #[test]
    fn false_on_a_fresh_root_with_nothing_open() {
        let ui = UIRoot::new();
        assert!(!ui.overlay_redraw_needed());
    }

    /// The named per-member proof from the phase brief: an animating overlay
    /// (here, the toast just fired) flips the aggregate `true`.
    #[test]
    fn toast_animating_flips_the_aggregate_true() {
        let mut ui = UIRoot::new();
        assert!(!ui.overlay_redraw_needed(), "idle toast contributes nothing");

        ui.toast.show("Undo");
        assert!(
            ui.overlay_redraw_needed(),
            "a freshly-fired toast is still ramping in — the aggregate must \
             see it as needing a keepalive redraw"
        );
    }

    /// Once the toast's whole enter/hold/fade timeline elapses, the
    /// aggregate drops back to `false` — the keepalive isn't permanent.
    #[test]
    fn settles_back_to_false_once_the_toast_finishes() {
        let mut ui = UIRoot::new();
        ui.toast.show("Redo");
        assert!(ui.overlay_redraw_needed());

        // Comfortably longer than the toast's total enter+hold+fade timeline.
        ui.toast.tick(10_000.0);
        assert!(!ui.overlay_redraw_needed(), "toast settled — no more keepalive needed");
    }
}

#[cfg(test)]
mod drag_capture_tests {
    //! `docs/DRAG_CAPTURE_DESIGN.md` P1 unit tests. `UIRoot::new()` +
    //! `resize()` is enough scaffolding for these — `resize` runs `build()`
    //! (sets `built = true`, computes real `tracks_rect`/`ruler_rect` from
    //! the layout) without needing a live `Project` (the same sequencing
    //! `ui_snapshot/script.rs` uses before `sync_build`).
    use super::*;

    const W: f32 = 1536.0;
    const H: f32 = 1216.0;

    fn new_root() -> UIRoot {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        ui
    }

    fn center(r: Rect) -> Vec2 {
        Vec2::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
    }

    /// D4: a modal claims a drag unconditionally, regardless of where the
    /// drag originated — even far outside the modal's own rect.
    #[test]
    fn modal_overlay_claims_drag_unconditionally() {
        let mut ui = new_root();
        ui.settings_popup.open();
        assert!(matches!(ui.settings_popup.modality(), Modality::Modal { .. }));

        let far_away = Vec2::new(W - 1.0, H - 1.0);
        let owner = ui.resolve_drag_owner(far_away, None);
        assert_eq!(owner, Some(DragOwner::Overlay(OverlayId::Settings)));
    }

    /// D3: an open dropdown that does NOT claim a foreign drag is dismissed
    /// as a side effect of resolution — WITHOUT consuming — and ownership
    /// passes to the real owner underneath (here, the tracks area). This is
    /// the BUG-058 wedge fixed by construction: nothing ever routes the
    /// drag's terminal event to the dropdown for it to eat.
    #[test]
    fn dropdown_open_at_drag_start_dismisses_without_consuming_and_falls_through() {
        let mut ui = new_root();
        let mut scratch_tree = UITree::new();
        ui.dropdown.open(
            vec![],
            Rect::new(10.0, 10.0, 50.0, 20.0),
            50.0,
            &mut scratch_tree,
        );
        assert!(ui.dropdown.is_open());

        let tracks_origin = center(ui.viewport.tracks_rect());
        let owner = ui.resolve_drag_owner(tracks_origin, None);

        assert!(!ui.dropdown.is_open(), "foreign drag dismisses the dropdown");
        assert_eq!(
            owner,
            Some(DragOwner::TimelineTracks),
            "ownership passes to the real owner instead of being eaten"
        );
    }

    /// §3.2 order: Ruler wins over TimelineTracks when the origin is in the
    /// ruler rect; TimelineTracks wins when it's only in the tracks rect;
    /// neither wins when the origin is in open space (e.g. above the ruler).
    #[test]
    fn owner_resolution_order_ruler_before_tracks_before_none() {
        let mut ui = new_root();

        let ruler_origin = center(ui.viewport.ruler_rect());
        assert_eq!(ui.resolve_drag_owner(ruler_origin, None), Some(DragOwner::Ruler));

        let tracks_origin = center(ui.viewport.tracks_rect());
        assert_eq!(
            ui.resolve_drag_owner(tracks_origin, None),
            Some(DragOwner::TimelineTracks)
        );

        let dead_space = Vec2::new(-100.0, -100.0);
        assert_eq!(ui.resolve_drag_owner(dead_space, None), None);
    }

    /// §3.3 failure story (a): a `DragEnd` released outside `tracks_rect`
    /// (e.g. the cursor drifted over the inspector) must still stash for
    /// `InteractionOverlay::on_end_drag` — ownership decides, not position.
    /// The old `is_event_in_tracks_area` positional gate would have dropped
    /// this exact case (BUG-058's leak-adjacent failure mode).
    #[test]
    fn drag_end_stashes_by_ownership_regardless_of_release_position() {
        let mut ui = new_root();
        let far_outside = Vec2::new(ui.viewport.tracks_rect().x_max() + 500.0, -200.0);
        let drag_end = UIEvent::DragEnd { node_id: None, pos: far_outside };

        ui.drag_owner = Some(DragOwner::TimelineTracks);
        assert!(
            ui.should_stash_for_tracks(&drag_end),
            "TimelineTracks owns the gesture, so the release position is irrelevant"
        );

        ui.drag_owner = Some(DragOwner::Inspector);
        assert!(
            !ui.should_stash_for_tracks(&drag_end),
            "a DragEnd owned by someone else must not stash for the timeline"
        );

        ui.drag_owner = None;
        assert!(!ui.should_stash_for_tracks(&drag_end));
    }

    /// §3.3: the terminal broadcast always clears the owner, so the next
    /// gesture starts from a clean slate — this is what makes a drag
    /// released over the inspector followed immediately by a new drag on a
    /// second clip behave (the no-wedge proof `drag-clip-release-over-
    /// inspector.json` exercises end-to-end).
    #[test]
    fn broadcast_gesture_end_clears_owner() {
        let mut ui = new_root();
        ui.drag_owner = Some(DragOwner::TimelineTracks);
        ui.broadcast_gesture_end();
        assert_eq!(ui.drag_owner, None);
    }

    /// BUG-075 regression: a timeline drag driven through the REAL
    /// `process_events` path must stash its terminal `DragEnd` for
    /// `InteractionOverlay::on_end_drag` — which is the only thing that
    /// finalizes trim / marquee / move (commits undo, resets `drag_mode`).
    ///
    /// This drives the same seam the shipped bug lived in: the terminal
    /// broadcast used to null `drag_owner` BEFORE `should_stash_for_tracks`
    /// read it, so the `DragEnd` never reached `viewport_events` and the
    /// gesture never finalized. The pre-existing ownership tests set
    /// `drag_owner` by hand and called `should_stash_for_tracks` directly, so
    /// they never exercised the broadcast-before-stash ordering — which is
    /// exactly why the bug shipped. This test refuses to do that: it goes
    /// Down → Move-past-threshold → Up through `pointer_event`/`process_events`
    /// and asserts the drained events, so the ordering is under test.
    #[test]
    fn timeline_drag_end_reaches_viewport_events_through_process_events() {
        let mut ui = new_root();
        let origin = center(ui.viewport.tracks_rect());

        // Press inside the tracks area.
        ui.pointer_event(origin, PointerAction::Down, 0.0);
        let _ = ui.process_events();
        let _ = ui.drain_viewport_events(); // clear the PointerDown stash

        // Move well past DRAG_THRESHOLD_PX (4px) → DragBegin + Drag. The
        // DragBegin is where `resolve_drag_owner` fixes the owner.
        let moved = Vec2::new(origin.x + 40.0, origin.y + 6.0);
        ui.pointer_event(moved, PointerAction::Move, 0.02);
        let _ = ui.process_events();
        assert_eq!(
            ui.drag_owner,
            Some(DragOwner::TimelineTracks),
            "the tracks-area DragBegin must resolve ownership to TimelineTracks \
             (if this fails the input never emitted DragBegin, not the bug)"
        );
        let mid = ui.drain_viewport_events();
        assert!(
            mid.iter().any(|e| matches!(e, UIEvent::DragBegin { .. })),
            "the DragBegin must stash while the gesture is owned: {mid:?}"
        );

        // Release. The terminal DragEnd must stash for on_end_drag, and the
        // owner must be cleared afterward (self-heal invariant preserved).
        ui.pointer_event(moved, PointerAction::Up, 0.04);
        let _ = ui.process_events();
        let end = ui.drain_viewport_events();
        assert!(
            end.iter().any(|e| matches!(e, UIEvent::DragEnd { .. })),
            "BUG-075: the terminal DragEnd must reach viewport_events so \
             on_end_drag finalizes the gesture — pre-fix this vec has no \
             DragEnd because the broadcast nulled the owner first: {end:?}"
        );
        assert_eq!(
            ui.drag_owner, None,
            "the owner must be cleared once the terminal event is fully routed"
        );
    }

    /// D1/§3.5: full-stack proof that a PointerDown landing on the docked Audio
    /// Setup panel's Low/Mid crossover divider requests immediate drag for that
    /// press, so a 1px Move begins the drag immediately (not after the usual
    /// `DRAG_THRESHOLD_PX = 4.0`) and the following Drag reaches the panel as an
    /// `AudioCrossoverChanged` action. Drives the real entry points
    /// (`pointer_event` → `process_events`) so it exercises the exact
    /// docked-panel routing `process_events` does — the seam this phase built.
    #[test]
    fn divider_grab_requests_immediate_drag_and_one_pixel_move_yields_crossover_changed() {
        let mut ui = new_root();
        ui.audio_setup_panel.open();
        // Open the dock column so `ui.build()` builds it into `audio_setup()`.
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.configure(
            None,
            vec![manifold_ui::panels::audio_setup_panel::AudioSendRow {
                id: manifold_core::AudioSendId::new("s1"),
                label: "Audio 1".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: manifold_ui::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: Vec::new(),
                consumers: Vec::new(),
            }],
            None,
        );
        let (low_hz, mid_hz, fmin, fmax) = (200.0_f32, 2000.0_f32, 20.0_f32, 20_000.0_f32);
        ui.audio_setup_panel.set_scope_bands(low_hz, mid_hz, fmin, fmax);
        ui.build();

        let scope =
            ui.audio_setup_panel.scope_rect().expect("scope present once open, sent, and built");
        // Same log-scale mapping `AudioSetupPanel::scope_line_y` documents on
        // `set_scope_bands` — reproduced here from the public scope_rect() +
        // the bands just set, rather than reaching into the panel's private
        // hit-test math, so the test only depends on the panel's public
        // contract.
        let yn = (low_hz / fmin).log2() / (fmax / fmin).log2();
        let divider_y = scope.y + scope.height * (1.0 - yn);
        let origin = Vec2::new(scope.x + scope.width * 0.5, divider_y);

        ui.pointer_event(origin, PointerAction::Down, 0.0);
        let down_actions = ui.process_events();
        assert!(
            down_actions
                .iter()
                .any(|a| matches!(a, PanelAction::AudioCrossoverDragBegin)),
            "PointerDown on the divider should arm the band grab: {down_actions:?}"
        );
        assert!(ui.audio_setup_panel.is_dragging_band(), "divider grab should be armed");

        // First Move: only 1px past the origin. With the global threshold
        // (4px) this would NOT begin a drag — proving this requires the
        // wiring having actually called `request_immediate_drag` off the
        // PointerDown above.
        let move1 = Vec2::new(origin.x, origin.y + 1.0);
        ui.pointer_event(move1, PointerAction::Move, 0.01);
        let move1_actions = ui.process_events();
        assert!(
            ui.input.is_dragging(),
            "a 1px move on an immediate-drag press must begin the drag \
             immediately, not wait for DRAG_THRESHOLD_PX; actions: {move1_actions:?}"
        );

        // Second Move: now a Drag (not DragBegin) event fires and reaches the
        // panel as the crossover-changed action.
        let move2 = Vec2::new(origin.x, origin.y + 2.0);
        ui.pointer_event(move2, PointerAction::Move, 0.02);
        let move2_actions = ui.process_events();
        assert!(
            move2_actions
                .iter()
                .any(|a| matches!(a, PanelAction::AudioCrossoverChanged(BandDivider::Low, _))),
            "the Drag following the immediate DragBegin should yield an \
             AudioCrossoverChanged(Low, _) action: {move2_actions:?}"
        );
    }

    // P3 regression guard (buttons still need the ordinary 4px threshold) is
    // proved directly in `manifold-ui`'s `input.rs` test module — that's
    // where `DRAG_THRESHOLD`/`immediate_drag_armed` actually live, and it
    // already has a bare-button test fixture (`setup()`); see
    // `three_pixel_wiggle_without_immediate_drag_still_resolves_to_click` and
    // `request_immediate_drag_allows_one_pixel_move_to_begin_drag` there.

    /// One app frame's post-process rebuild: `overlay_dirty` (set whenever an
    /// overlay consumes an event) drives a VISUAL `rebuild_scroll_panels` — the
    /// exact path `app_render.rs` takes, which re-runs `build_overlays` and
    /// re-mints the Audio Setup chrome.
    fn settle(ui: &mut UIRoot) {
        if ui.overlay_dirty {
            ui.overlay_dirty = false;
            ui.rebuild_scroll_panels(ScrollDirty {
                visual: true,
                ..ScrollDirty::default()
            });
        }
    }

    fn open_audio_panel_with_send(ui: &mut UIRoot) {
        ui.audio_setup_panel.open();
        // Open the dock column too (the real toggle sets both — D1); without a
        // width the dock rect is ZERO and the body clips to nothing.
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.configure(
            None,
            vec![manifold_ui::panels::audio_setup_panel::AudioSendRow {
                id: manifold_core::AudioSendId::new("s1"),
                label: "Audio 1".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: manifold_ui::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: Vec::new(),
                consumers: Vec::new(),
            }],
            None,
        );
        ui.audio_setup_panel.set_scope_bands(200.0, 2000.0, 20.0, 20_000.0);
        ui.build();
    }

    /// THE double-click regression (the Audio Setup buttons-need-two-clicks bug).
    /// Drives TWO real clicks on the Floor `−` stepper through the exact app
    /// frame loop — Down → process_events → overlay-dirty rebuild → Up →
    /// process_events — and asserts each fires exactly one `AudioSendFloorStep`.
    ///
    /// Root cause it guards: the Audio Setup panel is the only overlay that
    /// consumes `PointerDown` (BUG-059 leak stopgap), so a press on one of its
    /// buttons marks the overlay dirty and rebuilds the tree BETWEEN press and
    /// release. `rebuild_scroll_panels` → `UITree::truncate_from` used to
    /// recompute `root_count` from the current root-parented survivors, which
    /// undercounts once a root has been reparented (the inspector wraps its
    /// subpanels under a ClipRegion) — so the rebuilt overlay chrome root got a
    /// DIFFERENT salt than at press, its `WidgetId` (and every child's) churned,
    /// and the release resolved to a different widget than the press → no
    /// `Click`. Fixed by salting truncate's root count from `root_minted`
    /// (mint-time parentage). Pre-fix this test sees `steps1 == 0`.
    #[test]
    fn floor_stepper_fires_on_a_single_click_across_overlay_rebuild() {
        let mut ui = new_root();
        open_audio_panel_with_send(&mut ui);

        let floor0 = ui
            .audio_setup_panel
            .floor_minus_id()
            .expect("floor stepper builds when a send is selected");
        let fb = ui.tree.get_bounds(floor0);
        assert_ne!(fb, Rect::ZERO, "floor button must be live in the built tree");
        let p = Vec2::new(fb.x + fb.width * 0.5, fb.y + fb.height * 0.5);
        let w_at_build = ui.tree.widget_of(floor0);

        // ── Click 1 ─────────────────────────────────────────────
        ui.pointer_event(p, PointerAction::Down, 0.0);
        let _ = ui.process_events();
        settle(&mut ui); // the consumed PointerDown rebuilt the overlay
        let w_after_rebuild = ui.audio_setup_panel.floor_minus_id().map(|n| ui.tree.widget_of(n));
        ui.pointer_event(p, PointerAction::Up, 0.05);
        let click1 = ui.process_events();
        settle(&mut ui);

        // ── Click 2 (same pixel) ────────────────────────────────
        ui.pointer_event(p, PointerAction::Down, 0.20);
        let _ = ui.process_events();
        settle(&mut ui);
        ui.pointer_event(p, PointerAction::Up, 0.25);
        let click2 = ui.process_events();

        let steps = |acts: &[PanelAction]| {
            acts.iter()
                .filter(|a| matches!(a, PanelAction::AudioSendFloorStep(..)))
                .count()
        };

        // The button's identity must survive the mid-click rebuild — the whole
        // failure was this WidgetId churning between press and release.
        assert_eq!(
            w_after_rebuild,
            Some(w_at_build),
            "the Floor button's WidgetId must survive the overlay rebuild that a \
             consumed PointerDown triggers (build={:?} after-rebuild={w_after_rebuild:?})",
            Some(w_at_build),
        );
        assert_eq!(
            steps(&click1),
            1,
            "the FIRST click on the Floor stepper must fire one step — a second \
             click should not be needed. click1={click1:?}"
        );
        assert_eq!(steps(&click2), 1, "the second click must also fire one step: click2={click2:?}");
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
    /// and a scroll-triggered partial rebuild in between — covers
    /// `build_overlays`'s per-overlay regions and the `truncate_from` /
    /// region-list pruning path `build_viewport_panels` exercises.
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

        // zoom: true forces `needs_layer_headers()` — the full
        // `tree.truncate_from(scroll_panels_start)` + `build_scroll_panels()`
        // path, not the scroll-only in-place fast path that skips
        // truncation (and so wouldn't exercise the region-list pruning at
        // all).
        ui.rebuild_scroll_panels(ScrollDirty { scroll_x: false, scroll_y: true, zoom: true, visual: false });
        assert!(
            ui.tree.all_roots_are_regions(),
            "a stray root survived a scroll-triggered partial rebuild"
        );
    }

    /// The editor window's `build_inspector_in_rect` entry point (D5: the
    /// mechanism is window-agnostic) is likewise fully regioned.
    #[test]
    fn inspector_in_rect_has_no_stray_roots() {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        ui.build_inspector_in_rect(Rect::new(0.0, 0.0, 400.0, H));
        assert!(
            ui.tree.all_roots_are_regions(),
            "build_inspector_in_rect left a stray root"
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
    /// `pub` on `ParamCardConfig`, so this fixture is a direct, honest
    /// construction of the same shape `ui_bridge::state_sync` builds from
    /// real project state.
    fn fixture_config(collapsed: bool) -> manifold_ui::ParamCardConfig {
        let n = 1;
        manifold_ui::ParamCardConfig {
            kind: manifold_ui::ParamCardKind::Effect,
            name: "Fixture".into(),
            params: vec![manifold_ui::ParamInfo {
                param_id: std::borrow::Cow::Borrowed("amount"),
                name: "Amount".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                whole_numbers: false,
                is_angle: false,
                exposed: true,
                is_toggle: false,
                is_trigger: false,
                is_trigger_gate: false,
                value_labels: None,
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: false,
                section: None,
            }],
            string_params: Vec::new(),
            collapsed,
            effect_index: 0,
            effect_id: manifold_core::EffectId::new("bug160-fixture"),
            enabled: true,
            supports_envelopes: true,
            has_drv: false,
            has_env: false,
            has_abl: false,
            has_graph_mod: false,
            layer_id: None,
            driver_active: vec![false; n],
            envelope_active: vec![false; n],
            trim_min: vec![0.0; n],
            trim_max: vec![1.0; n],
            target_norm: vec![1.0; n],
            env_decay: vec![1.0; n],
            driver_beat_div_idx: vec![-1; n],
            driver_waveform_idx: vec![-1; n],
            driver_reversed: vec![false; n],
            driver_dotted: vec![false; n],
            driver_triplet: vec![false; n],
            driver_free_period: vec![None; n],
            audio: Default::default(),
            automation_active: vec![false; n],
            automation_overridden: vec![false; n],
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
