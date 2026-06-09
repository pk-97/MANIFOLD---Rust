//! UIRoot — owns the entire UI state for one window.
//!
//! Contains the UITree, UIInputSystem, ScreenLayout, all panels,
//! and the dropdown overlay. The app layer creates one UIRoot per
//! workspace window and forwards winit events through it.

use manifold_playback::ableton_bridge::AbletonSession;
use manifold_ui::input::{Key, Modifiers, PointerAction, UIEvent};
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::*;

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
    BlendMode(usize),
    MidiNote(usize),
    MidiChannel(usize),
    MidiDevice(usize),
    Resolution,
    ClipContext(String),      // right-click on clip: clip_id
    TrackContext(f32, usize), // right-click on empty track: beat, layer
    LayerContext(usize),      // right-click on layer header: layer_index
    MasterExitPath,           // LED exit path dropdown
    ClkDevice,                // MIDI clock device selection
    CardContext(GraphParamTarget), // right-click on a preset card header (effect or generator)
    ParamContext(GraphParamTarget, manifold_core::effects::ParamId, f32), // gpt, param_id, default_val
    MacroSlotContext(usize),  // macro_index (right-click on macro slider)
    GenStringParamDropdown(usize), // string_param_index (dropdown selector)
    AudioInputDevice,         // audio input device selection for live recording
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
}

/// Owns all UI state for one window.
pub struct UIRoot {
    // Core
    pub tree: UITree,
    pub input: UIInputSystem,
    pub layout: ScreenLayout,

    // Panels
    pub transport: TransportPanel,
    pub header: HeaderPanel,
    pub footer: FooterPanel,
    pub layer_headers: LayerHeaderPanel,
    pub inspector: InspectorCompositePanel,
    pub viewport: TimelineViewportPanel,
    pub dropdown: DropdownPanel,
    pub browser_popup: manifold_ui::panels::browser_popup::BrowserPopupPanel,
    pub perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel,

    /// Project-embedded ("forked") presets surfaced into the Add pickers, kept
    /// in sync with the content snapshot. Change-gated by
    /// `embedded_presets_fingerprint` so the Vec rebuilds only when the embedded
    /// set actually changes, not every frame.
    pub embedded_presets: Vec<EmbeddedPresetItem>,
    embedded_presets_fingerprint: u64,

    // Waveform panels (bitmap-rendered, not UITree-based)
    pub waveform_lane: WaveformLanePanel,
    pub stem_lanes: StemLaneGroupPanel,

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

    /// Cached audio input device names for the recording audio device dropdown.
    pub audio_input_device_names: Vec<String>,
    /// Currently selected audio input device name for live recording.
    pub selected_audio_input_device: Option<String>,

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
    split_handle_id: i32,
    /// Node ID for the inspector resize handle (vertical bar at inspector right edge).
    inspector_handle_id: i32,

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
}

impl UIRoot {
    pub fn new() -> Self {
        Self {
            tree: UITree::new(),
            input: UIInputSystem::new(),
            layout: ScreenLayout::new(1280.0, 720.0),
            transport: TransportPanel::new(),
            header: HeaderPanel::new(),
            footer: FooterPanel::new(),
            layer_headers: LayerHeaderPanel::new(),
            inspector: InspectorCompositePanel::new(),
            viewport: TimelineViewportPanel::new(),
            dropdown: DropdownPanel::new(),
            browser_popup: manifold_ui::panels::browser_popup::BrowserPopupPanel::new(),
            perf_hud: manifold_ui::panels::perf_hud::PerfHudPanel::new(),
            embedded_presets: Vec::new(),
            embedded_presets_fingerprint: 0,
            waveform_lane: WaveformLanePanel::new(),
            stem_lanes: StemLaneGroupPanel::new(),
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
            audio_input_device_names: Vec::new(),
            selected_audio_input_device: None,
            inspector_resize_dragging: false,
            inspector_drag_start_x: 0.0,
            inspector_drag_start_width: 0.0,
            overlay_dirty: false,
            effect_clipboard_count: 0,
            gen_clipboard: manifold_editing::clipboard::GeneratorClipboard::new(),
            cursor_hover_actions: Vec::new(),
            viewport_events: Vec::new(),
            last_right_click_pos: Vec2::new(0.0, 0.0),
            macro_labels: std::array::from_fn(|_| String::new()),
            macro_mapping_descs: std::array::from_fn(|_| Vec::new()),
            macro_ableton_mapped: [false; manifold_core::MACRO_COUNT],
            split_handle_id: -1,
            inspector_handle_id: -1,
            overlay_drag_active: false,
            ableton_session: None,
            ableton_picker: manifold_ui::panels::ableton_picker::AbletonPickerPopup::new(),
            ableton_picker_context: None,
            ableton_rediscovery_needed: false,
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
        self.layer_headers
            .set_scroll_y(settings.viewport_scroll_y_px);

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
            self.split_handle_id = self.tree.add_panel(
                -1,
                r.x,
                r.y,
                r.width,
                r.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ) as i32;
        }

        // Inspector resize handle — thin vertical bar at inspector right edge.
        {
            let edge_x = self.layout.content_left() - 2.0;
            let insp = self.layout.inspector();
            self.inspector_handle_id = self.tree.add_panel(
                -1,
                edge_x,
                insp.y,
                4.0,
                insp.height,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            ) as i32;
        }

        // Mark boundary — everything after this is rebuilt on scroll.
        self.scroll_panels_start = self.tree.count();

        // Scroll-affected panels — rebuilt on scroll/zoom changes.
        self.build_scroll_panels();

        self.built = true;
    }

    /// Rebuild only scroll-affected panels (layer_headers, viewport, perf_hud).
    /// Static panels (transport, header, footer, inspector) keep their tree nodes.
    ///
    /// Uses `dirty` flags to skip layer header rebuild on horizontal-only scroll.
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
                ok = self
                    .layer_headers
                    .try_update_vertical_scroll(&mut self.tree, &self.layout)
                    && self.viewport.try_update_vertical_scroll(&mut self.tree);
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
        self.layer_headers.build(&mut self.tree, &self.layout);
        // Record boundary between layer headers and viewport panels.
        self.viewport_panels_start = self.tree.count();
        self.viewport.build(&mut self.tree, &self.layout);

        // Waveform & stem lane UITree nodes — must be after viewport.build()
        // so waveform_lane_rect()/stem_lanes_rect() have valid rects.
        {
            let wf_rect = self.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                self.waveform_lane.build_nodes(&mut self.tree, wf_rect);
            }
            let sl_rect = self.viewport.stem_lanes_rect();
            if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                self.stem_lanes.build_nodes(&mut self.tree, sl_rect);
            }
        }

        self.perf_hud.build(&mut self.tree, &self.layout);

        self.dropdown
            .set_screen_size(self.screen_width, self.screen_height);
        if self.dropdown.is_open() {
            self.dropdown.rebuild_nodes(&mut self.tree);
        }

        self.browser_popup
            .set_screen_size(self.screen_width, self.screen_height);
        if self.browser_popup.is_open() {
            self.browser_popup.build(&mut self.tree);
        }

        self.ableton_picker
            .set_screen_size(self.screen_width, self.screen_height);
        if self.ableton_picker.is_open() {
            self.ableton_picker.build(&mut self.tree);
        }
    }

    /// Internal: build viewport + remaining scroll panels (skip layer headers).
    /// Used on horizontal-only scroll where layer headers don't change.
    fn build_viewport_panels(&mut self) {
        self.viewport.build(&mut self.tree, &self.layout);

        {
            let wf_rect = self.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                self.waveform_lane.build_nodes(&mut self.tree, wf_rect);
            }
            let sl_rect = self.viewport.stem_lanes_rect();
            if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                self.stem_lanes.build_nodes(&mut self.tree, sl_rect);
            }
        }

        self.perf_hud.build(&mut self.tree, &self.layout);

        self.dropdown
            .set_screen_size(self.screen_width, self.screen_height);
        if self.dropdown.is_open() {
            self.dropdown.rebuild_nodes(&mut self.tree);
        }

        self.browser_popup
            .set_screen_size(self.screen_width, self.screen_height);
        if self.browser_popup.is_open() {
            self.browser_popup.build(&mut self.tree);
        }

        self.ableton_picker
            .set_screen_size(self.screen_width, self.screen_height);
        if self.ableton_picker.is_open() {
            self.ableton_picker.build(&mut self.tree);
        }
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
        self.input.process_key(key, modifiers);
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
                })
            })
            .collect();
    }

    /// Drain events from the input system and route to panels.
    /// Returns all panel actions for the app layer to dispatch.
    pub fn process_events(&mut self) -> Vec<PanelAction> {
        if !self.built {
            return Vec::new();
        }

        let events = self.input.drain_events();
        let mut actions = Vec::new();

        // Drain continuous hover actions accumulated from cursor movement.
        actions.append(&mut self.cursor_hover_actions);

        let mut last_click_node: i32 = -1;
        for event in &events {
            // Track which node was clicked (for dropdown anchoring).
            if let UIEvent::Click { node_id, .. } = event {
                last_click_node = *node_id as i32;
            }
            if let UIEvent::RightClick { pos, .. } = event {
                self.last_right_click_pos = *pos;
            }

            // Ableton picker popup — highest z-order (opened from dropdown).
            if self.ableton_picker.is_open() {
                use manifold_ui::panels::ableton_picker::AbletonPickerAction;
                let mut consumed = false;

                if let UIEvent::KeyDown {
                    key: Key::Escape, ..
                } = event
                    && self.ableton_picker.handle_escape().is_some()
                {
                    self.overlay_dirty = true;
                    consumed = true;
                }

                if let UIEvent::Click { node_id, .. } = event {
                    if let Some(picker_action) = self.ableton_picker.handle_click(*node_id) {
                        match picker_action {
                            AbletonPickerAction::Selected(addr) => {
                                if let Some(ctx) = self.ableton_picker_context.take() {
                                    use manifold_ui::panels::ableton_picker::AbletonPickerContext;
                                    match ctx {
                                        AbletonPickerContext::Param { gpt, param_id } => {
                                            actions.push(PanelAction::MapParamToAbleton(
                                                gpt, param_id, addr,
                                            ));
                                        }
                                        AbletonPickerContext::MacroSlot { slot_idx } => {
                                            actions.push(PanelAction::MapMacroToAbleton(
                                                slot_idx, addr,
                                            ));
                                        }
                                    }
                                }
                            }
                            AbletonPickerAction::Dismissed => {}
                        }
                        self.overlay_dirty = true;
                        consumed = true;
                    } else if self.ableton_picker.contains_node(*node_id) {
                        // Track-selection click — redraw right column next frame.
                        self.overlay_dirty = true;
                        consumed = true;
                    }
                }

                if consumed {
                    continue;
                }
            }

            // Browser popup gets first crack (higher z-order than dropdown).
            if self.browser_popup.is_open() {
                use manifold_ui::panels::browser_popup::{BrowserPopupAction, BrowserPopupMode};
                let mut consumed = false;

                // Escape key
                if let UIEvent::KeyDown {
                    key: Key::Escape, ..
                } = event
                    && self.browser_popup.handle_escape().is_some()
                {
                    self.overlay_dirty = true;
                    consumed = true;
                }

                // Click events
                if let UIEvent::Click { node_id, .. } = event {
                    // Search bar click → open text input
                    if self.browser_popup.is_search_bar(*node_id) {
                        actions.push(PanelAction::BrowserSearchClicked);
                        consumed = true;
                    } else {
                        if let Some(bp_action) = self.browser_popup.handle_click(*node_id) {
                            match bp_action {
                                BrowserPopupAction::Selected {
                                    type_id,
                                    mode,
                                    tab,
                                    layer_id,
                                } => match mode {
                                    BrowserPopupMode::Effect => {
                                        actions.push(PanelAction::AddEffect(
                                            tab,
                                            manifold_core::PresetTypeId::from_string(type_id),
                                        ));
                                    }
                                    BrowserPopupMode::Generator => {
                                        actions.push(PanelAction::SetGenType(
                                            layer_id,
                                            manifold_core::PresetTypeId::from_string(type_id),
                                        ));
                                    }
                                    // Node mode is editor-window only; the
                                    // main-window popup never opens it.
                                    BrowserPopupMode::Node => {}
                                },
                                BrowserPopupAction::Paste => {
                                    actions.push(PanelAction::PasteEffects);
                                }
                                BrowserPopupAction::Dismissed => {}
                                // Editor-window only; never reached here.
                                BrowserPopupAction::NodeSelected { .. } => {}
                            }
                            self.overlay_dirty = true;
                            consumed = true;
                        } else if self.browser_popup.contains_node(*node_id) {
                            // Internal popup click (category chip, background, etc.)
                            // Consume so it doesn't leak to panels below.
                            self.overlay_dirty = true;
                            consumed = true;
                        }
                    }
                }

                // Scroll events within the popup
                if let UIEvent::Scroll { delta, .. } = event {
                    self.browser_popup.handle_scroll(delta.y);
                    self.overlay_dirty = true;
                    consumed = true;
                }

                if consumed {
                    continue;
                }
            }

            // Dropdown gets first crack at all events.
            if self.dropdown.is_open()
                && let Some(dd_action) = self.dropdown.handle_event(event, &mut self.tree)
            {
                match dd_action {
                    DropdownAction::Selected(index) => {
                        if let Some(ctx) = self.dropdown_context.take()
                            && let Some(action) = self.dropdown_to_action(ctx, index)
                        {
                            actions.push(action);
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
                        // Only clear context if dropdown actually closed
                        // (disabled item clicks send Dismissed but keep dropdown open)
                        if !self.dropdown.is_open() {
                            self.dropdown_context = None;
                        }
                    }
                }
                continue; // Event consumed by dropdown.
            }

            // Route to panels.
            let mut panel_actions;

            panel_actions = self.transport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.header.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.footer.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.layer_headers.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.inspector.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            // Waveform lane & stem lanes: route events with local coordinate conversion.
            // These panels use UITree nodes for buttons/overlays (providing valid hit_ids)
            // but still route by rect containment to handle local coordinate conversion.
            {
                let wf_rect = self.viewport.waveform_lane_rect();
                let sl_rect = if self.stem_lanes.is_expanded() {
                    self.viewport.stem_lanes_rect()
                } else {
                    manifold_ui::node::Rect::ZERO
                };

                let wf_active = self.waveform_lane.is_interacting();
                let mut consumed_by_lane = false;

                // Scroll events pass through to viewport (Unity: WaveformLaneScrollForwarder).
                let is_scroll = matches!(event, UIEvent::Scroll { .. });

                if let Some(pos) = event.pos() {
                    if !is_scroll
                        && wf_rect.width > 0.0
                        && wf_rect.height > 0.0
                        && wf_rect.contains(pos)
                    {
                        // Event is inside the waveform lane rect
                        let local = event.with_offset(-wf_rect.x, -wf_rect.y);
                        panel_actions = self.waveform_lane.handle_event(&local, &self.tree);
                        actions.append(&mut panel_actions);
                        consumed_by_lane = true;
                    } else if !is_scroll
                        && sl_rect.width > 0.0
                        && sl_rect.height > 0.0
                        && sl_rect.contains(pos)
                    {
                        // Event is inside the stem lanes rect
                        let local = event.with_offset(-sl_rect.x, -sl_rect.y);
                        panel_actions = self.stem_lanes.handle_event(&local, &self.tree);
                        actions.append(&mut panel_actions);
                        consumed_by_lane = true;
                    } else if wf_active {
                        // Active scrub/drag started inside waveform lane but moved outside.
                        // Continue routing Drag/PointerUp/DragEnd so the interaction completes.
                        match event {
                            UIEvent::Drag { .. }
                            | UIEvent::PointerUp { .. }
                            | UIEvent::DragEnd { .. } => {
                                let local = event.with_offset(-wf_rect.x, -wf_rect.y);
                                panel_actions = self.waveform_lane.handle_event(&local, &self.tree);
                                actions.append(&mut panel_actions);
                                consumed_by_lane = true;
                            }
                            _ => {}
                        }
                    }
                }

                // Route PointerUp/DragEnd even for position-less events
                // to ensure scrub/drag state is cleared.
                if !consumed_by_lane && wf_active {
                    match event {
                        UIEvent::PointerUp { .. } | UIEvent::DragEnd { .. } => {
                            panel_actions = self.waveform_lane.handle_event(event, &self.tree);
                            actions.append(&mut panel_actions);
                        }
                        _ => {}
                    }
                }

                if consumed_by_lane {
                    // Don't pass to viewport/overlay — event was in waveform/stem area
                    continue;
                }
            }

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
                        let mut lh_actions = self.layer_headers.handle_drag(&mut self.tree, *pos);
                        actions.append(&mut lh_actions);
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

    /// If the action is a dropdown trigger, open the dropdown anchored to the
    /// clicked button and return true (action consumed). Otherwise return false.
    fn try_open_dropdown(&mut self, action: &PanelAction, click_node: i32) -> bool {
        let right_click_pos = self.last_right_click_pos;
        let trigger = if click_node >= 0 {
            self.tree.get_bounds(click_node as u32)
        } else {
            Rect::new(100.0, 100.0, 80.0, 24.0)
        };

        match action {
            PanelAction::BlendModeClicked(idx) => {
                use manifold_core::types::BlendMode;
                let items: Vec<DropdownItem> = BlendMode::ALL
                    .iter()
                    .map(|m| DropdownItem::new(m.display_name()))
                    .collect();
                self.open_dropdown_at(DropdownContext::BlendMode(*idx), items, trigger);
                true
            }
            PanelAction::AddEffectClicked(tab) => {
                use manifold_core::{preset_def::PresetKind, preset_type_registry};
                use manifold_ui::panels::browser_popup::*;

                let available = preset_type_registry::available_of_kind(PresetKind::Effect);
                let mut items: Vec<(String, String, String)> = available
                    .iter()
                    .map(|reg| {
                        (
                            reg.display_name.to_string(),
                            reg.id.as_str().to_string(),
                            reg.category.unwrap_or("").to_string(),
                        )
                    })
                    .collect();
                // Project-embedded ("forked") effects, grouped under "Project".
                let embedded: Vec<(String, String, String)> = self
                    .embedded_presets
                    .iter()
                    .filter(|e| matches!(e.kind, PresetKind::Effect))
                    .map(|e| (e.display_name.clone(), e.type_id.clone(), "Project".to_string()))
                    .collect();
                let has_embedded = !embedded.is_empty();
                items.extend(embedded);
                items.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                let names: Vec<String> = items.iter().map(|i| i.0.clone()).collect();
                let type_ids: Vec<String> = items.iter().map(|i| i.1.clone()).collect();
                let categories: Vec<String> = items.iter().map(|i| i.2.clone()).collect();

                // Unique category names (+ "Project" when embedded effects exist).
                let mut cat_names: Vec<String> = preset_type_registry::ALL_CATEGORIES
                    .iter()
                    .map(|&c| c.to_string())
                    .collect();
                if has_embedded {
                    cat_names.push("Project".to_string());
                }

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Effect,
                    tab: *tab,
                    layer_id: None,
                    item_names: names,
                    item_categories: categories,
                    category_names: cat_names,
                    item_type_ids: type_ids,
                    item_search: None,
                    spawn_graph_pos: None,
                    paste_count: self.effect_clipboard_count,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::GenTypeClicked(layer_id) => {
                use manifold_core::{preset_def::PresetKind, preset_type_registry};
                use manifold_ui::panels::browser_popup::*;

                let available = preset_type_registry::available_of_kind(PresetKind::Generator);
                let mut items: Vec<(String, String)> = available
                    .iter()
                    .map(|reg| (reg.display_name.to_string(), reg.id.as_str().to_string()))
                    .collect();
                // Project-embedded ("forked") generators.
                items.extend(
                    self.embedded_presets
                        .iter()
                        .filter(|e| matches!(e.kind, PresetKind::Generator))
                        .map(|e| (e.display_name.clone(), e.type_id.clone())),
                );
                items.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                let names: Vec<String> = items.iter().map(|i| i.0.clone()).collect();
                let type_ids: Vec<String> = items.iter().map(|i| i.1.clone()).collect();

                self.browser_popup
                    .set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Generator,
                    tab: InspectorTab::Layer,
                    layer_id: layer_id.clone(),
                    item_names: names,
                    item_categories: Vec::new(),
                    category_names: Vec::new(),
                    item_type_ids: type_ids,
                    item_search: None,
                    spawn_graph_pos: None,
                    paste_count: 0,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::SelectAudioInputDevice => {
                // Enumerate audio input devices on demand.
                self.audio_input_device_names =
                    manifold_audio::capture::AudioCaptureDevice::list_devices()
                        .into_iter()
                        .map(|d| d.name)
                        .collect();
                let mut items: Vec<DropdownItem> = vec![DropdownItem::new("None (video only)")];
                items.extend(
                    self.audio_input_device_names
                        .iter()
                        .map(|name| DropdownItem::new(name)),
                );
                self.open_dropdown_at(DropdownContext::AudioInputDevice, items, trigger);
                true
            }
            PanelAction::SelectClkDevice => {
                if self.midi_device_names.is_empty() {
                    log::info!("[UIRoot] No MIDI devices available for CLK selection");
                    return false;
                }
                let items: Vec<DropdownItem> = self
                    .midi_device_names
                    .iter()
                    .map(|name| DropdownItem::new(name))
                    .collect();
                self.open_dropdown_at(DropdownContext::ClkDevice, items, trigger);
                true
            }
            PanelAction::MidiInputClicked(idx) => {
                let items: Vec<DropdownItem> = (0..128)
                    .map(|n| DropdownItem::new(&manifold_core::midi::note_number_to_name(n)))
                    .collect();
                self.open_dropdown_at(DropdownContext::MidiNote(*idx), items, trigger);
                true
            }
            PanelAction::MidiChannelClicked(idx) => {
                let mut items: Vec<DropdownItem> = vec![DropdownItem::new("All")];
                items.extend((1..=16).map(|ch| DropdownItem::new(&format!("Ch {}", ch))));
                self.open_dropdown_at(DropdownContext::MidiChannel(*idx), items, trigger);
                true
            }
            PanelAction::MidiDeviceClicked(idx) => {
                let mut items: Vec<DropdownItem> = vec![DropdownItem::new("All Devices")];
                items.extend(
                    self.midi_device_names
                        .iter()
                        .map(|name| DropdownItem::new(name)),
                );
                self.open_dropdown_at(DropdownContext::MidiDevice(*idx), items, trigger);
                true
            }
            PanelAction::ResolutionClicked => {
                use manifold_core::types::ResolutionPreset;
                let has_displays = !self.display_resolutions.is_empty();

                let mut items: Vec<DropdownItem> = ResolutionPreset::ALL
                    .iter()
                    .map(|r| DropdownItem::new(&r.dropdown_label()))
                    .collect();

                // Add display resolutions below presets (Unity: Footer.CollectDisplayResolutions)
                if has_displays {
                    // Separator label (disabled, non-selectable) — matches Unity format
                    items.push(DropdownItem::disabled("---  Displays  ---"));
                    for (w, h, label) in &self.display_resolutions {
                        items.push(DropdownItem::new(&format!("{}  ({}x{})", label, w, h)));
                    }
                }

                self.open_dropdown_at(DropdownContext::Resolution, items, trigger);
                true
            }
            PanelAction::MasterExitPathClicked => {
                // Build dropdown: "After All FX" (default), "Before FX", then each effect
                let mut items = vec![
                    DropdownItem::new("After All FX"),
                    DropdownItem::new("Before FX"),
                ];
                for name in &self.master_effect_names {
                    items.push(DropdownItem::new(&format!("After {}", name)));
                }
                self.open_dropdown_at(DropdownContext::MasterExitPath, items, trigger);
                true
            }
            PanelAction::ClipRightClicked(clip_id) => {
                let items = vec![
                    DropdownItem::new("Split at Playhead"),
                    DropdownItem::new("Delete"),
                    DropdownItem::new("Duplicate"),
                ];
                self.dropdown_context = Some(DropdownContext::ClipContext(clip_id.clone()));
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::TrackRightClicked(beat, layer) => {
                let items = vec![
                    DropdownItem::new("Paste"),
                    DropdownItem::new("Import MIDI File"),
                    DropdownItem::new("Insert Video Layer"),
                    DropdownItem::new("Insert Generator Layer"),
                ];
                self.dropdown_context = Some(DropdownContext::TrackContext(*beat, *layer));
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::LayerHeaderRightClicked(layer_idx) => {
                let layer_info = self.layer_headers.layer_info(*layer_idx);
                let is_group = layer_info.is_some_and(|l| l.is_group);

                let mut items = vec![DropdownItem::new("Paste")];
                if !is_group {
                    items.push(DropdownItem::new("Import MIDI File"));
                }
                items.push(DropdownItem::new("Insert Video Layer"));
                items.push(DropdownItem::new("Insert Generator Layer"));
                items.push(DropdownItem::new("Duplicate Layer"));
                // "Group" only when 2+ non-group, non-nested layers are selected
                let can_group = self.layer_headers.layer_count() >= 2 && !is_group;
                if can_group {
                    items.push(DropdownItem::new("Group Selected Layers"));
                }
                if is_group {
                    items.push(DropdownItem::new("Ungroup"));
                }
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(DropdownItem::new("Delete Layer"));
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
                    items.push(DropdownItem::new(&label));
                }
                // Ableton picker entry
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                let ableton_connected = self.ableton_session.as_ref().is_some_and(|s| s.connected);
                if ableton_connected {
                    items.push(DropdownItem::new("Map to Ableton Macro…"));
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
                    items.push(DropdownItem::new("Remove Ableton Mapping"));
                }
                self.dropdown_context =
                    Some(DropdownContext::ParamContext(*gpt, param_id.clone(), 0.0));
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::MacroLabelRightClick(macro_idx) => {
                let descs = &self.macro_mapping_descs[*macro_idx];
                let mut rename = DropdownItem::new("Rename");
                rename.separator_after = true;
                let mut items = vec![rename];
                if descs.is_empty() {
                    let mut item = DropdownItem::new("No mappings");
                    item.enabled = false;
                    items.push(item);
                } else {
                    for desc in descs {
                        items.push(DropdownItem::new(desc));
                    }
                    if descs.len() > 1 {
                        if let Some(last) = items.last_mut() {
                            last.separator_after = true;
                        }
                        items.push(DropdownItem::new("Clear All"));
                    }
                }
                // Ableton section — same pattern as effect/gen param dropdowns
                if let Some(last) = items.last_mut() {
                    last.separator_after = true;
                }
                if self.ableton_session.is_some() {
                    items.push(DropdownItem::new("Map to Ableton Macro\u{2026}"));
                } else {
                    let mut item = DropdownItem::new("Ableton not connected");
                    item.enabled = false;
                    items.push(item);
                }
                // "Remove Ableton Mapping" if this macro is mapped
                if self.macro_ableton_mapped[*macro_idx] {
                    items.push(DropdownItem::new("Remove Ableton Mapping"));
                }
                self.dropdown_context = Some(DropdownContext::MacroSlotContext(*macro_idx));
                self.dropdown
                    .open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::CardRightClicked(gpt) => {
                // Generators carry Copy/Paste (their own clipboard); both kinds
                // share Make Unique / Export / Import. The menu CONTENTS differ
                // per kind by design (the legitimately-divergent shell); the
                // fork actions + their dispatch are one path keyed by `gpt`.
                let mut items = Vec::new();
                if matches!(gpt, GraphParamTarget::Generator) {
                    items.push(DropdownItem::new("Copy Generator"));
                    if self.gen_clipboard.has_content() {
                        items.push(DropdownItem::new("Paste Generator"));
                    }
                }
                items.push(DropdownItem::new("Make Unique"));
                items.push(DropdownItem::new("Export Preset…"));
                items.push(DropdownItem::new("Import Preset…"));
                self.dropdown_context = Some(DropdownContext::CardContext(*gpt));
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

    /// Convert a dropdown selection into the appropriate PanelAction based on context.
    fn dropdown_to_action(&self, ctx: DropdownContext, index: usize) -> Option<PanelAction> {
        match ctx {
            DropdownContext::BlendMode(layer_idx) => {
                use manifold_core::types::BlendMode;
                let mode = BlendMode::from_index(index);
                Some(PanelAction::SetBlendMode(layer_idx, format!("{:?}", mode)))
            }
            DropdownContext::MidiNote(layer_idx) => {
                Some(PanelAction::SetMidiNote(layer_idx, index as i32))
            }
            DropdownContext::MidiChannel(layer_idx) => {
                // Item 0 = "All" (-1 sentinel); 1..=16 map to channels 0..15.
                let ch = if index == 0 { -1 } else { index as i32 - 1 };
                Some(PanelAction::SetMidiChannel(layer_idx, ch))
            }
            DropdownContext::MidiDevice(layer_idx) => {
                // Item 0 = "All Devices" (None); subsequent items map into midi_device_names.
                let device = if index == 0 {
                    None
                } else {
                    self.midi_device_names.get(index - 1).cloned()
                };
                Some(PanelAction::SetMidiDevice(layer_idx, device))
            }
            DropdownContext::Resolution => {
                use manifold_core::types::ResolutionPreset;
                let preset_count = ResolutionPreset::ALL.len();
                if index < preset_count {
                    // Preset selection (undoable)
                    Some(PanelAction::SetResolution(index))
                } else {
                    // Skip separator (preset_count = separator index)
                    let display_idx = index.checked_sub(preset_count + 1)?;
                    let (w, h, _) = self.display_resolutions.get(display_idx)?;
                    Some(PanelAction::SetDisplayResolution(*w as i32, *h as i32))
                }
            }
            DropdownContext::ClipContext(clip_id) => match index {
                0 => Some(PanelAction::ContextSplitAtPlayhead(clip_id)),
                1 => Some(PanelAction::ContextDeleteClip(clip_id)),
                2 => Some(PanelAction::ContextDuplicateClip(clip_id)),
                _ => None,
            },
            DropdownContext::TrackContext(beat, layer) => match index {
                0 => Some(PanelAction::ContextPasteAtTrack(beat, layer)),
                1 => Some(PanelAction::ContextImportMidi(layer)),
                2 => Some(PanelAction::ContextAddVideoLayer(layer)),
                3 => Some(PanelAction::ContextAddGeneratorLayer(layer)),
                _ => None,
            },
            DropdownContext::LayerContext(layer_idx) => match self.dropdown.item_label(index) {
                Some("Paste") => Some(PanelAction::ContextPasteAtLayer(layer_idx)),
                Some("Import MIDI File") => Some(PanelAction::ContextImportMidi(layer_idx)),
                Some("Insert Video Layer") => Some(PanelAction::ContextAddVideoLayer(layer_idx)),
                Some("Insert Generator Layer") => {
                    Some(PanelAction::ContextAddGeneratorLayer(layer_idx))
                }
                Some("Duplicate Layer") => Some(PanelAction::ContextDuplicateLayer(layer_idx)),
                Some("Group Selected Layers") => Some(PanelAction::ContextGroupSelectedLayers),
                Some("Ungroup") => Some(PanelAction::ContextUngroup(layer_idx)),
                Some("Delete Layer") => Some(PanelAction::ContextDeleteLayer(layer_idx)),
                _ => None,
            },
            DropdownContext::ClkDevice => Some(PanelAction::SetMidiClockDevice(index as i32)),
            DropdownContext::AudioInputDevice => {
                if index == 0 {
                    // "None (video only)"
                    Some(PanelAction::SetAudioInputDevice(String::new()))
                } else {
                    let device_idx = index - 1;
                    self.audio_input_device_names
                        .get(device_idx)
                        .map(|name| PanelAction::SetAudioInputDevice(name.clone()))
                }
            }
            DropdownContext::CardContext(gpt) => {
                // Label-matched: Copy/Paste are generator-only + conditional, so
                // item indices shift — match the label, not a fixed position.
                // Make Unique / Export / Import carry the card's target so the
                // dispatch runs one path for effects and generators.
                match self.dropdown.item_label(index) {
                    Some("Copy Generator") => Some(PanelAction::CopyGenerator),
                    Some("Paste Generator") => Some(PanelAction::PasteGenerator),
                    Some("Make Unique") => Some(PanelAction::MakePresetUnique(gpt)),
                    Some("Export Preset\u{2026}") => Some(PanelAction::ExportPreset(gpt)),
                    Some("Import Preset\u{2026}") => Some(PanelAction::ImportPreset(gpt)),
                    _ => None,
                }
            }
            DropdownContext::ParamContext(gpt, param_id, _default_val) => {
                if index < manifold_core::MACRO_COUNT {
                    Some(PanelAction::MapParamToMacro(gpt, param_id, index))
                } else if index == manifold_core::MACRO_COUNT {
                    // "Map to Ableton Macro…" (only reached if Ableton connected — item is
                    // disabled otherwise and won't fire Selected).
                    Some(PanelAction::OpenAbletonPickerForParam(gpt, param_id))
                } else if index == manifold_core::MACRO_COUNT + 1 {
                    // "Remove Ableton Mapping" (only present when param is mapped)
                    Some(PanelAction::UnmapParamAbleton(gpt, param_id))
                } else {
                    None
                }
            }
            DropdownContext::MacroSlotContext(macro_idx) => {
                let mapping_count = self.macro_mapping_descs[macro_idx].len();
                // Match by label to avoid brittle index math with variable Ableton items
                let label = self.dropdown.item_label(index);
                if index == 0 {
                    Some(PanelAction::MacroLabelRename(macro_idx))
                } else if label == Some("Map to Ableton Macro\u{2026}") {
                    Some(PanelAction::OpenAbletonPickerForMacro(macro_idx))
                } else if label == Some("Remove Ableton Mapping") {
                    Some(PanelAction::UnmapMacroAbleton(macro_idx))
                } else if label == Some("Clear All") {
                    Some(PanelAction::ClearMacroMappings(macro_idx))
                } else if index > 0 && index <= mapping_count {
                    Some(PanelAction::UnmapMacro(macro_idx, index - 1))
                } else {
                    None
                }
            }
            DropdownContext::GenStringParamDropdown(sp_idx) => {
                let label = self.dropdown.item_label(index)?;
                Some(PanelAction::GenStringParamSelected(
                    sp_idx,
                    label.to_string(),
                ))
            }
            DropdownContext::MasterExitPath => {
                // 0 = "After All FX" → led_exit_index -1
                // 1 = "Before FX"    → led_exit_index 0
                // 2+ = "After {N}"   → led_exit_index N (1-based in Unity convention)
                let exit_index: i32 = match index {
                    0 => -1,
                    1 => 0,
                    n => n as i32 - 1,
                };
                Some(PanelAction::SetLedExitIndex(exit_index))
            }
        }
    }

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

    /// Returns true if pos is near the inspector right edge (resize handle).
    pub fn is_near_inspector_edge(&self, pos: Vec2) -> bool {
        let edge_x = self.layout.content_left();
        (pos.x - edge_x).abs() < Self::RESIZE_EDGE_PX && pos.y >= self.layout.inspector().y
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
        let delta = x - self.inspector_drag_start_x;
        let new_width = (self.inspector_drag_start_width + delta)
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
        if self.split_handle_id >= 0 {
            self.tree.set_style(
                self.split_handle_id as u32,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_HOVER,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    /// Update split handle color to drag state.
    pub fn set_split_handle_drag(&mut self) {
        if self.split_handle_id >= 0 {
            self.tree.set_style(
                self.split_handle_id as u32,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_DRAG,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    /// Update split handle color to idle state.
    pub fn set_split_handle_idle(&mut self) {
        if self.split_handle_id >= 0 {
            self.tree.set_style(
                self.split_handle_id as u32,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_hover(&mut self) {
        if self.inspector_handle_id >= 0 {
            self.tree.set_style(
                self.inspector_handle_id as u32,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_HOVER,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_drag(&mut self) {
        if self.inspector_handle_id >= 0 {
            self.tree.set_style(
                self.inspector_handle_id as u32,
                manifold_ui::node::UIStyle {
                    bg_color: manifold_ui::color::RESIZE_HANDLE_DRAG,
                    ..manifold_ui::node::UIStyle::default()
                },
            );
        }
    }

    pub fn set_inspector_handle_idle(&mut self) {
        if self.inspector_handle_id >= 0 {
            self.tree.set_style(
                self.inspector_handle_id as u32,
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
        actions.retain(|action| !self.try_open_dropdown(action, -1));
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
        self.transport.update(&mut self.tree);
        self.header.update(&mut self.tree);
        self.footer.update(&mut self.tree);
        self.layer_headers.update(&mut self.tree);
        self.inspector.update(&mut self.tree);
        self.viewport.update(&mut self.tree);
        self.perf_hud.update(&mut self.tree);
    }

    /// Push waveform/stem lane node visibility and style to UITree.
    /// Called from app_render after syncing mute/solo/stems_available state.
    /// Separate from update() because app_render must sync state first.
    pub fn update_waveform_stem_nodes(&mut self) {
        self.waveform_lane.update_nodes(&mut self.tree);
        self.stem_lanes.update_nodes(&mut self.tree);
    }
}
