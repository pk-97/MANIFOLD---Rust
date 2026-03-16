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

    // State
    built: bool,
    screen_width: f32,
    screen_height: f32,
    time_accumulator: f32,

    /// Context for the currently-open dropdown (set before open, read on selection).
    dropdown_context: Option<DropdownContext>,

    // Inspector resize state
    pub inspector_resize_dragging: bool,
    inspector_drag_start_x: f32,
    inspector_drag_start_width: f32,

    /// Hover actions produced by continuous cursor movement, drained in process_events.
    cursor_hover_actions: Vec<PanelAction>,
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
            built: false,
            screen_width: 1280.0,
            screen_height: 720.0,
            time_accumulator: 0.0,
            dropdown_context: None,
            inspector_resize_dragging: false,
            inspector_drag_start_x: 0.0,
            inspector_drag_start_width: 0.0,
            cursor_hover_actions: Vec::new(),
        }
    }

    /// Build all panels. Call once after creation and after resize.
    pub fn build(&mut self) {
        self.tree.clear();
        // Invalidate input state — old node IDs are now stale
        self.input.invalidate_hover();

        self.layout = ScreenLayout::new(self.screen_width, self.screen_height);

        self.transport.build(&mut self.tree, &self.layout);
        self.header.build(&mut self.tree, &self.layout);
        self.footer.build(&mut self.tree, &self.layout);
        self.layer_headers.build(&mut self.tree, &self.layout);
        self.inspector.build(&mut self.tree, &self.layout);
        self.viewport.build(&mut self.tree, &self.layout);

        self.dropdown.set_screen_size(self.screen_width, self.screen_height);

        self.built = true;
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
        let mut last_right_click_pos = Vec2::new(0.0, 0.0);

        for event in &events {
            // Track which node was clicked (for dropdown anchoring).
            if let UIEvent::Click { node_id, .. } = event {
                last_click_node = *node_id as i32;
            }
            if let UIEvent::RightClick { pos, .. } = event {
                last_right_click_pos = *pos;
            }

            // Dropdown gets first crack at all events.
            if self.dropdown.is_open() {
                if let Some(dd_action) = self.dropdown.handle_event(event, &mut self.tree) {
                    match dd_action {
                        DropdownAction::Selected(index) => {
                            if let Some(ctx) = self.dropdown_context.take() {
                                if let Some(action) = Self::dropdown_to_action(ctx, index) {
                                    actions.push(action);
                                }
                            }
                        }
                        DropdownAction::Dismissed => {
                            self.dropdown_context = None;
                        }
                    }
                    continue; // Event consumed by dropdown.
                }
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

            panel_actions = self.viewport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);
        }

        // Route Drag/DragEnd to inspector directly (needs &mut tree for slider feedback).
        // This is separate from the panel event loop because Panel::handle_event takes
        // &UITree, but slider drag updates need &mut UITree.
        for event in &events {
            match event {
                UIEvent::Drag { pos, .. } => {
                    if self.inspector.has_pressed_target() {
                        let mut drag_actions = self.inspector.handle_drag(*pos, &mut self.tree);
                        actions.append(&mut drag_actions);
                    }
                }
                UIEvent::DragEnd { .. } => {
                    if self.inspector.has_pressed_target() {
                        let mut end_actions = self.inspector.handle_drag_end(&mut self.tree);
                        actions.append(&mut end_actions);
                    }
                }
                _ => {}
            }
        }

        // Intercept dropdown-triggering actions and open dropdowns here
        // (where we have access to the tree for node bounds).
        let mut filtered = Vec::with_capacity(actions.len());
        for action in actions {
            if self.try_open_dropdown(&action, last_click_node, last_right_click_pos) {
                // Consumed — don't forward to dispatch.
                continue;
            }
            filtered.push(action);
        }

        filtered
    }

    /// If the action is a dropdown trigger, open the dropdown anchored to the
    /// clicked button and return true (action consumed). Otherwise return false.
    fn try_open_dropdown(&mut self, action: &PanelAction, click_node: i32, right_click_pos: Vec2) -> bool {
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
                let items: Vec<DropdownItem> = EffectType::ALL.iter()
                    .map(|e| DropdownItem::new(e.display_name()))
                    .collect();
                self.open_dropdown_at(DropdownContext::AddEffect(*tab), items, trigger);
                true
            }
            PanelAction::GenTypeClicked => {
                use manifold_core::types::GeneratorType;
                let items: Vec<DropdownItem> = GeneratorType::ALL.iter()
                    .map(|g| DropdownItem::new(g.display_name()))
                    .collect();
                self.open_dropdown_at(DropdownContext::GenType(0), items, trigger);
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
                let items: Vec<DropdownItem> = ResolutionPreset::ALL.iter()
                    .map(|r| DropdownItem::new(r.display_name()))
                    .collect();
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
                    DropdownItem::new("Insert Video Layer"),
                    DropdownItem::new("Insert Generator Layer"),
                ];
                self.dropdown_context = Some(DropdownContext::TrackContext(*beat, *layer));
                self.dropdown.open_context(items, right_click_pos, &mut self.tree);
                true
            }
            PanelAction::LayerHeaderRightClicked(layer_idx) => {
                let mut items = vec![
                    DropdownItem::new("Insert Video Layer"),
                    DropdownItem::new("Insert Generator Layer"),
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
    fn dropdown_to_action(ctx: DropdownContext, index: usize) -> Option<PanelAction> {
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
                Some(PanelAction::SetResolution(index))
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
                    1 => Some(PanelAction::ContextAddVideoLayer(layer)),
                    2 => Some(PanelAction::ContextAddGeneratorLayer(layer)),
                    _ => None,
                }
            }
            DropdownContext::LayerContext(layer_idx) => {
                match index {
                    0 => Some(PanelAction::ContextAddVideoLayer(layer_idx)),
                    1 => Some(PanelAction::ContextAddGeneratorLayer(layer_idx)),
                    2 => Some(PanelAction::ContextDeleteLayer(layer_idx)),
                    _ => None,
                }
            }
        }
    }

    // ── Inspector resize ──────────────────────────────────────────

    const RESIZE_EDGE_PX: f32 = 4.0;
    const INSPECTOR_MIN_W: f32 = 200.0;
    const INSPECTOR_MAX_W: f32 = 500.0;

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
    }
}
