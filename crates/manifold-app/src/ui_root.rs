//! UIRoot — owns the entire UI state for one window.
//!
//! Contains the UITree, UIInputSystem, ScreenLayout, all panels,
//! and the dropdown overlay. The app layer creates one UIRoot per
//! workspace window and forwards winit events through it.

use manifold_ui::*;
use manifold_ui::input::{Key, Modifiers, PointerAction, UIEvent};
use manifold_ui::node::{Vec2, Rect};

/// What the currently-open dropdown is selecting for.
#[derive(Debug, Clone)]
pub enum DropdownContext {
    BlendMode(usize),
    MidiNote(usize),
    MidiChannel(usize),
    Resolution,
    #[allow(dead_code)]
    AddEffect(InspectorTab),
    GenType(usize),
    ClipContext(String),     // right-click on clip: clip_id
    TrackContext(f32, usize), // right-click on empty track: beat, layer
    LayerContext(usize),     // right-click on layer header: layer_index
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

    /// Context for the currently-open dropdown (set before open, read on selection).
    dropdown_context: Option<DropdownContext>,

    /// Detected display resolutions from winit monitors: (w, h, label).
    /// Set by Application after monitor enumeration.
    display_resolutions: Vec<(u32, u32, String)>,

    // Inspector resize state
    pub inspector_resize_dragging: bool,
    inspector_drag_start_x: f32,
    inspector_drag_start_width: f32,

    /// Set when overlay state changes (popup open/close, scroll, category change).
    /// Consumed by app.rs to trigger rebuild_scroll_panels.
    pub overlay_dirty: bool,

    /// Effect clipboard count (set by app.rs, used by browser popup).
    pub effect_clipboard_count: usize,

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

    /// Node ID for the video/timeline split handle (color feedback on hover/drag).
    /// From Unity PanelResizeHandle.cs — idle/hover/drag color states.
    split_handle_id: i32,
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
            waveform_lane: WaveformLanePanel::new(),
            stem_lanes: StemLaneGroupPanel::new(),
            built: false,
            screen_width: 1280.0,
            screen_height: 720.0,
            time_accumulator: 0.0,
            scroll_panels_start: 0,
            dropdown_context: None,
            display_resolutions: Vec::new(),
            inspector_resize_dragging: false,
            inspector_drag_start_x: 0.0,
            inspector_drag_start_width: 0.0,
            overlay_dirty: false,
            effect_clipboard_count: 0,
            cursor_hover_actions: Vec::new(),
            viewport_events: Vec::new(),
            last_right_click_pos: Vec2::new(0.0, 0.0),
            split_handle_id: -1,
        }
    }

    /// Set detected display resolutions from winit monitors.
    pub fn set_display_resolutions(&mut self, resolutions: Vec<(u32, u32, String)>) {
        self.display_resolutions = resolutions;
    }

    /// Apply saved layout from project settings. Called after project load.
    /// Equivalent to Unity's WorkspaceController.ApplySavedLayout().
    pub fn apply_project_layout(&mut self, settings: &manifold_core::settings::ProjectSettings) {
        if settings.inspector_width > 0.0 {
            self.layout.inspector_width = settings.inspector_width
                .clamp(Self::INSPECTOR_MIN_W, Self::INSPECTOR_MAX_W);
        }
        if settings.timeline_height_percent > 0.0 {
            self.layout.timeline_split_ratio = settings.timeline_height_percent
                .clamp(manifold_ui::color::MIN_TIMELINE_SPLIT_RATIO,
                       manifold_ui::color::MAX_TIMELINE_SPLIT_RATIO);
        }
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
                -1, r.x, r.y, r.width, r.height,
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
    /// From Unity's pattern: CheckScrollAndInvalidate only repaints dirty layers,
    /// not the entire UI.
    pub fn rebuild_scroll_panels(&mut self) {
        if !self.built {
            return self.build();
        }
        self.tree.truncate_from(self.scroll_panels_start);
        // Invalidate hover — scroll panel node IDs are now stale
        self.input.invalidate_hover();
        self.build_scroll_panels();
    }

    /// Internal: build the scroll-affected panel group.
    fn build_scroll_panels(&mut self) {
        self.layer_headers.build(&mut self.tree, &self.layout);
        self.viewport.build(&mut self.tree, &self.layout);
        self.perf_hud.build(&mut self.tree, &self.layout);

        self.dropdown.set_screen_size(self.screen_width, self.screen_height);
        if self.dropdown.is_open() {
            self.dropdown.rebuild_nodes(&mut self.tree);
        }

        self.browser_popup.set_screen_size(self.screen_width, self.screen_height);
        if self.browser_popup.is_open() {
            self.browser_popup.build(&mut self.tree);
        }
    }

    /// Handle a resize event. Rebuilds all panels.
    pub fn resize(&mut self, width: f32, height: f32) {
        let same_size = (width - self.screen_width).abs() < 1.0
            && (height - self.screen_height).abs() < 1.0;
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
        self.input.process_pointer(&mut self.tree, pos, action, time);

        // On cursor move, perform continuous clip hit-testing in the viewport.
        // HoverEnter/HoverExit only fire on node-level transitions; they cannot
        // detect hover changes within the same node's bounding box (e.g., moving
        // between clips in the same track background). update_hover_at fills that gap.
        if action == PointerAction::Move {
            let mut hover_actions = self.viewport.update_hover_at(pos);
            self.cursor_hover_actions.append(&mut hover_actions);

            // Update waveform button hover state on cursor move.
            // Bitmap panels have no UITree nodes so HoverEnter/HoverExit won't fire
            // for individual buttons — track hover directly on every pointer move.
            if !self.waveform_lane.is_interacting() {
                let wf_rect = self.viewport.waveform_lane_rect();
                if wf_rect.width > 0.0 && wf_rect.height > 0.0 && wf_rect.contains(pos) {
                    self.waveform_lane.update_hover(pos.x - wf_rect.x, pos.y - wf_rect.y);
                } else {
                    self.waveform_lane.clear_hover();
                }
            }
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
    fn open_dropdown_at(&mut self, context: DropdownContext, items: Vec<DropdownItem>, trigger: Rect) {
        self.dropdown_context = Some(context);
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
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

            // Browser popup gets first crack (higher z-order than dropdown).
            if self.browser_popup.is_open() {
                use manifold_ui::panels::browser_popup::BrowserPopupAction;
                let mut consumed = false;

                // Escape key
                if let UIEvent::KeyDown { key: Key::Escape, .. } = event
                    && self.browser_popup.handle_escape().is_some() {
                        self.overlay_dirty = true;
                        consumed = true;
                    }

                // Click events
                if let UIEvent::Click { node_id, .. } = event {
                    // Search bar click → open text input
                    if self.browser_popup.is_search_bar(*node_id) {
                        actions.push(PanelAction::BrowserSearchClicked);
                        consumed = true;
                    } else if let Some(bp_action) = self.browser_popup.handle_click(*node_id) {
                        match bp_action {
                            BrowserPopupAction::Selected(key) => {
                                let tab = self.browser_popup.tab();
                                actions.push(PanelAction::AddEffect(tab, key as usize));
                            }
                            BrowserPopupAction::Paste => {
                                actions.push(PanelAction::PasteEffects);
                            }
                            BrowserPopupAction::Dismissed => {}
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
                && let Some(dd_action) = self.dropdown.handle_event(event, &mut self.tree) {
                    match dd_action {
                        DropdownAction::Selected(index) => {
                            if let Some(ctx) = self.dropdown_context.take()
                                && let Some(action) = self.dropdown_to_action(ctx, index) {
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
            // Bitmap-rendered panels have no UITree nodes — hit-test against rects.
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
                    if !is_scroll && wf_rect.width > 0.0 && wf_rect.height > 0.0 && wf_rect.contains(pos) {
                        // Event is inside the waveform lane rect
                        let local = event.with_offset(-wf_rect.x, -wf_rect.y);
                        panel_actions = self.waveform_lane.handle_event(&local, &self.tree);
                        actions.append(&mut panel_actions);
                        consumed_by_lane = true;
                    } else if !is_scroll && sl_rect.width > 0.0 && sl_rect.height > 0.0 && sl_rect.contains(pos) {
                        // Event is inside the stem lanes rect
                        let local = event.with_offset(-sl_rect.x, -sl_rect.y);
                        panel_actions = self.stem_lanes.handle_event(&local, &self.tree);
                        actions.append(&mut panel_actions);
                        consumed_by_lane = true;
                    } else if wf_active {
                        // Active scrub/drag started inside waveform lane but moved outside.
                        // Continue routing Drag/PointerUp/DragEnd so the interaction completes.
                        match event {
                            UIEvent::Drag { .. } | UIEvent::PointerUp { .. } | UIEvent::DragEnd { .. } => {
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
                    let mut lh_actions = self.layer_headers.handle_drag_begin(&mut self.tree, *node_id);
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

        // If popup was just opened, flag for rebuild so nodes appear this frame
        if !popup_open_before && self.browser_popup.is_open() {
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
                let items: Vec<DropdownItem> = BlendMode::ALL.iter()
                    .map(|m| DropdownItem::new(m.display_name()))
                    .collect();
                self.open_dropdown_at(DropdownContext::BlendMode(*idx), items, trigger);
                true
            }
            PanelAction::AddEffectClicked(tab) => {
                use manifold_core::types::EffectType;
                use manifold_core::effect_category_registry;
                use manifold_ui::panels::browser_popup::*;

                let mut names = Vec::new();
                let mut keys = Vec::new();
                let mut categories = Vec::new();
                for &et in EffectType::ALL {
                    names.push(et.display_name().to_string());
                    keys.push(et as i32);
                    categories.push(effect_category_registry::get_category(et).to_string());
                }

                // Unique category names (excluding Generators)
                let cat_names: Vec<String> = effect_category_registry::ALL_CATEGORIES.iter()
                    .filter(|&&c| c != effect_category_registry::GENERATORS)
                    .map(|&c| c.to_string())
                    .collect();

                self.browser_popup.set_screen_size(self.screen_width, self.screen_height);
                self.browser_popup.open(BrowserPopupRequest {
                    mode: BrowserPopupMode::Effect,
                    tab: *tab,
                    item_names: names,
                    item_keys: keys,
                    item_categories: categories,
                    category_names: cat_names,
                    paste_count: self.effect_clipboard_count,
                    screen_anchor: Vec2::new(trigger.x, trigger.y + trigger.height),
                });
                true
            }
            PanelAction::GenTypeClicked(layer_idx) => {
                use manifold_core::types::GeneratorType;
                let items: Vec<DropdownItem> = GeneratorType::ALL.iter()
                    .map(|g| DropdownItem::new(g.display_name()))
                    .collect();
                self.open_dropdown_at(DropdownContext::GenType(*layer_idx), items, trigger);
                true
            }
            PanelAction::MidiInputClicked(idx) => {
                let items: Vec<DropdownItem> = (0..128)
                    .map(|n| DropdownItem::new(&format!("{}", n)))
                    .collect();
                self.open_dropdown_at(DropdownContext::MidiNote(*idx), items, trigger);
                true
            }
            PanelAction::MidiChannelClicked(idx) => {
                let items: Vec<DropdownItem> = (1..=16)
                    .map(|ch| DropdownItem::new(&format!("Ch {}", ch)))
                    .collect();
                self.open_dropdown_at(DropdownContext::MidiChannel(*idx), items, trigger);
                true
            }
            PanelAction::ResolutionClicked => {
                use manifold_core::types::ResolutionPreset;
                let has_displays = !self.display_resolutions.is_empty();

                let mut items: Vec<DropdownItem> = ResolutionPreset::ALL.iter()
                    .map(|r| DropdownItem::new(&r.dropdown_label()))
                    .collect();

                // Add display resolutions below presets (Unity: Footer.CollectDisplayResolutions)
                if has_displays {
                    // Separator label (disabled, non-selectable) — matches Unity format
                    items.push(DropdownItem::disabled("\u{2500}\u{2500}  Displays  \u{2500}\u{2500}"));
                    for (w, h, label) in &self.display_resolutions {
                        items.push(DropdownItem::new(&format!("{}  ({}x{})", label, w, h)));
                    }
                }

                self.open_dropdown_at(DropdownContext::Resolution, items, trigger);
                true
            }
            PanelAction::ClipRightClicked(clip_id) => {
                let items = vec![
                    DropdownItem::new("Split at Playhead"),
                    DropdownItem::new("Delete"),
                    DropdownItem::new("Duplicate"),
                ];
                self.dropdown_context = Some(DropdownContext::ClipContext(clip_id.clone()));
                self.dropdown.open_context(items, right_click_pos, &mut self.tree);
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
                self.dropdown.open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::LayerHeaderRightClicked(layer_idx) => {
                let mut items = vec![
                    DropdownItem::new("Paste"),
                    DropdownItem::new("Import MIDI File"),
                    DropdownItem::new("Insert Video Layer"),
                    DropdownItem::new("Insert Generator Layer"),
                    DropdownItem::new("Group Selected Layers"),
                    DropdownItem::new("Ungroup"),
                ];
                // Only allow delete if more than 1 layer exists
                if self.layer_headers.layer_count() > 1 {
                    items.push(DropdownItem::new("Delete Layer"));
                }
                self.dropdown_context = Some(DropdownContext::LayerContext(*layer_idx));
                self.dropdown.open_context(items, right_click_pos, &mut self.tree);
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
                // Dropdown items are 0-indexed ("Ch 1" = index 0), channel is 1-based.
                Some(PanelAction::SetMidiChannel(layer_idx, index as i32 + 1))
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
            DropdownContext::AddEffect(tab) => {
                Some(PanelAction::AddEffect(tab, index))
            }
            DropdownContext::GenType(layer_idx) => {
                Some(PanelAction::SetGenType(layer_idx, index))
            }
            DropdownContext::ClipContext(clip_id) => {
                match index {
                    0 => Some(PanelAction::ContextSplitAtPlayhead(clip_id)),
                    1 => Some(PanelAction::ContextDeleteClip(clip_id)),
                    2 => Some(PanelAction::ContextDuplicateClip(clip_id)),
                    _ => None,
                }
            }
            DropdownContext::TrackContext(beat, layer) => {
                match index {
                    0 => Some(PanelAction::ContextPasteAtTrack(beat, layer)),
                    1 => Some(PanelAction::ContextImportMidi(layer)),
                    2 => Some(PanelAction::ContextAddVideoLayer(layer)),
                    3 => Some(PanelAction::ContextAddGeneratorLayer(layer)),
                    _ => None,
                }
            }
            DropdownContext::LayerContext(layer_idx) => {
                match index {
                    0 => Some(PanelAction::ContextPasteAtLayer(layer_idx)),
                    1 => Some(PanelAction::ContextImportMidi(layer_idx)),
                    2 => Some(PanelAction::ContextAddVideoLayer(layer_idx)),
                    3 => Some(PanelAction::ContextAddGeneratorLayer(layer_idx)),
                    4 => Some(PanelAction::ContextGroupSelectedLayers),
                    5 => Some(PanelAction::ContextUngroup(layer_idx)),
                    6 => Some(PanelAction::ContextDeleteLayer(layer_idx)),
                    _ => None,
                }
            }
        }
    }

    // ── Inspector resize ──────────────────────────────────────────

    const RESIZE_EDGE_PX: f32 = manifold_ui::color::RESIZE_EDGE_PX;
    const INSPECTOR_MIN_W: f32 = manifold_ui::color::MIN_INSPECTOR_WIDTH;
    const INSPECTOR_MAX_W: f32 = manifold_ui::color::MAX_INSPECTOR_WIDTH;

    /// Returns true if pos is near the inspector right edge (resize handle).
    pub fn is_near_inspector_edge(&self, pos: Vec2) -> bool {
        let edge_x = self.layout.content_left();
        (pos.x - edge_x).abs() < Self::RESIZE_EDGE_PX
            && pos.y >= self.layout.inspector().y
    }

    /// Begin an inspector resize drag.
    pub fn begin_inspector_resize(&mut self, x: f32) {
        self.inspector_resize_dragging = true;
        self.inspector_drag_start_x = x;
        self.inspector_drag_start_width = self.layout.inspector_width;
    }

    /// Update inspector width during resize drag. Returns true if width changed.
    pub fn update_inspector_resize(&mut self, x: f32) -> bool {
        if !self.inspector_resize_dragging { return false; }
        let delta = x - self.inspector_drag_start_x;
        let new_width = (self.inspector_drag_start_width + delta)
            .clamp(Self::INSPECTOR_MIN_W, Self::INSPECTOR_MAX_W);
        if (new_width - self.layout.inspector_width).abs() > 1.0 {
            self.layout.inspector_width = new_width;
            self.build();
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
            self.tree.set_style(self.split_handle_id as u32, manifold_ui::node::UIStyle {
                bg_color: manifold_ui::color::RESIZE_HANDLE_HOVER,
                ..manifold_ui::node::UIStyle::default()
            });
        }
    }

    /// Update split handle color to drag state.
    pub fn set_split_handle_drag(&mut self) {
        if self.split_handle_id >= 0 {
            self.tree.set_style(self.split_handle_id as u32, manifold_ui::node::UIStyle {
                bg_color: manifold_ui::color::RESIZE_HANDLE_DRAG,
                ..manifold_ui::node::UIStyle::default()
            });
        }
    }

    /// Update split handle color to idle state.
    pub fn set_split_handle_idle(&mut self) {
        if self.split_handle_id >= 0 {
            self.tree.set_style(self.split_handle_id as u32, manifold_ui::node::UIStyle {
                bg_color: manifold_ui::color::RESIZE_HANDLE_IDLE,
                ..manifold_ui::node::UIStyle::default()
            });
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
    fn is_event_in_tracks_area(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        let pos = match event {
            UIEvent::Click { pos, .. } => *pos,
            UIEvent::DoubleClick { pos, .. } => *pos,
            UIEvent::RightClick { pos, .. } => *pos,
            UIEvent::DragBegin { origin, .. } => *origin,
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
}
