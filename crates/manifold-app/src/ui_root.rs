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
    AudioSetup,
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
    const Z_ORDER: [OverlayId; 7] = [
        OverlayId::PerfHud,
        OverlayId::AudioSetup,
        OverlayId::Settings,
        OverlayId::BrowserPopup,
        OverlayId::AbletonPicker,
        OverlayId::Dropdown,
        OverlayId::Toast,
    ];
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
    // GenStringParamDropdown.
    LayerContext(usize), // survives only for its color swatches (text items are typed)
    AudioSendRoutings,   // Audio Setup: read-only list of a send's routings (device + layers)
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
    /// Candidate routing layers (id + name) for the Audio Setup modal's live
    /// trigger layer dropdowns. Refreshed by `state_sync` while the modal is
    /// open; read when a trigger row's layer dropdown opens.
    audio_trigger_layers: Vec<(manifold_core::LayerId, String)>,

    // Inspector resize state
    pub inspector_resize_dragging: bool,
    inspector_drag_start_x: f32,
    inspector_drag_start_width: f32,

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
    /// P2 "panel-split snap-back" (D15): self-tracked elapsed time for
    /// `layout.tick_splits`, same self-contained-`Instant` shape as
    /// `InspectorCompositePanel::update`'s `motion_last_tick`.
    layout_tick_last: std::time::Instant,

    /// True when a DragBegin originated in the tracks area. While active,
    /// all Drag/DragEnd events are stashed for the InteractionOverlay
    /// regardless of cursor position — prevents trim/move events from being
    /// lost when the cursor moves outside the tracks rect.
    overlay_drag_active: bool,

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
            audio_setup_panel: manifold_ui::panels::audio_setup_panel::AudioSetupPanel::new(),
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
            audio_trigger_layers: Vec::new(),
            inspector_resize_dragging: false,
            inspector_drag_start_x: 0.0,
            inspector_drag_start_width: 0.0,
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
            layout_tick_last: std::time::Instant::now(),
            overlay_drag_active: false,
            ableton_session: None,
            ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup::new(),
            ableton_picker_context: None,
            ableton_rediscovery_needed: false,
            overlay_draw: Vec::new(),
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
    pub fn build(&mut self) {
        self.tree.clear();
        // Invalidate input state — old node IDs are now stale
        self.input.invalidate_hover();

        self.layout.resize(self.screen_width, self.screen_height);

        // Static panels — preserved during scroll-only rebuilds.
        // Order: transport, header, footer, inspector (non-scroll-affected).
        self.transport.build(&mut self.tree, &self.layout);
        self.header.build(&mut self.tree, &self.layout);
        self.footer.build(&mut self.tree, &self.layout);
        self.inspector.build(&mut self.tree, &self.layout);

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
    pub fn build_inspector_in_rect(&mut self, rect: Rect) {
        self.inspector.build_in_rect(&mut self.tree, rect);
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

    /// Internal: build the scroll-affected panel group.
    fn build_scroll_panels(&mut self) {
        self.layer_headers.build(
            &mut self.tree,
            &self.layout,
            self.viewport.mapper(),
            self.viewport.scroll_y_px(),
        );
        // Record boundary between layer headers and viewport panels.
        self.viewport_panels_start = self.tree.count();
        self.viewport.build(&mut self.tree, &self.layout);

        // All top-level overlays (perf HUD + dropdown + modals) build at the
        // tail of the tree via the single overlay driver — one enumeration for
        // build, draw, and input. See build_overlays / route_overlay_event.
        self.build_overlays();
    }

    /// Internal: build viewport + remaining scroll panels (skip layer headers).
    /// Used on horizontal-only scroll where layer headers don't change.
    fn build_viewport_panels(&mut self) {
        self.viewport.build(&mut self.tree, &self.layout);

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
            OverlayId::AudioSetup => &mut self.audio_setup_panel,
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
            OverlayId::AudioSetup => self.audio_setup_panel.is_open(),
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

    /// Build every open overlay into the tree, bottom→top, recording each one's
    /// node range for the draw pass. A modal that requests a dim background gets
    /// a full-screen scrim node first (and a click on it dismisses the modal,
    /// since the scrim is not one of the modal's own nodes).
    fn build_overlays(&mut self) {
        let screen = Vec2::new(self.screen_width, self.screen_height);
        // Take the tree out so `overlay_mut` (which borrows all of self) can run
        // alongside tree writes — standard disjoint-borrow split.
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let region_start = tree.count();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if !ov.is_open() {
                continue;
            }
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
            ranges.push((start, tree.count()));
        }
        self.tree = tree;
        self.overlay_region_start = region_start;
        self.overlay_draw = ranges;
        // The tree's overlay region now matches the live open-set — record it so
        // `detect_overlay_open_change` only fires on the next genuine open/close.
        self.overlay_open_snapshot = self.current_overlay_open_mask();
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
                    break;
                }
                OverlayResponse::Ignored => {
                    if is_modal {
                        // A modal captures everything — no fall-through below it.
                        consumed = true;
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

    /// Open a dropdown anchored below a trigger rect.
    pub(crate) fn open_dropdown_at(
        &mut self,
        context: DropdownContext,
        items: Vec<DropdownItem>,
        trigger: Rect,
    ) {
        self.dropdown_context = Some(context);
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
        // Force an overlay rebuild so the just-opened dropdown is recorded into
        // `overlay_draw` and drawn this frame — essential when the trigger lives
        // inside another overlay (e.g. the Audio Setup modal), where the click is
        // consumed by the overlay driver and wouldn't otherwise dirty the tree.
        self.overlay_dirty = true;
    }

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

    /// Cache the candidate target layers for the Audio Setup modal's live
    /// trigger layer dropdowns. Set by `state_sync` while the modal is open.
    pub fn set_audio_trigger_layers(
        &mut self,
        layers: Vec<(manifold_core::LayerId, String)>,
    ) {
        self.audio_trigger_layers = layers;
    }

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
                self.drain_overlay_selections(&mut actions);
                continue;
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

            // Stash events in the tracks area for overlay processing.
            // The overlay needs &mut TimelineEditingHost which UIRoot can't provide.
            if self.is_event_in_tracks_area(event) {
                // Track drag state so Drag/DragEnd are stashed even outside tracks rect.
                if matches!(event, UIEvent::DragBegin { .. }) {
                    self.overlay_drag_active = true;
                }
                if matches!(event, UIEvent::DragEnd { .. }) {
                    self.overlay_drag_active = false;
                }
                self.viewport_events.push(event.clone());
            }
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
        for event in &events {
            match event {
                UIEvent::DragBegin { node_id, .. } => {
                    // Effect card drag handle — try to start card reorder drag
                    self.inspector.try_begin_card_drag(*node_id, &mut self.tree);
                    // Layer header drag handle — needs &mut tree for dim/indicator
                    let mut lh_actions = self
                        .layer_headers
                        .handle_drag_begin(&mut self.tree, *node_id);
                    actions.append(&mut lh_actions);
                }
                UIEvent::Drag { pos, .. } => {
                    if self.inspector.is_card_drag_active() {
                        self.inspector.update_card_drag(*pos, &mut self.tree);
                    } else if self.inspector.has_pressed_target() {
                        let mut drag_actions = self.inspector.handle_drag(*pos, &mut self.tree);
                        actions.append(&mut drag_actions);
                    }
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
                }
                _ => {}
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
                            .with_action(PanelAction::SetBlendMode(*idx, format!("{:?}", m)))
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
            PanelAction::AudioTriggerLayerClicked(send_id, band) => {
                // "Auto" (route by send name) first, then every candidate layer
                // cached by state_sync — each carries its target layer.
                let mut items = Vec::with_capacity(self.audio_trigger_layers.len() + 1);
                items.push(DropdownItem::new("Auto").with_action(
                    PanelAction::AudioTriggerSetLayer(send_id.clone(), *band, None),
                ));
                for (id, name) in &self.audio_trigger_layers {
                    items.push(DropdownItem::new(name).with_action(
                        PanelAction::AudioTriggerSetLayer(send_id.clone(), *band, Some(id.clone())),
                    ));
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::AudioSendClicked(idx) => {
                // "No send" first, then every named send from Audio Setup so the
                // layer dropdown and the setup panel can never disagree — each
                // carries its SetLayerAudioSend directly.
                let sends = self.audio_setup_panel.send_options();
                let mut items = Vec::with_capacity(sends.len() + 1);
                items.push(
                    DropdownItem::new("No send")
                        .with_action(PanelAction::SetLayerAudioSend(*idx, None)),
                );
                for (id, label) in sends {
                    items.push(
                        DropdownItem::new(&label)
                            .with_action(PanelAction::SetLayerAudioSend(*idx, Some(id))),
                    );
                }
                self.open_dropdown_typed(items, trigger);
                true
            }
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
            PanelAction::AudioSendRoutingsClicked(send_id) => {
                // Read-only: list where this send is fed from (capture device +
                // each feeding layer). Every row disabled, so nothing is
                // selectable — routing is edited from the layer header / channel
                // control, not here.
                let routings = self.audio_setup_panel.send_routings(send_id);
                let items: Vec<DropdownItem> = if routings.is_empty() {
                    vec![DropdownItem::disabled("No routing")]
                } else {
                    routings.iter().map(|r| DropdownItem::disabled(r)).collect()
                };
                self.open_dropdown_at(DropdownContext::AudioSendRoutings, items, trigger);
                true
            }
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
                // (2b.11), preserving the send's mono/stereo pairing.
                let stereo = self.audio_setup_panel.is_send_stereo(send_id);
                let items = if self
                    .audio_setup_panel
                    .current_device()
                    .is_some_and(|d| d.is_tap())
                {
                    build_tap_channel_dropdown(send_id, stereo)
                } else {
                    let dir = manifold_audio::directory::system_directory();
                    let device = match self.audio_setup_panel.current_device() {
                        Some(dev_ref) => dir.resolve(dev_ref.uid_opt(), Some(&dev_ref.name)),
                        // No explicit device → the system default input.
                        None => dir.list_input_devices().into_iter().find(|d| d.is_default),
                    };
                    build_channel_dropdown(device.as_ref(), send_id, stereo)
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
                            .with_action(PanelAction::SetMidiNote(*idx, n))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MidiChannelClicked(idx) => {
                // "All" (-1) then channels 0..15 (displayed 1..16).
                let mut items: Vec<DropdownItem> =
                    vec![DropdownItem::new("All").with_action(PanelAction::SetMidiChannel(*idx, -1))];
                items.extend((1..=16).map(|ch| {
                    DropdownItem::new(&format!("Ch {}", ch))
                        .with_action(PanelAction::SetMidiChannel(*idx, ch - 1))
                }));
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::MidiDeviceClicked(idx) => {
                // "All Devices" (None) then each named device.
                let mut items: Vec<DropdownItem> = vec![
                    DropdownItem::new("All Devices")
                        .with_action(PanelAction::SetMidiDevice(*idx, None)),
                ];
                items.extend(self.midi_device_names.iter().map(|name| {
                    DropdownItem::new(name)
                        .with_action(PanelAction::SetMidiDevice(*idx, Some(name.clone())))
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
                // Typed (2b.11): each item carries its track action.
                let items = vec![
                    DropdownItem::new("Paste")
                        .with_action(PanelAction::ContextPasteAtTrack(*beat, *layer)),
                    DropdownItem::new("Import MIDI File")
                        .with_action(PanelAction::ContextImportMidi(*layer)),
                    DropdownItem::new("Insert Video Layer")
                        .with_action(PanelAction::ContextAddVideoLayer(*layer)),
                    DropdownItem::new("Insert Generator Layer")
                        .with_action(PanelAction::ContextAddGeneratorLayer(*layer)),
                    DropdownItem::new("Insert Audio Layer")
                        .with_action(PanelAction::ContextAddAudioLayer(*layer)),
                ];
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::LayerHeaderRightClicked(layer_idx) => {
                let layer_info = self.layer_headers.layer_info(*layer_idx);
                let is_group = layer_info.is_some_and(|l| l.is_group);

                // Typed (2b.11): each text item carries its layer action. The
                // context is still set so the color swatches below (ColorSelected →
                // dropdown_color_to_action) can recover the layer.
                let li = *layer_idx;
                let mut items =
                    vec![DropdownItem::new("Paste").with_action(PanelAction::ContextPasteAtLayer(li))];
                if !is_group {
                    items.push(
                        DropdownItem::new("Import MIDI File")
                            .with_action(PanelAction::ContextImportMidi(li)),
                    );
                }
                items.push(
                    DropdownItem::new("Insert Video Layer")
                        .with_action(PanelAction::ContextAddVideoLayer(li)),
                );
                items.push(
                    DropdownItem::new("Insert Generator Layer")
                        .with_action(PanelAction::ContextAddGeneratorLayer(li)),
                );
                items.push(
                    DropdownItem::new("Insert Audio Layer")
                        .with_action(PanelAction::ContextAddAudioLayer(li)),
                );
                items.push(
                    DropdownItem::new("Duplicate Layer")
                        .with_action(PanelAction::ContextDuplicateLayer(li)),
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
                        DropdownItem::new("Ungroup").with_action(PanelAction::ContextUngroup(li)),
                    );
                }
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(
                        DropdownItem::new("Delete Layer")
                            .with_action(PanelAction::ContextDeleteLayer(li)),
                    );
                }
                // Last text item gets a separator before the color grid
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                self.dropdown_context = Some(DropdownContext::LayerContext(*layer_idx));
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
            _ => false,
        }
    }

    // dropdown_to_action removed (2b.11): every selectable dropdown item now
    // carries its own PanelAction via DropdownItem::with_action and fires
    // DropdownAction::SelectedAction directly. The only surviving DropdownContexts
    // are LayerContext (its color swatches, handled below) and AudioSendRoutings
    // (read-only), neither of which maps a positional text Selected(index).

    /// Convert a color swatch selection into the appropriate PanelAction.
    fn dropdown_color_to_action(
        &self,
        ctx: DropdownContext,
        color_idx: usize,
    ) -> Option<PanelAction> {
        match ctx {
            DropdownContext::LayerContext(layer_idx) => {
                let color = manifold_ui::color::COLOR_GRID.get(color_idx)?;
                Some(PanelAction::ContextSetLayerColor(layer_idx, *color))
            }
            _ => None,
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

    /// Check if a UI event's position falls within the tracks area.
    /// When an overlay drag is active (DragBegin originated in tracks), Drag and
    /// DragEnd events are always stashed regardless of cursor position — this
    /// prevents trim/move events from being lost when the cursor exits the rect.
    fn is_event_in_tracks_area(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        let pos = match event {
            UIEvent::Click { pos, .. } => *pos,
            UIEvent::DoubleClick { pos, .. } => *pos,
            UIEvent::RightClick { pos, .. } => *pos,
            UIEvent::DragBegin { origin, .. } => *origin,
            UIEvent::Drag { .. } | UIEvent::DragEnd { .. } if self.overlay_drag_active => {
                return true;
            }
            UIEvent::Drag { pos, .. } => *pos,
            UIEvent::DragEnd { pos, .. } => *pos,
            UIEvent::HoverEnter { pos, .. } => *pos,
            UIEvent::PointerDown { pos, .. } => *pos,
            // HoverExit has no position — treat as viewport event if overlay is dragging
            UIEvent::HoverExit { .. } => return true,
            _ => return false,
        };
        self.viewport.tracks_rect().contains(pos)
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
        self.inspector.update(&mut self.tree);
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
    }

    /// Drive the per-row trigger meters + firing flash from the selected send's
    /// live per-band transient levels `[whole, low, mid, high]` (0..1), or rest
    /// them when `None`. In place, every frame — see `update_audio_band_meters`.
    pub fn update_audio_trigger_levels(&mut self, levels: Option<[f32; 4]>) {
        if !self.audio_setup_panel.is_open() {
            return;
        }
        self.audio_setup_panel
            .update_trigger_levels(&mut self.tree, levels);
    }
}

/// The `AudioSetSendChannels` action a channel row fires (2b.11): a stereo send
/// picks the chosen channel plus its pair partner, a mono send just the channel.
fn send_channels_action(
    send_id: &manifold_core::AudioSendId,
    stereo: bool,
    ch: u16,
) -> PanelAction {
    let channels = if stereo { vec![ch, ch + 1] } else { vec![ch] };
    PanelAction::AudioSetSendChannels(send_id.clone(), channels)
}

/// Channel dropdown for a tap source. Output taps are a fixed stereo mixdown, so
/// the choices are simply Left (0) and Right (1). Each row carries its typed
/// channel action.
fn build_tap_channel_dropdown(
    send_id: &manifold_core::AudioSendId,
    stereo: bool,
) -> Vec<DropdownItem> {
    vec![
        DropdownItem::new("Left").with_action(send_channels_action(send_id, stereo, 0)),
        DropdownItem::new("Right").with_action(send_channels_action(send_id, stereo, 1)),
    ]
}

/// Build the send-channel dropdown for `device`, grouped by subdevice with
/// platform channel names; each selectable row carries its typed channel action
/// (subdevice headers stay non-selectable). Falls back to a single mono entry
/// when no device metadata is available.
fn build_channel_dropdown(
    device: Option<&manifold_audio::directory::DeviceInfo>,
    send_id: &manifold_core::AudioSendId,
    stereo: bool,
) -> Vec<DropdownItem> {
    let fallback =
        || vec![DropdownItem::new("Channel 1").with_action(send_channels_action(send_id, stereo, 0))];
    let Some(device) = device else {
        return fallback();
    };
    if device.channels.is_empty() {
        return fallback();
    }

    let mut items = Vec::new();
    let row = |ch: &manifold_audio::directory::ChannelInfo| {
        DropdownItem::new(&ch.display_name()).with_action(send_channels_action(send_id, stereo, ch.index))
    };

    if device.subdevices.is_empty() {
        for ch in &device.channels {
            items.push(row(ch));
        }
    } else {
        for group in &device.subdevices {
            items.push(DropdownItem::disabled(&group.name));
            let end = group.channel_start.saturating_add(group.channel_count);
            for idx in group.channel_start..end {
                if let Some(ch) = device.channels.get(idx as usize) {
                    items.push(row(ch));
                }
            }
        }
    }
    items
}
