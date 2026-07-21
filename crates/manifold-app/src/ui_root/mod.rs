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

mod dropdowns;
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


/// What the currently-open dropdown is selecting for.
#[derive(Debug, Clone)]
pub enum DropdownContext {
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


    /// Cache the candidate target layers for the audio-clip detection layer
    /// dropdowns. Set by `state_sync` when an audio clip is selected; read when
    /// an instrument's layer dropdown opens.
    pub fn set_clip_detect_layers(
        &mut self,
        layers: Vec<(manifold_core::LayerId, String)>,
    ) {
        self.clip_detect_layers = layers;
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
        self.scene_setup_panel.register_intents(&mut self.intents);
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

            // Self-heal: a stale owner can only mean
            // the previous gesture's terminal event never reached the
            // broadcast. The next PointerDown clears it, firing the same
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
                let (consumed, mut acts) = self.scene_setup_panel.handle_event(event, &self.tree);
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

            // Owner lifetime: the stash read just above
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
                    section: None,
                },
                value: manifold_ui::RowValue { base: 0.5, effective: 0.5, exposed: true, driven: false },
                modulation: manifold_ui::RowMod::default(),
                mapping: manifold_ui::RowMapping {
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
            }],
            string_params: Vec::new(),
            collapsed,
            effect_index: 0,
            effect_id: manifold_core::EffectId::new("bug160-fixture"),
            enabled: true,
            supports_envelopes: true,
            has_graph_mod: false,
            layer_id: None,
            audio: Default::default(),
            relight: manifold_ui::RelightCardConfig::default(),
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



